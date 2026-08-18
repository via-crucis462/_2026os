use super::{Stat, Statx, VfsInode};
use crate::auth::{FileMode, PermStat};
use crate::drivers::loopdev::*;
use crate::fs::devfs::NullInode;
use crate::fs::devfs::RtcInode;
use crate::fs::devfs::TtyInode;
use crate::fs::devfs::UrandomInode;
use crate::fs::devfs::ZeroInode;
use crate::fs::ino::get_next_ino;
use crate::fs::Dentry;
use crate::fs::ROOT_DENTRY;
use crate::mm::frame_alloc;
use crate::mm::PageSize::Page4K;
use crate::mm::{user_buffer, PageSize};
use crate::mm::{FrameTracker, PhysPageNum};
use crate::syscall::errno::Errno;
use crate::syscall::fs::Statfs;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use spin::{lazy, Mutex};

use crate::PAGE_SIZE;

use lazy_static::lazy_static;
/// 大页目录
lazy_static! {
    pub static ref HUGEPAGES_DENTRY: Arc<super::Dentry> = mount_hugepages();
}

/// 临时文件inode
pub struct TmpfsFileInode {
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
        stat.ino = get_next_ino();
        Self {
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
    fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
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
                let src = &frame.ppn.get_bytes_array()
                    [page_inner_offset..page_inner_offset + bytes_to_read];
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
    fn raw_write_at(&self, offset: usize, buf: &[u8]) -> usize {
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
                crate::mm::frame_alloc(Page4K)
                    .expect("[Tmpfs] Failed to allocate physical page frame")
            });

            let dest = &mut frame.ppn.get_bytes_array()
                [page_inner_offset..page_inner_offset + bytes_to_write];
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
    fn truncate(&self, len: usize) -> bool {
        let mut size = self.size.lock();
        let mut pages = self.pages.lock();
        let old_size = *size;
        // 更新大小信息
        *size = len;

        if len < old_size {
            // 收缩：释放超出部分的物理页
            let new_end_page = (len + PAGE_SIZE - 1) / PAGE_SIZE;
            pages.retain(|&page_idx, _| page_idx < new_end_page);
        }
        // 扩张：tmpfs 用惰性分配策略，跳过
        true
    }
    fn get_shared_page(&self, page_offset: usize) -> Option<Arc<crate::mm::mmap::PageCache>> {
        let mut frames = self.pages.lock();
        // 如果 mmap 映射的页超出了当前文件大小，Linux 允许直接分配空白页给它
        let frame = frames.entry(page_offset).or_insert_with(|| {
            let f = frame_alloc(Page4K).unwrap();
            f.get_bytes_array().fill(0);
            f
        });
        let page_cache = crate::mm::mmap::PageCache::from_frame(frame.clone());
        Some(Arc::new(page_cache))
    }
    fn get_file_page(&self, page_offset: usize) -> Option<Arc<crate::mm::mmap::PageCache>> {
        if let Some(frame) = self.pages.lock().get(&page_offset).cloned() {
            return Some(Arc::new(crate::mm::mmap::PageCache::from_frame(frame)));
        }
        // A sparse tmpfs hole is logically zero-filled.  Do not insert a
        // synthetic page into the file just to service an executable read.
        let frame = frame_alloc(Page4K)?;
        Some(Arc::new(crate::mm::mmap::PageCache::from_frame(frame)))
    }
    fn ino(&self) -> u64 {
        self.stat.lock().ino
    }
    fn get_stat(&self) -> super::Stat {
        let file_size = self.get_size() as i64;
        let mut stat = *self.stat.lock();
        stat.size = file_size;
        stat.blocks = (file_size + 511) / 512;
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
        //println!("VFS: set_time called on TmpfsFileInode, atime=({}, {}), mtime=({}, {})",
        //    atime.tv_sec, atime.tv_nsec, mtime.tv_sec, mtime.tv_nsec);
        let mut stat = self.stat.lock();
        stat.atime_sec = atime.tv_sec as i64;
        stat.atime_nsec = atime.tv_nsec as i64;
        stat.mtime_sec = mtime.tv_sec as i64;
        stat.mtime_nsec = mtime.tv_nsec as i64;
        0
    }
    fn type_name(&self) -> &'static str {
        "TmpfsFileInode"
    }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> {
        None
    }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize {
        -1
    }
}

