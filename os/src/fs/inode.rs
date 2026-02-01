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
/// inode in memory
/// A wrapper around a filesystem inode
/// to implement File trait atop


/// List all apps in the root directory
pub fn list_apps() {
    println!("/**** APPS ****");
    for app in ROOT_INODE.ls() {
        println!("{}", app);
    }
    println!("**************/");
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
pub fn create_root_inode(device: Arc<dyn BlockDevice>) -> Arc<dyn VfsInode> {
    let ext4fs = Ext4FS::open(device.clone());
    let root_disk_inode = ext4fs.get_disk_inode(2); // ext4根目录通常是2号
    Arc::new(Ext4Inode::new(2, &root_disk_inode, Arc::new(ext4fs), None))
        // 未来可扩展
}
lazy_static! {
    pub static ref ROOT_INODE: Arc<dyn VfsInode> = {
        create_root_inode(BLOCK_DEVICE.clone())
    };
}
