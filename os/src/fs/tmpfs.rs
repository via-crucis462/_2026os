use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use crate::fs::ROOT_DENTRY;
use crate::mm::user_buffer;
use alloc::vec;
use crate::fs::devfs::NullInode;
use crate::fs::devfs::ZeroInode;
use crate::fs::devfs::RtcInode;
use crate::fs::devfs::TtyInode;
use super::{VfsInode, Stat, Statx};
use crate::syscall::fs::Statfs;
use crate::auth::{PermStat, FileMode};

// 全局唯一的 Inode 分配器
static TMPFS_INO_COUNTER: AtomicUsize = AtomicUsize::new(10000);

/// 临时文件inode
pub struct TmpfsFileInode {
    ino: usize,
    data: Mutex<alloc::vec::Vec<u8>>,
    perms: Mutex<PermStat>, // 权限信息
}

impl TmpfsFileInode {
    pub fn new() -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            data: Mutex::new(alloc::vec::Vec::new()),
            perms: Mutex::new(PermStat::new(FileMode::from_bits_truncate(0o100777), 0, 0)), // 默认权限
        }
    }
    pub fn new_with_data(data: &[u8]) -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            data: Mutex::new(data.to_vec()), // 直接把传进来的切片转成 Vec 存起来
        }
    }
}

impl super::VfsInode for TmpfsFileInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let data = self.data.lock();
        if offset >= data.len() {
            return 0; // 读到文件尾
        }
        let read_len = core::cmp::min(buf.len(), data.len() - offset);
        buf[..read_len].copy_from_slice(&data[offset..offset + read_len]);
        read_len
    }

    // 真正的内存文件写（支持自动扩容）
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut data = self.data.lock();
        let end_offset = offset + buf.len();
        if end_offset > data.len() {
            // 如果写超出了当前文件大小，自动扩展 Vec，并用 0 填充空隙
            data.resize(end_offset, 0); 
        }
        data[offset..end_offset].copy_from_slice(buf);
        buf.len()
    }
    fn get_size(&self) -> usize { self.data.lock().len() }

    fn get_stat(&self) -> super::Stat {
        let data_len = self.data.lock().len();
        let perms = self.perms.lock();
        let (mode, uid, gid) = (perms.mode.bits(), perms.uid, perms.gid);
        super::Stat {
            dev: 0, 
            ino: self.ino as u64,
            mode: mode, nlink: 1, 
            uid: uid, gid: gid, rdev: 0, __pad: 0, 

            size: self.get_size() as i64, 
            blksize: 512, __pad2: 0,
            blocks: ((self.get_size() as i64) + 511) / 512, 
            atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
    fn get_statx(&self) -> super::Statx {
        let stat = self.get_stat();
        Statx{
            stx_mask: 0,
            stx_blksize: stat.blksize as u32,
            stx_attributes: 0,
            stx_nlink: stat.nlink,
            stx_uid: stat.uid,
            stx_gid: stat.gid,
            stx_mode: stat.mode as u16,
            __spare0: [0; 1],
            stx_ino: stat.ino,
            stx_size: stat.size as u64,
            stx_blocks: stat.blocks as u64,
            stx_attributes_mask: 0,
            stx_atime: super::StatxTimestamp {
                tv_sec: stat.atime_sec,
                tv_nsec: stat.atime_nsec as u32,
                __reserved: 0,
            },
            stx_btime: super::StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
            stx_ctime: super::StatxTimestamp {
                tv_sec: stat.ctime_sec,
                tv_nsec: stat.ctime_nsec as u32,
                __reserved: 0,
            },
            stx_mtime: super::StatxTimestamp {
                tv_sec: stat.mtime_sec,
                tv_nsec: stat.mtime_nsec as u32,
                __reserved: 0,
            },
            stx_rdev_major: 0, 
            stx_rdev_minor: 0, 
            stx_dev_major: 0, 
            stx_dev_minor: 0, 
            __spare2: [0; 14],
        }
    }
    fn get_perm(&self) -> PermStat {
        self.perms.lock().clone()
    }
    fn set_perm(&self, perm: PermStat) -> bool {
        *self.perms.lock() = perm;
        true
    }
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
    perms: Mutex<PermStat>, // 权限信息
}

impl TmpfsDirInode {
    pub fn new() -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            entries: Mutex::new(BTreeMap::new()),
            perms: Mutex::new(PermStat::new(FileMode::from_bits_truncate(0o040777), 0, 0)),
        }
    }
    pub fn insert(&self, name: String, inode: Arc<dyn VfsInode>) -> Arc<dyn VfsInode> {
        self.entries.lock().insert(name, inode.clone());
        inode
    }
}