/// 临时目录inode
pub struct TmpfsDirInode {
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
        stat.ino = get_next_ino();
        Self {
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

    /// Run a short operation while the directory entry map is stable.
    ///
    /// `getdents` only needs the map to remain stable while it fills one
    /// kernel buffer.  Keeping that lock avoids cloning every name and inode
    /// on each call, while retaining the existing index-based cursor model.
    pub(crate) fn with_entries<R>(
        &self,
        f: impl FnOnce(&BTreeMap<String, Arc<dyn VfsInode>>) -> R,
    ) -> R {
        let entries = self.entries.lock();
        f(&entries)
    }
}

fn tmpfs_dirent_type_from_mode(mode: u32) -> u8 {
    match mode & 0o170000 {
        0o010000 => 1,
        0o020000 => 2,
        0o040000 => 4,
        0o060000 => 6,
        0o100000 => 8,
        0o120000 => 10,
        0o140000 => 12,
        _ => 0,
    }
}

impl super::VfsInode for TmpfsDirInode {
    fn raw_read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize {
        0
    }
    fn raw_write_at(&self, _offset: usize, _buf: &[u8]) -> usize {
        0
    }
    fn get_size(&self) -> usize {
        0
    }
    fn ino(&self) -> u64 {
        self.stat.lock().ino
    }

    fn get_stat(&self) -> super::Stat {
        *self.stat.lock()
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
        if mode == 0o120777 {
            let symlink_inode = Arc::new(TmpfsFsSymbolicLinkInode {
                target: String::new(),
                stat: Mutex::new({
                    let mut s = Stat::default();
                    s.mode = 0o120777; // 符号链接
                    s.nlink = 1;
                    s.blksize = 4096;
                    s.ino = get_next_ino();
                    s
                }),
            });
            self.entries
                .lock()
                .insert(name.to_string(), symlink_inode.clone());
            return Some(symlink_inode);
        }
        let new_file: Arc<dyn super::VfsInode> = Arc::new(TmpfsFileInode::new(mode));
        self.entries
            .lock()
            .insert(name.to_string(), new_file.clone());
        Some(new_file)
    }

    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        let new_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new(mode));
        self.entries
            .lock()
            .insert(name.to_string(), new_dir.clone());
        Some(new_dir)
    }

    fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        let mut entries = self.entries.lock();
        entries.remove(name).map(|inode| inode.ino() as u32)
    }

    fn getdents(&self, offset: &mut usize, buf: &mut [u8]) -> isize {
        let entries = self.entries.lock();
        let mut buf_offset = 0usize;
        let mut next_offset = *offset;

        for (name, inode) in entries.iter().skip(next_offset) {
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len().min(255);
            let total_len = 8 + 8 + 2 + 1 + name_len + 1;
            let d_reclen = (total_len + 7) & !7;
            if buf_offset + d_reclen > buf.len() {
                break;
            }

            let stat = inode.get_stat();
            let d_off = (next_offset + 1) as i64;
            buf[buf_offset..buf_offset + 8].copy_from_slice(&stat.ino.to_ne_bytes());
            buf[buf_offset + 8..buf_offset + 16].copy_from_slice(&d_off.to_ne_bytes());
            buf[buf_offset + 16..buf_offset + 18].copy_from_slice(&(d_reclen as u16).to_ne_bytes());
            buf[buf_offset + 18] = tmpfs_dirent_type_from_mode(stat.mode);
            buf[buf_offset + 19..buf_offset + 19 + name_len]
                .copy_from_slice(&name_bytes[..name_len]);
            for byte in &mut buf[buf_offset + 19 + name_len..buf_offset + d_reclen] {
                *byte = 0;
            }

            buf_offset += d_reclen;
            next_offset += 1;
        }

        *offset = next_offset;

        if buf_offset == 0 && next_offset < entries.len() {
            Errno::EINVAL.as_isize()
        } else {
            buf_offset as isize
        }
    }
    fn statfs(&self) -> Statfs {
        Statfs {
            f_type: 0x01021994, // Tmpfs 的魔数
            f_bsize: 4096,
            f_blocks: 0, // 内存文件系统，块数为 0 即可
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_fsid: [0, 0],
            f_namelen: 255,
            f_frsize: 4096,
            f_flags: 0,
            f_spare: [0; 4],
        }
    }
    fn create_symlink(&self, name: &str, target: &str) -> Option<Arc<dyn VfsInode>> {
        let symlink_inode = Arc::new(TmpfsFsSymbolicLinkInode {
            target: target.to_string(),
            stat: Mutex::new({
                let mut s = Stat::default();
                s.mode = 0o120777; // 符号链接
                s.nlink = 1;
                s.blksize = 4096;
                s.ino = get_next_ino();
                s
            }),
        });
        self.entries
            .lock()
            .insert(name.to_string(), symlink_inode.clone());
        Some(symlink_inode)
    }
    fn set_time(&self, _atime: &super::TimeSpec, _mtime: &super::TimeSpec) -> isize {
        // println!("VFS: set_time called on TmpfsDirInode, atime=({}, {}), mtime=({}, {})",
        //     _atime.tv_sec, _atime.tv_nsec, _mtime.tv_sec, _mtime.tv_nsec);
        let mut stat = self.stat.lock();
        stat.atime_sec = _atime.tv_sec as i64;
        stat.atime_nsec = _atime.tv_nsec as i64;
        stat.mtime_sec = _mtime.tv_sec as i64;
        stat.mtime_nsec = _mtime.tv_nsec as i64;
        0
    }
    fn type_name(&self) -> &'static str {
        "TmpfsDirInode"
    }
}

