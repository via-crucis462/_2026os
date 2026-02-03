//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod pipe;
mod stdio;
mod dir_entry;
mod file_tree;

pub use dir_entry::DirEntry;
pub use file_tree::{ROOT_DENTRY, parent_path, file_name, create_file_in_dentry};
use alloc::boxed::Box;
use crate::mm::UserBuffer;
use alloc::sync::Arc;
/// trait File for all file types
pub trait File: Send + Sync {
    /// the file readable?
    fn readable(&self) -> bool;
    /// the file writable?
    fn writable(&self) -> bool;
    /// read from the file to buf, return the number of bytes read
    fn read(&self, buf: UserBuffer) -> usize;
    /// write to the file from buf, return the number of bytes written
    fn write(&self, buf: UserBuffer) -> usize;
    /// get the stat of the file
    fn get_stat(&self) -> Stat;
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
    /// unused
    pub __unused: [u32; 2],
}
pub trait VfsInode: Send + Sync {
    fn ls<'a>(&'a self) -> Box<dyn Iterator<Item = DirEntry> + 'a>;
    fn init(&self);
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize;
    fn get_size(&self) -> usize;
    fn get_stat(&self) -> Stat;
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>>;
    fn create_file(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>>;
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

pub use inode::{list_apps, OpenFlags, open_file, ROOT_INODE};
pub use pipe::{make_pipe, Pipe};
pub use stdio::{Stdin, Stdout};