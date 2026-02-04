#[allow(unused)]
use super::File;
use crate::drivers::BLOCK_DEVICE;
use alloc::sync::Arc;
use bitflags::*;
use lazy_static::*;
use crate::ext4fs::ext4::Ext4FS;
use crate::ext4fs::block_dev::BlockDevice;
use crate::ext4fs::ext4inode::Ext4Inode;
use crate::fs::file_tree::*;
use super::VfsInode;
use spin::Mutex;
use crate::mm::UserBuffer;

pub struct OSInode {
    readable: bool,
    writable: bool,
    inner: Mutex<OSInodeInner>,
    pub inode: Arc<dyn VfsInode>,   //实现了VfsInode trait的具体文件系统的inode
}

pub struct OSInodeInner {
    offset: usize,
}

impl OSInode {
    pub fn new(readable: bool, writable: bool, inode: Arc<dyn VfsInode>) -> Self {
        Self {
            readable,
            writable,
            inner: Mutex::new(OSInodeInner { offset: 0 }),
            inode,
        }
    }
    pub fn read_all(&self) -> alloc::vec::Vec<u8> {
        // 1. 获取文件总大小
        let size = self.inode.get_size();
        trace!("[kernel] read_all: size={}", size);
        // 2. 准备缓冲区
        let mut buffer = alloc::vec![0u8; size];
        // 3. 从偏移量 0 开始读取
        let read_len = self.inode.read_at(0, &mut buffer);
        trace!("[kernel] read_all: read_len={}", read_len);
        
        // 理论上 read_len 应该等于 size
        if read_len != size {
            buffer.truncate(read_len);
        }
        buffer
    }
}

impl File for OSInode {
    fn readable(&self) -> bool { self.readable }
    fn writable(&self) -> bool { self.writable }

    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut inner = self.inner.lock();
        let mut total_read = 0;
        // 针对 UserBuffer 的每一段进行读取（处理跨页）
        for slice in buf.buffers.iter_mut() {
            let read_len = self.inode.read_at(inner.offset, *slice);
            if read_len == 0 { break; }
            inner.offset += read_len;
            total_read += read_len;
        }
        total_read
    }

    fn write(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.lock();
        let mut total_write = 0;
        for slice in buf.buffers.iter() {
            let write_len = self.inode.write_at(inner.offset, *slice);
            if write_len == 0 { break; }
            inner.offset += write_len;
            total_write += write_len;
        }
        total_write
    }

    fn get_stat(&self) -> super::Stat {
        self.inode.get_stat()
    }
}
bitflags! {
    ///  The flags argument to the open() system call is constructed by ORing together zero or more of the following values:
    pub struct OpenFlags: u32 {
        /// readyonly
        const RDONLY = 0;
        /// writeonly
        const WRONLY = 1 << 0;
        /// read and write
        const RDWR = 1 << 1;
        /// create new file
        const CREATE = 1 << 6;
        /// truncate file size to 0
        const TRUNC = 1 << 9;
        /// 用于mkdir中，open二次确认是否新建的文件是目录类型
        const DIRECTORY = 1 << 16;
    }
}
impl OpenFlags {
    /// Do not check validity for simplicity
    /// Return (readable, writable)
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {
            (true, false)
        } else if self.contains(Self::WRONLY) {
            (false, true)
        } else {
            (true, true)
        }
    }
    pub fn should_create(&self) -> bool {
        self.contains(Self::CREATE)
    }
    pub fn should_be_directory(&self) -> bool {
        self.contains(Self::DIRECTORY)
    }
}
pub fn create_root_inode(device: Arc<dyn BlockDevice>) -> Arc<OSInode> {
    let ext4fs = Ext4FS::open(device.clone());
    let root_disk_inode = ext4fs.get_disk_inode(2); // ext4根目录通常是2号
    let vfs_inode = Arc::new(Ext4Inode::new(2, &root_disk_inode, Arc::new(ext4fs), None));
    Arc::new(OSInode::new(true, false, vfs_inode))
}
pub fn open_file(path: &str, flags: OpenFlags) -> Option<Arc<OSInode>> {
    // 使用全局 Dentry 树递归查找路径，并自动填充缓存
    // 1. 查找文件是否已存在
    let target_dentry = ROOT_DENTRY.find_tree(path);
    // 2.1 若不存在
    // 2.1.1 若文件不需要创建，返回 None
    if target_dentry.is_none() {
        // 文件不存在，且没有创建标志，返回 None
        if !flags.should_create() {
            return None;
        }
        // 创建新文件的逻辑（简化处理，只创建空文件）
    // 2.1.2 创建新文件
        let parent_path = parent_path(path);
        let parent_dentry = ROOT_DENTRY.find_tree(&parent_path)?;
        let file_name = file_name(path);
        let new_dentry = create_file_in_dentry(&parent_dentry, file_name);
        let (readable, writable) = flags.read_write();
        return Some(Arc::new(OSInode::new(
            readable,
            writable,
            new_dentry.inode.clone(),
        )));
    }
    // 2.2 若存在，直接返回对应的 OSInode
    let target_dentry = target_dentry.unwrap();
    let (readable, writable) = flags.read_write();
    Some(Arc::new(OSInode::new(
        readable,
        writable,
        target_dentry.inode.clone(),
    )))
}

pub fn make_dir(path: &str , _mode: u32) -> Option<u32> {
    // 1. 检查目录是否已存在
    if ROOT_DENTRY.find_tree(path).is_some() {
        println!("VFS: make_dir - target '{}' already exists", path);
        return None; 
    }
    let parent_path = parent_path(path);
    let parent_dentry = ROOT_DENTRY.find_tree(&parent_path)?;
    let dir_name = file_name(path);
    println!("VFS: make_dir - creating directory '{}' in parent '{}'", dir_name, parent_path);
    let new_dentry = create_dir_in_dentry(&parent_dentry, dir_name , _mode);
    Some(new_dentry.inode.get_stat().ino as u32)
}
/// List all apps in the root directory
pub fn list_apps() {
    info!("/**** APPS ****");
    for app in ROOT_INODE.inode.ls() {
        println!("{}", app.name);
    }
    info!("**************/");
}
lazy_static! {
    pub static ref ROOT_INODE: Arc<OSInode> = {
        let root = create_root_inode(BLOCK_DEVICE.clone());
        root.inode.init();
        root
    };
}