impl super::VfsInode for TmpfsDirInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0, 
            ino: self.ino as u64, 
            mode: 0o040777, nlink: 2,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
    fn get_statx(&self) -> super::Statx { 
        let stat = self.get_stat();
        Statx{
            stx_mask: 0,
            stx_blksize: stat.blksize as u32,
            stx_attributes: 0,
            stx_nlink: stat.nlink,
            stx_uid: stat.uid,
            stx_gid: stat.gid,
            stx_mode: stat.mode as u16,
            __spare0: [0; 1],
            stx_ino: stat.ino,
            stx_size: stat.size as u64,
            stx_blocks: stat.blocks as u64,
            stx_attributes_mask: 0,
            stx_atime: super::StatxTimestamp {
                tv_sec: stat.atime_sec,
                tv_nsec: stat.atime_nsec as u32,
                __reserved: 0,
            },
            stx_btime: super::StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
            stx_ctime: super::StatxTimestamp {
                tv_sec: stat.ctime_sec,
                tv_nsec: stat.ctime_nsec as u32,
                __reserved: 0,
            },
            stx_mtime: super::StatxTimestamp {
                tv_sec: stat.mtime_sec,
                tv_nsec: stat.mtime_nsec as u32,
                __reserved: 0,
            },
            stx_rdev_major: 0, 
            stx_rdev_minor: 0, 
            stx_dev_major: 0, 
            stx_dev_minor: 0, 
            __spare2: [0; 14],
        }
    }
    fn get_perm(&self) -> PermStat {
        self.perms.lock().clone()
    }
    fn set_perm(&self, perm: PermStat) -> bool {
        *self.perms.lock() = perm;
        true
    }
    fn find(&self, name: &str) -> Option<Arc<dyn super::VfsInode>> {
        self.entries.lock().get(name).cloned()
    }

    fn create_file(&self, name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        let new_file: Arc<dyn super::VfsInode> = Arc::new(TmpfsFileInode::new());
        self.entries.lock().insert(name.to_string(), new_file.clone());
        Some(new_file)
    }

    fn create_dir(&self, name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        let new_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());
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
}