/// 通过 getdents 枚举目录项，将源目录下所有条目的 inode 映射到目标 lib/lib64
fn populate_lib_from_dentries(src: &Arc<Dentry>, lib: &Arc<Dentry>, lib64: &Arc<Dentry>) {
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
                        // 用 find_tree 跟随符号链接，拿到真实文件 inode。
                        // 兼容库只能补缺，不能覆盖根文件系统已有的同名库；
                        // 尤其不能把另一版本的 ld.so 与 libc.so.6 混用。
                        if let Ok(child) = src.find_tree(name, true) {
                            if lib.find_child(name).is_none() {
                                trace!("[VFS] Mounted missing lib entry: {}", name);
                                lib.mount_child(name.to_string(), child.inode.clone());
                            }
                            if lib64.find_child(name).is_none() {
                                trace!("[VFS] Mounted missing lib64 entry: {}", name);
                                lib64.mount_child(name.to_string(), child.inode.clone());
                            }
                        }
                    }
                }
            }
            pos += d_reclen;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RootfsProfile {
    FinalDebian,
    PreliminaryCompat,
    Unknown,
}

fn rootfs_profile(root: &Arc<Dentry>) -> RootfsProfile {
    // The final image is Debian usr-merge and must retain its own `/bin`,
    // `/etc`, and library directories. Prefer it when both layouts coexist.
    if root.find_tree("/usr/bin/bash", true).is_ok()
        && root
            .find_tree("/usr/lib/loongarch64-linux-gnu", true)
            .is_ok()
    {
        RootfsProfile::FinalDebian
    } else if root.find_tree("/musl/busybox", true).is_ok()
        && root.find_tree("/musl/lib", true).is_ok()
    {
        RootfsProfile::PreliminaryCompat
    } else {
        RootfsProfile::Unknown
    }
}

/// Return an existing child, mounting an empty directory only when absent.
///
/// `find_child` must precede `mount_child`: mounts take precedence over disk
/// entries and would otherwise hide files from a complete root filesystem.
fn existing_or_mount_dir(parent: &Arc<Dentry>, name: &str, mode: u32) -> Arc<Dentry> {
    parent
        .find_child(name)
        .unwrap_or_else(|| parent.mount_child(name.to_string(), Arc::new(TmpfsDirInode::new(mode))))
}

/// Mount a kernel-provided compatibility entry only if neither the image nor
/// an earlier mount already provides it.  These entries intentionally live in
/// `mounted_children`, so they overlay but never become lower filesystem
/// dentry-cache entries.
fn mount_if_missing(parent: &Arc<Dentry>, name: &str, inode: Arc<dyn VfsInode>) -> Arc<Dentry> {
    parent
        .find_child(name)
        .unwrap_or_else(|| parent.mount_child(name.to_string(), inode))
}

fn mount_if_missing_or_empty(
    parent: &Arc<Dentry>,
    name: &str,
    inode: Arc<dyn VfsInode>,
) -> Arc<Dentry> {
    if let Some(existing) = parent.find_child(name) {
        if existing.inode.get_size() != 0 {
            return existing;
        }
    }
    parent.mount_child(name.to_string(), inode)
}

