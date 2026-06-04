use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::{Mutex, lazy};
use crate::fs::ROOT_DENTRY;
use crate::fs::Dentry;
use crate::mm::{PageSize, user_buffer};
use alloc::vec;
use crate::fs::devfs::NullInode;
use crate::fs::devfs::ZeroInode;
use crate::fs::devfs::RtcInode;
use crate::fs::devfs::TtyInode;
use super::{VfsInode, Stat, Statx};
use crate::syscall::fs::Statfs;
use crate::auth::{PermStat, FileMode};
use crate::drivers::loopdev::*;
use crate::mm::{FrameTracker, PhysPageNum };
use crate::mm::frame_alloc;
use crate::mm::PageSize::Page4K;

use crate::PAGE_SIZE;
// 全局唯一的 Inode 分配器
pub static TMPFS_INO_COUNTER: AtomicUsize = AtomicUsize::new(10000);

use lazy_static::lazy_static;
/// 大页目录
lazy_static! {
    pub static ref HUGEPAGES_DENTRY: Arc<super::Dentry> = mount_hugepages();
}

/// 临时文件inode
pub struct TmpfsFileInode {
    ino: usize,
    pages: Mutex<BTreeMap<usize, FrameTracker>>,
    size: Mutex<usize>,
    stat: Mutex<Stat>,
}

impl TmpfsFileInode {
    pub fn new(mode: u32) -> Self {
        let file_type = mode & 0o170000;
        let full_mode = if file_type == 0 {
            0o100000 | (mode & 0o7777)
        } else {
            file_type | (mode & 0o7777)
        };
        let mut stat = Stat::default();
        stat.mode = full_mode;
        stat.nlink = 1;
        stat.blksize = 4096;
        stat.ino = TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst) as u64;
        Self {
            ino: stat.ino as usize,
            pages: Mutex::new(BTreeMap::new()),
            size: Mutex::new(0),
            stat: Mutex::new(stat),
        }
    }

    pub fn new_with_data(data: &[u8]) -> Self {
        let inode = Self::new(0o777);
        inode.write_at(0, data); 
        inode
    }
}

