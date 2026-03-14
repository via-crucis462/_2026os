//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod pipe;
mod stdio;
mod dir_entry;
mod file_tree;
mod procfs;
mod devfs;
pub use devfs::mount_devfs;
pub use procfs::mount_procfs;
pub use dir_entry::DirEntry;
pub use file_tree::{ROOT_DENTRY, parent_path, file_name, create_file_in_dentry};
pub use file_tree::{Dentry};
use crate::mm::UserBuffer;
use alloc::sync::Arc;
/// trait File for all file types
pub trait File: Send + Sync {
    /// the file readable?
    fn readable(&self) -> bool;
    /// the file writable?
    fn writable(&self) -> bool;
    /// read from the file to buf, return the number of bytes read
    fn read(&self, _buf: UserBuffer) -> usize { 0 }
    /// write to the file from buf, return the number of bytes written
    fn pread(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn write(&self, _buf: UserBuffer) -> usize { 0 }
    /// read from the file to buf at a given offset, return the number of bytes read
    fn read_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    /// write to the file from buf at a given offset, return the number of bytes written
    fn write_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    /// get the stat of the file
    fn get_stat(&self) -> Stat;
    /// 获取目录下的所有目录项
    fn getdents(&self, _buf: &mut [u8]) -> isize;
    /// 获取文件的 Dentry
    fn get_dentry(&self) -> Option<Arc<Dentry>> { None }
    fn lseek(&self, _offset: isize, _whence: i32) -> isize {
        -29 
    }
}

/// The stat of a inode
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
//文件状态结构体
pub struct Stat {
    /// ID of device containing file
    pub dev: u64,
    /// inode number
    pub ino: u64,
    /// file type and mode
    pub mode: u32,
    /// number of hard links
    pub nlink: u32,
    /// user ID of owner
    pub uid: u32,
    /// group ID of owner
    pub gid: u32,
    /// device ID (if special file)
    pub rdev: u64,
    /// padding
    pub __pad: u64,
    /// total size, in bytes
    pub size: i64,
    /// blocksize for filesystem I/O
    pub blksize: i32,
    /// padding
    pub __pad2: i32,
    /// number of 512B blocks allocated
    pub blocks: i64,
    /// time of last access
    pub atime_sec: i64,
    /// time of last access (nanoseconds)
    pub atime_nsec: i64,
    /// time of last modification
    pub mtime_sec: i64,
    /// time of last modification (nanoseconds)
    pub mtime_nsec: i64,
    /// time of last status change
    pub ctime_sec: i64,
    /// time of last status change (nanoseconds)
    pub ctime_nsec: i64,
    /// padding
    pub __unused: [u32; 1],
}
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Statx {
    pub stx_mask: u32,
    pub stx_blksize: u32,
    pub stx_attributes: u64,
    pub stx_nlink: u32,
    pub stx_uid: u32,
    pub stx_gid: u32,
    pub stx_mode: u16,  // 注意：statx 中 mode 是 u16
    pub __spare0: [u16; 1],
    pub stx_ino: u64,
    pub stx_size: u64,
    pub stx_blocks: u64,
    pub stx_attributes_mask: u64,
    pub stx_atime: StatxTimestamp,
    pub stx_btime: StatxTimestamp,
    pub stx_ctime: StatxTimestamp,
    pub stx_mtime: StatxTimestamp,
    pub stx_rdev_major: u32,
    pub stx_rdev_minor: u32,
    pub stx_dev_major: u32,
    pub stx_dev_minor: u32,
    pub __spare2: [u64; 14],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct StatxTimestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub __reserved: i32,
}
pub trait VfsInode: Send + Sync {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize;
    fn get_size(&self) -> usize;
    fn get_stat(&self) -> Stat;
    fn get_statx(&self) -> Statx;
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>>;
    fn create_file(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>>;
    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>>;
    fn delete_dir_entry(&self, name: &str) -> Option<u32>;
    fn getdents(&self, offset: &mut usize, buf: &mut [u8]) -> isize;
    fn rename_dir_entry(&self, _old_name: &str, _new_name: &str) -> bool{
        false
    }
}

bitflags! {
    /// The mode of a inode
    /// whether a directory or a file
    pub struct StatMode: u32 {
        /// null
        const NULL  = 0;
        /// directory
        const DIR   = 0o040000;
        /// ordinary regular file
        const FILE  = 0o100000;
    }
}

pub use inode::{list_apps, OpenFlags, open_file, ROOT_INODE, ROOT_VFS_INODE, make_dir, OSInode};
pub use pipe::{make_pipe, Pipe};
pub use stdio::{Stdin, Stdout};