fn setup_network_config(root: &Arc<Dentry>) {
    let etc_dentry = existing_or_mount_dir(root, "etc", 0o755);

    #[cfg(board = "visionfive2")]
    let resolv_conf_content = "nameserver 1.1.1.1\noptions timeout:2 attempts:2\n";
    #[cfg(not(board = "visionfive2"))]
    let resolv_conf_content = "nameserver 10.0.2.3\noptions timeout:2 attempts:2\n";
    mount_if_missing_or_empty(
        &etc_dentry,
        "resolv.conf",
        Arc::new(TmpfsFileInode::new_with_data(
            resolv_conf_content.as_bytes(),
        )),
    );
    #[cfg(board = "visionfive2")]
    let hosts_content = "127.0.0.1 localhost\n192.168.1.101 shellcore\n";
    #[cfg(not(board = "visionfive2"))]
    let hosts_content = "127.0.0.1 localhost\n10.0.2.15 shellcore\n";
    mount_if_missing_or_empty(
        &etc_dentry,
        "hosts",
        Arc::new(TmpfsFileInode::new_with_data(hosts_content.as_bytes())),
    );
}

fn setup_common_env(root: &Arc<Dentry>) {
    // Reuse rootfs standard directories. A trimmed image receives only the
    // missing writable directories as Tmpfs, so a full image keeps its shell,
    // env, configuration, and dynamic libraries visible.
    existing_or_mount_dir(root, "tmp", 0o1777);
    existing_or_mount_dir(root, "run", 0o755);
    let var = existing_or_mount_dir(root, "var", 0o755);
    existing_or_mount_dir(&var, "tmp", 0o1777);

    let dev = existing_or_mount_dir(root, "dev", 0o755);
    // Kernel-provided device nodes are mounted overlays, never entries in the
    // lower filesystem dentry cache.
    dev.mount_child("shm".to_string(), Arc::new(TmpfsDirInode::new(0o1777)));
    dev.mount_child("null".to_string(), Arc::new(NullInode::new()));
    dev.mount_child("zero".to_string(), Arc::new(ZeroInode::new()));
    dev.mount_child("rtc".to_string(), Arc::new(RtcInode::new()));
    dev.mount_child("urandom".to_string(), Arc::new(UrandomInode::new()));
    dev.mount_child("random".to_string(), Arc::new(UrandomInode::new()));
    dev.mount_child("tty".to_string(), Arc::new(TtyInode::new()));
    dev.mount_child(
        "loop-control".to_string(),
        Arc::new(LoopControlInode::new()),
    );
    for index in 0..8 {
        dev.mount_child(
            alloc::format!("loop{}", index),
            create_loop_device(None, 0, 0),
        );
    }
}

/// 挂载决赛的 glibc 环境
fn setup_final_glibc_env(root: &Arc<Dentry>) {
    #[cfg(target_arch = "loongarch64")]
    {
        // The image's normal multiarch directory remains authoritative. These
        // aliases support PT_INTERP paths used by LoongArch glibc binaries.
        let usr_lib = root.find_tree("/usr/lib", true);
        let usr_lib64 = root.find_tree("/usr/lib64", true);
        if let (Ok(usr_lib), Ok(usr_lib64)) = (usr_lib, usr_lib64) {
            if let Ok(multiarch_lib) = root.find_tree("/usr/lib/loongarch64-linux-gnu", true) {
                populate_lib_from_dentries(&multiarch_lib, &usr_lib, &usr_lib64);
            }

            let loader = [
                "/opt/qemu-la64/lib/ld-linux-loongarch-lp64d.so.1",
                "/usr/lib/loongarch64-linux-gnu/ld-linux-loongarch-lp64d.so.1",
                "/glibc/lib/ld-linux-loongarch-lp64d.so.1",
            ]
            .iter()
            .find_map(|path| root.find_tree(path, true).ok());
            if let Some(loader) = loader {
                let name = "ld-linux-loongarch-lp64d.so.1".to_string();
                if usr_lib.find_child(&name).is_none() {
                    usr_lib.mount_child(name.clone(), loader.inode.clone());
                }
                if usr_lib64.find_child(&name).is_none() {
                    usr_lib64.mount_child(name, loader.inode.clone());
                }
                info!("[VFS] Installed LoongArch glibc loader aliases");
            } else {
                warn!("[VFS] No LoongArch glibc loader was found in the final image");
            }
        } else {
            warn!("[VFS] Final image is missing /usr/lib or /usr/lib64");
        }
    }
}