impl super::VfsInode for TmpfsFileInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let size = *self.size.lock();
        if offset >= size {
            return 0; 
        }
        let read_len = core::cmp::min(buf.len(), size - offset);
        
        let pages = self.pages.lock();
        let mut current_offset = offset;
        let mut buf_idx = 0;
        
        while buf_idx < read_len {
            let page_idx = current_offset / PAGE_SIZE;
            let page_inner_offset = current_offset % PAGE_SIZE;
            let bytes_to_read = core::cmp::min(read_len - buf_idx, PAGE_SIZE - page_inner_offset);
            
            if let Some(frame) = pages.get(&page_idx) {
                let src = &frame.ppn.get_bytes_array()[page_inner_offset..page_inner_offset + bytes_to_read];
                buf[buf_idx..buf_idx + bytes_to_read].copy_from_slice(src);
            } else {
                // 稀疏文件未分配页则填 0
                buf[buf_idx..buf_idx + bytes_to_read].fill(0);
            }
            
            current_offset += bytes_to_read;
            buf_idx += bytes_to_read;
        }
        read_len
    }

    // 2. 适配页缓存的文件写入（支持动态自动扩容分配物理页）
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut size = self.size.lock();
        let mut pages = self.pages.lock();
        let end_offset = offset + buf.len();
        
        let mut current_offset = offset;
        let mut buf_idx = 0;
        
        while buf_idx < buf.len() {
            let page_idx = current_offset / PAGE_SIZE;
            let page_inner_offset = current_offset % PAGE_SIZE;
            let bytes_to_write = core::cmp::min(buf.len() - buf_idx, PAGE_SIZE - page_inner_offset);
            
            // 如果这一页还没创建，直接调用内核页分配器占领一个物理页
            let frame = pages.entry(page_idx).or_insert_with(|| {
                crate::mm::frame_alloc(Page4K).expect("[Tmpfs] Failed to allocate physical page frame")
            });
            
            let dest = &mut frame.ppn.get_bytes_array()[page_inner_offset..page_inner_offset + bytes_to_write];
            dest.copy_from_slice(&buf[buf_idx..buf_idx + bytes_to_write]);
            
            current_offset += bytes_to_write;
            buf_idx += bytes_to_write;
        }
        
        if end_offset > *size {
            *size = end_offset; // 自动扩容
        }
        buf.len()
    }

    fn get_size(&self) -> usize {
        *self.size.lock()
    }
    fn get_shared_page(&self, page_offset: usize) -> Option<PhysPageNum> {
        let mut frames = self.pages.lock();
        // 如果 mmap 映射的页超出了当前文件大小，Linux 允许直接分配空白页给它
        let frame = frames.entry(page_offset).or_insert_with(|| {
            let f = frame_alloc(Page4K).unwrap();
            let page_kvaddr = f.ppn.0 << 12;
            unsafe { core::slice::from_raw_parts_mut(page_kvaddr as *mut u8, PAGE_SIZE).fill(0); }
            f
        });
        Some(frame.ppn)
    }
    fn get_stat(&self) -> super::Stat {
        let file_size = self.get_size() as i64;
        let mut stat = *self.stat.lock();
        stat.size = file_size;
        stat.blocks = (file_size + 511) / 512;
        stat.ino = self.ino as u64;
        stat
    }
    
    fn get_statx(&self) -> super::Statx {
        super::stat_to_statx(self.get_stat())
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.stat.lock();
        PermStat {
            mode: FileMode::from_bits_truncate(stat.mode as u16),
            uid: stat.uid,
            gid: stat.gid,
        }
    }
    fn set_perm(&self, perm: PermStat) -> bool {
        let mut stat = self.stat.lock();
        stat.mode = perm.mode.bits() as u32;
        stat.uid = perm.uid;
        stat.gid = perm.gid;
        true
    }
    fn set_time(&self, atime: &super::TimeSpec, mtime: &super::TimeSpec) -> isize {
        println!("VFS: set_time called on TmpfsFileInode, atime=({}, {}), mtime=({}, {})", 
            atime.tv_sec, atime.tv_nsec, mtime.tv_sec, mtime.tv_nsec);
        let mut stat = self.stat.lock();
        stat.atime_sec = atime.tv_sec as i64;
        stat.atime_nsec = atime.tv_nsec as i64;
        stat.mtime_sec = mtime.tv_sec as i64;
        stat.mtime_nsec = mtime.tv_nsec as i64;
        0
    }
    fn type_name(&self) -> &'static str { "TmpfsFileInode" }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

/// 临时目录inode
pub struct TmpfsDirInode {
    ino: usize,
    entries: Mutex<BTreeMap<String, Arc<dyn super::VfsInode>>>,
    stat: Mutex<Stat>,
}

impl TmpfsDirInode {
    pub fn new(mode: u32) -> Self {
        let full_mode = 0o040000 | (mode & 0o7777); // S_IFDIR
        let mut stat = Stat::default();
        stat.mode = full_mode;
        stat.nlink = 2;
        stat.blksize = 512;
        stat.ino = TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst) as u64;
        Self {
            ino: stat.ino as usize,
            entries: Mutex::new(BTreeMap::new()),
            stat: Mutex::new(stat),
        }
    }
    pub fn insert(&self, name: String, inode: Arc<dyn VfsInode>) -> Arc<dyn VfsInode> {
        self.entries.lock().insert(name, inode.clone());
        inode
    }

    // 返回当前目录项快照，避免调用方在目录枚举期间长期持有 entries 锁。
    pub fn entries_snapshot(&self) -> alloc::vec::Vec<(String, Arc<dyn VfsInode>)> {
        self.entries
            .lock()
            .iter()
            .map(|(name, inode)| (name.clone(), inode.clone()))
            .collect()
    }
}

