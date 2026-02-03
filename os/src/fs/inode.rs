#[allow(unused)]
use super::File;
use crate::drivers::BLOCK_DEVICE;
use alloc::sync::Arc;
use bitflags::*;
use lazy_static::*;
use crate::ext4fs::ext4::Ext4FS;
use crate::ext4fs::block_dev::BlockDevice;
use crate::ext4fs::ext4inode::Ext4Inode;
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
        const CREATE = 1 << 9;
        /// truncate file size to 0
        const TRUNC = 1 << 10;
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
}
pub fn create_root_inode(device: Arc<dyn BlockDevice>) -> Arc<OSInode> {
    let ext4fs = Ext4FS::open(device.clone());
    let root_disk_inode = ext4fs.get_disk_inode(2); // ext4根目录通常是2号
    let vfs_inode = Arc::new(Ext4Inode::new(2, &root_disk_inode, Arc::new(ext4fs), None));
    Arc::new(OSInode::new(true, false, vfs_inode))
}
pub fn open_file(path: &str, flags: OpenFlags) -> Option<Arc<OSInode>> {
    // 使用全局 Dentry 树递归查找路径，并自动填充缓存
    let target_dentry = crate::fs::ROOT_DENTRY.find_tree(path);

    let (readable, writable) = flags.read_write();
    Some(Arc::new(OSInode::new(
        readable,
        writable,
        target_dentry.inode.clone(),
    )))
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