/// 挂载初赛环境
fn setup_preliminary_compat_env(root: &Arc<Dentry>) {
    // Reuse rootfs standard directories. Only a trimmed preliminary image gets
    // a Tmpfs fallback, preventing compatibility mounts from hiding a final
    // image's bash, env, configuration, or dynamic libraries.
    let etc_dentry = existing_or_mount_dir(root, "etc", 0o755);
    let passwd_content =
        "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/bin/false\n";
    let group_content = "root:x:0:\nnobody:x:65534:\n";
    mount_if_missing(
        &etc_dentry,
        "passwd",
        Arc::new(TmpfsFileInode::new_with_data(passwd_content.as_bytes())),
    );
    mount_if_missing(
        &etc_dentry,
        "group",
        Arc::new(TmpfsFileInode::new_with_data(group_content.as_bytes())),
    );

    let var_dentry = existing_or_mount_dir(root, "var", 0o755);
    existing_or_mount_dir(&var_dentry, "run", 0o755);
    let bin_dentry = existing_or_mount_dir(root, "bin", 0o755);
    let sbin_dentry = existing_or_mount_dir(root, "sbin", 0o755);
    let usr_dentry = existing_or_mount_dir(root, "usr", 0o755);
    let usr_local_dentry = existing_or_mount_dir(&usr_dentry, "local", 0o755);
    let usr_local_bin_dentry = existing_or_mount_dir(&usr_local_dentry, "bin", 0o755);
    let usr_bin_dentry = existing_or_mount_dir(&usr_dentry, "bin", 0o755);
    let lib_dentry = existing_or_mount_dir(root, "lib", 0o755);
    let lib64_dentry = existing_or_mount_dir(root, "lib64", 0o755);

    // loop 测例检查的文件。仅补缺，避免覆盖镜像自己的 modules/sysfs。
    let lib_modules = existing_or_mount_dir(&lib_dentry, "modules", 0o755);
    let lib_modules_rcore = existing_or_mount_dir(&lib_modules, "5.10.0-rcore", 0o755);
    mount_if_missing(
        &lib_modules_rcore,
        "modules.builtin",
        Arc::new(TmpfsFileInode::new_with_data(
            b"kernel/drivers/block/loop.ko\n",
        )),
    );
    mount_if_missing(
        &lib_modules_rcore,
        "modules.dep",
        Arc::new(TmpfsFileInode::new_with_data(b"")),
    );

    let sys_dentry = existing_or_mount_dir(root, "sys", 0o755);
    let sys_module_dentry = existing_or_mount_dir(&sys_dentry, "module", 0o755);
    existing_or_mount_dir(&sys_module_dentry, "loop", 0o755);

    // 3. 将 Busybox 和 libc 的真实 Inode 映射进虚拟目录
    if let Ok(musl_dir) = root.find_tree("/musl", true) {
        if let Some(busybox_node) = musl_dir.find_child("busybox") {
            let bb_inode = busybox_node.inode.clone();
            let applets = [
                "[", "basename", "cat", "chmod", "cp", "cut", "date", "dirname", "echo", "env",
                "false", "grep", "head", "kill", "ln", "ls", "mkdir", "mv", "printf", "pwd", "rm",
                "rmdir", "sed", "sh", "sleep", "sort", "tail", "test", "touch", "tr", "true",
                "uname", "wc", "which", "xargs", "awk", "cut", "tr", "head", "tail", "sort",
                "uniq", "tee", "sleep", "id", "uname", "which", "find", "xargs", "chmod", "chown",
                "date", "printf", "clear", "ps", "fgrep", "mktemp",
            ];

            for app in applets {
                // BusyBox 仅用于补齐缺失命令。覆盖镜像原有命令会让
                // /usr/bin/env 等程序意外变成 BusyBox applet。
                for directory in [
                    &bin_dentry,
                    &sbin_dentry,
                    &usr_bin_dentry,
                    &usr_local_bin_dentry,
                ] {
                    if directory.find_child(app).is_none() {
                        directory.mount_child(app.to_string(), bb_inode.clone());
                    }
                }
            }
            info!("[VFS] Populated busybox applets");
        }
    } else {
        warn!("[VFS] WARNING: /musl not found, skipped busybox mapping.");
    }

    // --- 挂载 动态链接库 & 加载器 ---
    if let Ok(musl_lib) = root.find_tree("/musl/lib", true) {
        populate_lib_from_dentries(&musl_lib, &lib_dentry, &lib64_dentry);
        if let Some(ld) = musl_lib.find_child("libc.so") {
            #[cfg(target_arch = "loongarch64")]
            for name in ["ld-musl-loongarch-lp64d.so.1"] {
                if lib_dentry.find_child(name).is_none() {
                    lib_dentry.mount_child(name.to_string(), ld.inode.clone());
                }
                if lib64_dentry.find_child(name).is_none() {
                    lib64_dentry.mount_child(name.to_string(), ld.inode.clone());
                }
            }
            #[cfg(target_arch = "riscv64")]
            for name in ["ld-musl-riscv64.so.1", "ld-musl-riscv64-sf.so.1"] {
                if lib_dentry.find_child(name).is_none() {
                    lib_dentry.mount_child(name.to_string(), ld.inode.clone());
                }
                if lib64_dentry.find_child(name).is_none() {
                    lib64_dentry.mount_child(name.to_string(), ld.inode.clone());
                }
            }
        }
        info!("[VFS] Populated musl lib symlinks");
    } else {
        warn!("[VFS] /musl/lib not found, skipped musl aliases");
    }

    // glibc
    let user_lib64_dentry = existing_or_mount_dir(&usr_dentry, "lib64", 0o755);
    let user_lib_dentry = existing_or_mount_dir(&usr_dentry, "lib", 0o755);
    if let Ok(glibc_lib) = root.find_tree("/glibc/lib", true) {
        // 同时挂载到 /lib* 和 /usr/lib*。
        populate_lib_from_dentries(&glibc_lib, &user_lib_dentry, &user_lib64_dentry);
        populate_lib_from_dentries(&glibc_lib, &lib_dentry, &lib64_dentry);
        info!("[VFS] Populated glibc lib symlinks");
    } else {
        warn!("[VFS] /glibc/lib not found, skipped glibc aliases");
    }
    //返回简单的“语言、国家、字符编码”的一套环境变量并挂载
    let locale_content = "#!/bin/sh\necho \"LANG=C\"\necho \"LC_ALL=C\"\n";
    mount_if_missing(
        &bin_dentry,
        "locale",
        Arc::new(TmpfsFileInode::new_with_data(locale_content.as_bytes())),
    );
    mount_if_missing(
        &sbin_dentry,
        "locale",
        Arc::new(TmpfsFileInode::new_with_data(locale_content.as_bytes())),
    );
    mount_if_missing(
        &usr_bin_dentry,
        "locale",
        Arc::new(TmpfsFileInode::new_with_data(locale_content.as_bytes())),
    );
    // rsh远程连接sh
    let fake_rsh = r#"#!/bin/sh
    if [ "$1" = "-n" ]; then
        shift 2
    elif echo "$1" | grep -E -q '^[0-9\.]+ \d*$'; then
        shift 1
    fi
    exec /musl/busybox sh -c "$*"
    "#;
    mount_if_missing(
        &bin_dentry,
        "rsh",
        Arc::new(TmpfsFileInode::new_with_data(fake_rsh.as_bytes())),
    );
    mount_if_missing(
        &sbin_dentry,
        "rsh",
        Arc::new(TmpfsFileInode::new_with_data(fake_rsh.as_bytes())),
    );
    mount_if_missing(
        &usr_bin_dentry,
        "rsh",
        Arc::new(TmpfsFileInode::new_with_data(fake_rsh.as_bytes())),
    );
    //setkey命令
    let fake_setkey = "#!/bin/sh\nexit 0\n";
    mount_if_missing(
        &bin_dentry,
        "setkey",
        Arc::new(TmpfsFileInode::new_with_data(fake_setkey.as_bytes())),
    );
    mount_if_missing(
        &sbin_dentry,
        "setkey",
        Arc::new(TmpfsFileInode::new_with_data(fake_setkey.as_bytes())),
    );
    // 伪造并转发 expr 命令给 busybox
    let fake_expr = "#!/bin/sh\nexec /musl/busybox expr \"$@\"\n";
    mount_if_missing(
        &bin_dentry,
        "expr",
        Arc::new(TmpfsFileInode::new_with_data(fake_expr.as_bytes())),
    );
    mount_if_missing(
        &usr_bin_dentry,
        "expr",
        Arc::new(TmpfsFileInode::new_with_data(fake_expr.as_bytes())),
    );

    let fake_ip = "#!/bin/sh\nexec /musl/busybox ip \"$@\"\n";
    mount_if_missing(
        &sbin_dentry,
        "ip",
        Arc::new(TmpfsFileInode::new_with_data(fake_ip.as_bytes())),
    );
    mount_if_missing(
        &bin_dentry,
        "ip",
        Arc::new(TmpfsFileInode::new_with_data(fake_ip.as_bytes())),
    );
    //处理一个绝对路径脚本
    let symlink_inode: Arc<dyn super::VfsInode> = Arc::new(TmpfsFsSymbolicLinkInode::new(
        "/musl/ltp/testcases".to_string(),
    ));
    if root.find_child("testcases").is_none() {
        root.mount_child("testcases".to_string(), symlink_inode);
    }
}