impl super::VfsInode for TmpfsDirInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    
    fn get_stat(&self) -> super::Stat {
        let mut stat = *self.stat.lock();
        stat.ino = self.ino as u64;
        stat
    }
    
    fn get_statx(&self) -> super::Statx { 
        super::stat_to_statx(self.get_stat())
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.stat.lock();
        PermStat {
            mode: FileMode::from_bits_truncate(stat.mode as u16),
            uid: stat.uid,
            gid: stat.gid,
        }
    }
    fn set_perm(&self, perm: PermStat) -> bool {
        let mut stat = self.stat.lock();
        stat.mode = perm.mode.bits() as u32;
        stat.uid = perm.uid;
        stat.gid = perm.gid;
        true
    }
    fn find(&self, name: &str) -> Option<Arc<dyn super::VfsInode>> {
        self.entries.lock().get(name).cloned()
    }

    fn create_file(&self, name: &str, mode: u32) -> Option<Arc<dyn super::VfsInode>> {
       let new_file: Arc<dyn super::VfsInode> = Arc::new(TmpfsFileInode::new(mode));
        self.entries.lock().insert(name.to_string(), new_file.clone());
        Some(new_file)
    }

    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        let new_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new(mode));
        self.entries.lock().insert(name.to_string(), new_dir.clone());
        Some(new_dir)
    }

    fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        let mut entries = self.entries.lock();
        if entries.remove(name).is_some() {
            Some(0) 
        } else {
            None 
        }
    }

    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { 0 }
    fn statfs(&self) -> Statfs {
        Statfs {
            f_type: 0x01021994, // Tmpfs 的魔数
            f_bsize: 4096,
            f_blocks: 0, // 内存文件系统，块数为 0 即可
            f_bfree: 0, f_bavail: 0, f_files: 0, f_ffree: 0,
            f_fsid: [0, 0], f_namelen: 255, f_frsize: 4096,
            f_flags: 0, f_spare: [0; 4],
        }
    }
    fn set_time(&self, _atime: &super::TimeSpec, _mtime: &super::TimeSpec) -> isize {
        println!("VFS: set_time called on TmpfsDirInode, atime=({}, {}), mtime=({}, {})", 
            _atime.tv_sec, _atime.tv_nsec, _mtime.tv_sec, _mtime.tv_nsec);
        let mut stat = self.stat.lock();
        stat.atime_sec = _atime.tv_sec as i64;
        stat.atime_nsec = _atime.tv_nsec as i64;
        stat.mtime_sec = _mtime.tv_sec as i64;
        stat.mtime_nsec = _mtime.tv_nsec as i64;
        0
    }
    fn type_name(&self) -> &'static str { "TmpfsDirInode" }
}

/// 通过 getdents 枚举目录项，将源目录下所有条目的 inode 映射到目标 lib/lib64
fn populate_lib_from_dentries(
    src: &Arc<Dentry>,
    lib: &Arc<Dentry>,
    lib64: &Arc<Dentry>,
) {
    let mut buf = vec![0u8; 4096];
    let mut offset: usize = 0;
    // 循环调用getdents枚举目录项
    loop {
        let n = src.inode.getdents(&mut offset, &mut buf);
        if n <= 0 {
            break;
        }
        let data = &buf[..n as usize];
        let mut pos = 0;
        while pos + 19 <= data.len() {
            // 结构体解析
            let d_ino = u64::from_ne_bytes(data[pos..pos + 8].try_into().unwrap());
            let d_reclen =
                u16::from_ne_bytes(data[pos + 16..pos + 18].try_into().unwrap()) as usize;
            if d_reclen == 0 || pos + d_reclen > data.len() {
                break;
            }
            // 处理目录项，跳过 "." 和 ".."
            if d_ino != 0 {
                let name_start = pos + 19;
                let name_max = pos + d_reclen;
                let name_len = data[name_start..name_max]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(name_max - name_start);
                if let Ok(name) = core::str::from_utf8(&data[name_start..name_start + name_len]) {
                    if name != "." && name != ".." {
                        // 用 find_tree 跟随符号链接，拿到真实文件 inode
                        if let Ok(child) = src.find_tree(name, true) {
                            println!("[VFS] Mounted lib entry: {}", name);
                            lib.mount_child(name.to_string(), child.inode.clone());
                            lib64.mount_child(name.to_string(), child.inode.clone());
                        }
                    }
                }
            }
            pos += d_reclen;
        }
    }
}

