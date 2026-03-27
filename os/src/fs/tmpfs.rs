use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use crate::fs::ROOT_DENTRY;
use alloc::vec;
// 全局唯一的 Inode 分配器
static TMPFS_INO_COUNTER: AtomicUsize = AtomicUsize::new(10000);

// ==========================================
// 严谨的内存文件
// ==========================================
pub struct TmpfsFileInode {
    ino: usize,
    data: Mutex<alloc::vec::Vec<u8>>,
}

impl TmpfsFileInode {
    pub fn new() -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            data: Mutex::new(alloc::vec::Vec::new()),
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
        super::Stat {
            dev: 0, 
            ino: self.ino as u64,
            mode: 0o100777, nlink: 1, 
            uid: 0, gid: 0, rdev: 0, __pad: 0, 
            // 🚩 修复点 1：把 u64 改成 i64 迎合你们的 Stat 结构体
            size: self.get_size() as i64, 
            blksize: 512, __pad2: 0,
            blocks: ((self.get_size() as i64) + 511) / 512, 
            atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
    fn get_statx(&self) -> super::Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}


pub struct TmpfsDirInode {
    ino: usize,
    entries: Mutex<BTreeMap<String, Arc<dyn super::VfsInode>>>,
}

impl TmpfsDirInode {
    pub fn new() -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            entries: Mutex::new(BTreeMap::new()),
        }
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
    fn get_statx(&self) -> super::Statx { unimplemented!() }
}

pub fn setup_oscomp_env() {
    info!("VFS: Building virtual FHS directories using Tmpfs...");
    let root = ROOT_DENTRY.clone();

    // 1. 创建基于 Tmpfs 的虚拟目录 Inode
    let bin_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());
    let sbin_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());
    let usr_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());
    let usr_bin_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());
    let lib_dir: Arc<dyn super::VfsInode> = Arc::new(TmpfsDirInode::new());

    // 2. 将这些虚拟目录挂载到全局 Dentry 树中
    // 这让它们在 VFS 层面完全“真实存在”
    let bin_dentry = root.insert("bin".to_string(), bin_dir);
    let sbin_dentry = root.insert("sbin".to_string(), sbin_dir);
    let usr_dentry = root.insert("usr".to_string(), usr_dir);
    let usr_bin_dentry = usr_dentry.insert("bin".to_string(), usr_bin_dir);
    let lib_dentry = root.insert("lib".to_string(), lib_dir);

    info!("VFS: Virtual directories /bin, /sbin, /usr/bin, /lib mounted");

    // 3. 寻找实际磁盘上的 busybox 和动态链接库
    if let Some(musl_dir) = root.find_tree("/musl", true) {
        
        // --- 处理所有的基础命令 (映射到 busybox) ---
        if let Some(busybox_node) = musl_dir.find_child("busybox") {
            let busybox_inode = busybox_node.inode.clone();
            
            // LTP 测试常用的 busybox 小工具列表
            let applets = vec![
                "basename", "dirname", "sh", "grep", "sed", "awk", 
                "cat", "ls", "rm", "echo", "true", "false", "wc", "mkdir"
            ];
            
            for app in applets {
                // 核心：把 busybox 的实体 Inode 以不同名字直接塞进虚拟目录的 Dentry 里！
                // 这相当于在内存里创建了硬链接。
                bin_dentry.insert(app.to_string(), busybox_inode.clone());
                sbin_dentry.insert(app.to_string(), busybox_inode.clone());
                usr_bin_dentry.insert(app.to_string(), busybox_inode.clone());
            }
            info!("VFS: Busybox applets populated");
        }

        // --- 处理动态链接库 (让 ELF 加载器能找到 libc.so) ---
        // 假设 libc.so 在 /musl/libc.so 或者 /musl/lib/libc.so
        // 这里以 /musl/libc.so 为例，如果你的在 lib 下请改为 find_tree("/musl/lib/libc.so", true)
        if let Some(libc_node) = root.find_tree("/musl/libc.so", true).or_else(|| root.find_tree("/musl/lib/libc.so", true)) {
            // RISC-V MUSL 的 ELF 头通常会找 ld-musl-riscv64.so.1
            lib_dentry.insert("ld-musl-riscv64.so.1".to_string(), libc_node.inode.clone());
            lib_dentry.insert("libc.so".to_string(), libc_node.inode.clone());
            info!("VFS: libc.so symlinks populated");
        }
    }
}