pub fn setup_oscomp_env() {
    info!("[VFS] INFO: Start setup_oscomp_env...");
    let root = ROOT_DENTRY.clone();

    // 1. 挂载 /tmp 
    root.insert("tmp".to_string(), Arc::new(TmpfsDirInode::new()));
    info!("[VFS] Mounted /tmp");

    // 2. 挂载 bin, sbin, usr 等虚拟目录
    let etc_dentry = root.insert("etc".to_string(), Arc::new(TmpfsDirInode::new()));
    let passwd_content = "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/bin/false\n";
    let group_content = "root:x:0:\nnobody:x:65534:\n";
    
    etc_dentry.insert("passwd".to_string(), Arc::new(TmpfsFileInode::new_with_data(passwd_content.as_bytes())));
    etc_dentry.insert("group".to_string(), Arc::new(TmpfsFileInode::new_with_data(group_content.as_bytes())));

    let var_dentry = root.insert("var".to_string(), Arc::new(TmpfsDirInode::new()));
    var_dentry.insert("tmp".to_string(), Arc::new(TmpfsDirInode::new()));
    var_dentry.insert("run".to_string(), Arc::new(TmpfsDirInode::new()));
    let bin_dentry = root.insert("bin".to_string(), Arc::new(TmpfsDirInode::new()));
    let sbin_dentry = root.insert("sbin".to_string(), Arc::new(TmpfsDirInode::new()));
    let usr_dentry = root.insert("usr".to_string(), Arc::new(TmpfsDirInode::new()));
    let usr_local_dentry = usr_dentry.insert("local".to_string(), Arc::new(TmpfsDirInode::new()));
    let usr_local_bin_dentry = usr_local_dentry.insert("bin".to_string(), Arc::new(TmpfsDirInode::new()));
    let usr_bin_dentry = usr_dentry.insert("bin".to_string(), Arc::new(TmpfsDirInode::new()));
    let lib_dentry = root.insert("lib".to_string(), Arc::new(TmpfsDirInode::new()));
    let lib64_dentry = root.insert("lib64".to_string(), Arc::new(TmpfsDirInode::new()));

    // 3. 将 Busybox 和 libc 的真实 Inode 映射进虚拟目录
    if let Some(musl_dir) = root.find_tree("/musl", true) {
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
        let dev_dentry = if let Some(dev) = root.find_tree("/dev", true) {
            dev
        } else {
            // 理论上不会走到这，因为你在 mount_devfs 已经建了
            root.insert("dev".to_string(), Arc::new(TmpfsDirInode::new()))
        };

        // 安全地把 shm 塞进现有的 /dev 里
        dev_dentry.insert("shm".to_string(), Arc::new(TmpfsDirInode::new()));
        dev_dentry.insert("null".to_string(), Arc::new(NullInode::new())); 
        dev_dentry.insert("zero".to_string(), Arc::new(ZeroInode::new()));
        dev_dentry.insert("rtc".to_string(), Arc::new(RtcInode::new()));
        dev_dentry.insert("tty".to_string(), Arc::new(TtyInode::new()));
        // 2. 挂载 shm
 
        info!("[VFS] Mounted /dev/shm safely");
        if root.find_tree("/dev/shm", true).is_some() {
        info!("DEBUG: /dev/shm path is VALID");
        } else {
            error!("DEBUG: /dev/shm path is BROKEN!");
        }
        // --- 挂载动态链接库 ---
        // musl
        if let Some(libc_node) = root.find_tree("/musl/libc.so", true).or_else(|| root.find_tree("/musl/lib/libc.so", true)) {
            #[cfg(target_arch = "riscv64")]
            lib_dentry.insert("ld-musl-riscv64.so.1".to_string(), libc_node.inode.clone());
            
            #[cfg(target_arch = "loongarch64")]
            lib_dentry.insert("ld-musl-loongarch-lp64d.so.1".to_string(), libc_node.inode.clone());
            
            lib_dentry.insert("libc.so".to_string(), libc_node.inode.clone());
            info!("[VFS] Populated libc.so symlinks");
        }
        
    } else {
        warn!("[VFS] WARNING: /musl not found, skipped busybox mapping.");
    }
    // mount glibc ld
    if let Some(glibc_dir) = root.find_tree("/glibc", true) {
        #[cfg(target_arch = "loongarch64")]
        {
            if let Some(libc_node) = root.find_tree("/glibc/lib/ld-linux-loongarch-lp64d.so.1", true) {
                lib_dentry.insert("ld-linux-loongarch-lp64d.so.1".to_string(), libc_node.inode.clone());
            }
            if let Some(libc_node) = root.find_tree("/glibc/lib/libc.so.6", true) {
                lib_dentry.insert("libc.so.6".to_string(), libc_node.inode.clone());
            }
            if let Some(libc_node) = root.find_tree("/glibc/lib/libm.so.6", true) {
                lib_dentry.insert("libm.so.6".to_string(), libc_node.inode.clone());
            }
        }
        #[cfg(target_arch = "riscv64")]
        {
            if let Some(libc_node) = root.find_tree("/glibc/lib/ld-linux-riscv64-lp64d.so.1", true) {
                lib_dentry.insert("ld-linux-riscv64-lp64d.so.1".to_string(), libc_node.inode.clone());
            }
            if let Some(libc_node) = root.find_tree("/glibc/lib/libc.so.6", true) {
                lib_dentry.insert("libc.so.6".to_string(), libc_node.inode.clone());
            }
            if let Some(libc_node) = root.find_tree("/glibc/lib/libm.so.6", true) {
                lib_dentry.insert("libm.so.6".to_string(), libc_node.inode.clone());
            }
        }
    } else {
        warn!("[VFS] WARNING: /glibc not found, skipped glibc mapping.");
    }

    if root.find_tree("/dev/shm", true).is_some() {
        info!("DEBUG: /dev/shm path is VALID");
    } else {
        error!("DEBUG: /dev/shm path is BROKEN!");
    }
}