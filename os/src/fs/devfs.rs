// os/src/fs/devfs.rs

use super::{VfsInode, Stat, Statx, ROOT_DENTRY};
use alloc::sync::Arc;
use alloc::string::String;


// 1. /dev 目录本身

pub struct DevDirInode;

impl VfsInode for DevDirInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 900,
            mode: 0o040555, // 0o040000 目录, 0o555 读/执行权限
            nlink: 2,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { 0 }
}

// 2. /dev/null

pub struct NullInode;

impl VfsInode for NullInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize {
        0 // 读返回 0 (EOF)
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        buf.len() // 写假装全部写成功
    }
    
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 901,
            mode: 0o020666, // 0o020000 表示字符设备 (S_IFCHR)，0o666 表示 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}


// 3. /dev/zero

pub struct ZeroInode;

impl VfsInode for ZeroInode {
    fn read_at(&self, _offset: usize, buf: &mut [u8]) -> usize {
        buf.fill(0); // 缓冲区全填 0
        buf.len()    // 返回填满的长度
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        buf.len() 
    }
    
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 902,
            mode: 0o020666, // 同样是字符设备 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}


// 4. 执行挂载

pub fn mount_devfs() {
    println!("[VFS] Mounting pseudo-filesystem: /dev");
    
    let dev_dir = Arc::new(DevDirInode);
    let null_inode = Arc::new(NullInode);
    let zero_inode = Arc::new(ZeroInode);

    // 在根目录下挂载 dev
    let dev_dentry = ROOT_DENTRY.insert(String::from("dev"), dev_dir);
    
    // 在 dev 目录下挂载 null 和 zero
    dev_dentry.insert(String::from("null"), null_inode);
    dev_dentry.insert(String::from("zero"), zero_inode);
    
    println!("[VFS] /dev/null and /dev/zero mounted successfully!");
}