/// Set up the mounted root filesystem without assuming a particular contest
/// image layout. The public name is kept for existing boot paths.
pub fn set_up_env_final() {
    let root = ROOT_DENTRY.clone();
    let profile = rootfs_profile(&root);
    info!("[VFS] Setting up rootfs environment: {:?}", profile);
    setup_common_env(&root);
    setup_network_config(&root);

    match profile {
        RootfsProfile::FinalDebian => setup_final_glibc_env(&root),
        RootfsProfile::PreliminaryCompat => setup_preliminary_compat_env(&root),
        RootfsProfile::Unknown => {
            warn!("[VFS] Unknown rootfs layout; installed only non-destructive common environment")
        }
    }

    mount_hugepages();
    info!("[VFS] Rootfs environment ready");
}

/// Compatibility entry point used by older boot and test configurations.
pub fn setup_oscomp_env() {
    set_up_env_final();
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
        sys_dentry.mount_child("kernel".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    let mm_dentry = if let Ok(mm) = kernel_dentry.find_tree("/sys/kernel/mm", true) {
        mm
    } else {
        kernel_dentry.mount_child("mm".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
    };
    let hugepages_dentry =
        if let Ok(hugepages) = mm_dentry.find_tree("/sys/kernel/mm/hugepages", true) {
            hugepages
        } else {
            mm_dentry.mount_child("hugepages".to_string(), Arc::new(TmpfsDirInode::new(0o777)))
        };
    info!("[VFS] Mounted /sys/kernel/mm/hugepages");
    hugepages_dentry
}
pub struct TmpfsFsSymbolicLinkInode {
    target: String,
    stat: Mutex<Stat>,
}
impl VfsInode for TmpfsFsSymbolicLinkInode {
    fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let target_bytes = self.target.as_bytes();
        if offset >= target_bytes.len() {
            return 0;
        }
        let copy_len = core::cmp::min(buf.len(), target_bytes.len() - offset);
        buf[..copy_len].copy_from_slice(&target_bytes[offset..offset + copy_len]);
        copy_len
    }
    fn raw_write_at(&self, _offset: usize, _buf: &[u8]) -> usize {
        0
    }
    fn get_size(&self) -> usize {
        self.target.len()
    }
    fn get_stat(&self) -> super::Stat {
        let mut stat = *self.stat.lock();
        stat.size = self.target.len() as i64;
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
        println!(
            "VFS: set_time called on TmpfsFsSymbolicLinkInode, atime=({}, {}), mtime=({}, {})",
            atime.tv_sec, atime.tv_nsec, mtime.tv_sec, mtime.tv_nsec
        );
        let mut stat = self.stat.lock();
        stat.atime_sec = atime.tv_sec as i64;
        stat.atime_nsec = atime.tv_nsec as i64;
        stat.mtime_sec = mtime.tv_sec as i64;
        stat.mtime_nsec = mtime.tv_nsec as i64;
        0
    }
    fn type_name(&self) -> &'static str {
        "TmpfsFsSymbolicLinkInode"
    }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> {
        None
    }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> {
        None
    }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize {
        -1
    }
    fn ino(&self) -> u64 {
        self.stat.lock().ino
    }
}
impl TmpfsFsSymbolicLinkInode {
    fn new(target: String) -> Self {
        let mut stat = Stat::default();
        stat.mode = 0o120777; // S_IFLNK
        stat.nlink = 1;
        stat.blksize = 4096;
        stat.ino = get_next_ino();
        Self {
            target,
            stat: Mutex::new(stat),
        }
    }
}
