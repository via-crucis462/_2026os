//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod pipe;
mod stdio;
mod dir_entry;
mod file_tree;

pub use dir_entry::DirEntry;
pub use file_tree::ROOT_DENTRY;
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
}

/// The stat of a inode
#[repr(C)]
#[derive(Debug)]
pub struct Stat {
    /// ID of device containing file
    pub dev: u64,
    /// inode number
    pub ino: u64,
    /// file type and mode
    pub mode: StatMode,
    /// number of hard links
    pub nlink: u32,
    /// unused pad
    pad: [u64; 7],
}
pub trait VfsInode: Send + Sync {
    fn ls<'a>(&'a self) -> Box<dyn Iterator<Item = DirEntry> + 'a>;
    fn init(&self);
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize;
    fn get_size(&self) -> usize;
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>>;
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