pub fn setup_oscomp_env() {
    info!("[VFS] INFO: Start setup_oscomp_env...");
    let root = ROOT_DENTRY.clone();

    // 1. 挂载 /tmp 
    root.mount_child("tmp".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    info!("[VFS] Mounted /tmp");

    // 2. 挂载 bin, sbin, usr 等虚拟目录
    let etc_dentry = root.mount_child("etc".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let passwd_content = "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/bin/false\n";
    let group_content = "root:x:0:\nnobody:x:65534:\n";
    
    etc_dentry.insert("passwd".to_string(), Arc::new(TmpfsFileInode::new_with_data(passwd_content.as_bytes())));
    etc_dentry.insert("group".to_string(), Arc::new(TmpfsFileInode::new_with_data(group_content.as_bytes())));

    let var_dentry = root.mount_child("var".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    var_dentry.insert("tmp".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    var_dentry.insert("run".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let bin_dentry = root.mount_child("bin".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let sbin_dentry = root.mount_child("sbin".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let usr_dentry = root.mount_child("usr".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let usr_local_dentry = usr_dentry.insert("local".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let usr_local_bin_dentry = usr_local_dentry.insert("bin".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let usr_bin_dentry = usr_dentry.insert("bin".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let lib_dentry = root.mount_child("lib".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let lib64_dentry = root.mount_child("lib64".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    
    // loop测例检查的文件
    let lib_modules = lib_dentry.insert("modules".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let lib_modules_rcore = lib_modules.insert("5.10.0-rcore".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    lib_modules_rcore.insert("modules.builtin".to_string(), Arc::new(TmpfsFileInode::new_with_data(b"kernel/drivers/block/loop.ko\n")));
    lib_modules_rcore.insert("modules.dep".to_string(), Arc::new(TmpfsFileInode::new_with_data(b"")));
    
    // loop测例检查的文件
    let sys_dentry = root.mount_child("sys".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let sys_module_dentry = sys_dentry.insert("module".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    sys_module_dentry.insert("loop".to_string(), Arc::new(TmpfsDirInode::new(0o777)));

    // 3. 将 Busybox 和 libc 的真实 Inode 映射进虚拟目录
    if let Ok(musl_dir) = root.find_tree("/musl", true) {
        if let Some(busybox_node) = musl_dir.find_child("busybox") {
            let bb_inode = busybox_node.inode.clone();
            let applets = [
                "basename", "dirname", "sh", "grep", "sed", "awk", "cat", 
                "ls", "rm", "echo", "true", "false", "wc", "mkdir", "rmdir", "touch", "env"
            ];
            
            for app in applets {
                bin_dentry.insert(app.to_string(), bb_inode.clone());
                sbin_dentry.insert(app.to_string(), bb_inode.clone());
                usr_bin_dentry.insert(app.to_string(), bb_inode.clone());
                usr_local_bin_dentry.insert(app.to_string(), bb_inode.clone());
            }
            info!("[VFS] Populated busybox applets");
        }
        let dev_dentry = if let Ok(dev) = root.find_tree("/dev", true) {
            dev
        } else {
            // 理论上不会走到这
            root.mount_child("dev".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
        };

        // 挂载shm到/dev/shm
        dev_dentry.insert("shm".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
        // 挂载常用设备文件
        dev_dentry.insert("null".to_string(), Arc::new(NullInode::new())); 
        dev_dentry.insert("zero".to_string(), Arc::new(ZeroInode::new()));
        dev_dentry.insert("rtc".to_string(), Arc::new(RtcInode::new()));
        // 终端设备
        dev_dentry.insert("tty".to_string(), Arc::new(TtyInode::new()));

        // loop-control
        dev_dentry.insert("loop-control".to_string(), Arc::new(LoopControlInode::new()));

        // 挂载8个loop设备
        for i in 0..8 {
            let loop_name = alloc::format!("loop{}", i);
            let loop_device = create_loop_device(None, 0, 0);
            dev_dentry.insert(loop_name, loop_device);
        }

        info!("[VFS] Mounted /dev/shm safely");
        if let Ok(_) = root.find_tree("/dev/shm", true) {
            info!("DEBUG: /dev/shm path is VALID");
        } else {
            error!("DEBUG: /dev/shm path is BROKEN!");
        }        
    } else {
        warn!("[VFS] WARNING: /musl not found, skipped busybox mapping.");
    }

    // --- 挂载 动态链接库 & 加载器 ---
    // musl
    let libc_node = root.find_tree("/musl/lib", true).unwrap();
    populate_lib_from_dentries(&libc_node, &lib_dentry, &lib64_dentry);
    let ld = libc_node.find_child("libc.so").unwrap();
    #[cfg(target_arch = "loongarch64")]
    {
        lib64_dentry.mount_child("ld-musl-loongarch-lp64d.so.1".to_string(), ld.inode.clone());
        lib_dentry.mount_child("ld-musl-loongarch-lp64d.so.1".to_string(), ld.inode.clone());
    }
    #[cfg(target_arch = "riscv64")]
    {
        lib64_dentry.mount_child("ld-musl-riscv64.so.1".to_string(), ld.inode.clone());
        lib_dentry.mount_child("ld-musl-riscv64.so.1".to_string(), ld.inode.clone());
        lib64_dentry.mount_child("ld-musl-riscv64-sf.so.1".to_string(), ld.inode.clone());
        lib_dentry.mount_child("ld-musl-riscv64-sf.so.1".to_string(), ld.inode.clone());
    }
    info!("[VFS] Populated musl lib symlinks");

    // glibc
    let libc_node = root.find_tree("/glibc/lib", true).unwrap();
    let user_lib64_dentry = usr_dentry.mount_child("lib64".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    let user_lib_dentry = usr_dentry.mount_child("lib".to_string(), Arc::new(TmpfsDirInode::new(0o777)));
    // 同时挂载到 /lib* 和 /usr/lib*
    populate_lib_from_dentries(&libc_node, &user_lib_dentry, &user_lib64_dentry);
    populate_lib_from_dentries(&libc_node, &lib_dentry, &lib64_dentry);
    info!("[VFS] Populated glibc lib symlinks");


    if let Ok(_) = root.find_tree("/dev/shm", true) {
        info!("DEBUG: /dev/shm path is VALID");
    } else {
        error!("DEBUG: /dev/shm path is BROKEN!");
    }
    mount_hugepages();
    info!("[VFS] setup_oscomp_env done.");
}

// 挂载 /sys/kernel/mm/hugepages
fn mount_hugepages() -> Arc<super::Dentry> {
    let root = ROOT_DENTRY.clone();
    let sys_dentry = if let Ok(sys) = root.find_tree("/sys", true) {
        sys
    } else {
        root.mount_child("sys".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    let kernel_dentry = if let Ok(kernel) = sys_dentry.find_tree("/sys/kernel", true) {
        kernel
    } else {
        sys_dentry.insert("kernel".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    let mm_dentry = if let Ok(mm) = kernel_dentry.find_tree("/sys/kernel/mm", true) {
        mm
    } else {
        kernel_dentry.insert("mm".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    let hugepages_dentry = if let Ok(hugepages) = mm_dentry.find_tree("/sys/kernel/mm/hugepages", true) {
        hugepages
    } else {
        mm_dentry.insert("hugepages".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    info!("[VFS] Mounted /sys/kernel/mm/hugepages");
    hugepages_dentry
}