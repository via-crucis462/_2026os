//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod pipe;
mod stdio;
mod dir_entry;
mod file_tree;
mod procfs;
mod devfs;
pub mod tmpfs;
pub use tmpfs::setup_oscomp_env;
pub use devfs::mount_devfs;
pub use procfs::mount_procfs;
pub use dir_entry::DirEntry;
pub use file_tree::{ROOT_DENTRY, parent_path, file_name, create_file_in_dentry};
pub use file_tree::{Dentry};
use crate::mm::UserBuffer;
use alloc::sync::Arc;
use alloc::string::String;
use crate::fs::tmpfs::TmpfsDirInode;
use alloc::collections::VecDeque; // 如果你用了队列

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
    // 用于unlink时调整链接数
    fn dec_link_count(&self) -> bool {
        false
    }
    fn getdents(&self, offset: &mut usize, buf: &mut [u8]) -> isize;
    fn rename_dir_entry(&self, _old_name: &str, _new_name: &str) -> bool{
        false
    }
    /// 1. 创建软链接
    /// 在当前目录下创建一个名为 `name` 的软链接，指向 `target`
    fn create_symlink(&self, name: &str, target: &str) -> Option<Arc<dyn VfsInode>> {
        // 0o120777 代表 S_IFLNK (0o120000) 加上 777 权限
        // 这个 mode 位会被你的底层识别为 0xA000 (因为 0o120000 换算成 16 进制正是 0xA000)
        let inode = self.create_file(name, 0o120777)?; 
        
        // 直接调用 write_at，它会自动判断小于 60 字节的进 i_block，大于的进数据块！
        let bytes = target.as_bytes();
        let written = inode.write_at(0, bytes);
        
        if written == bytes.len() {
            Some(inode)
        } else {
            // 写入失败时最好删掉刚创建的 entry，这里做简单的防御性返回
            trace!("VFS: create_symlink failed to write target path");
            None
        }
    }

    fn readlink(&self) -> String {
        let size = self.get_size();
        if size == 0 {
            return String::new();
        }
        
        // 分配对应大小的缓冲区，调用你已经写好的 read_at 逻辑读取目标路径
        let mut buf = alloc::vec![0u8; size];
        self.read_at(0, &mut buf);
        
        // 转换为字符串并返回
        String::from_utf8_lossy(&buf).into_owned()
    }
    /// 3. 创建硬链接
    fn link(&self, name: &str, inode: Arc<dyn VfsInode>) -> bool {
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
        // symbolic link (S_IFLNK)  
        const SYMLINK = 0o120000;
    }
}

pub use inode::{list_apps, OpenFlags, open_file, ROOT_INODE, ROOT_VFS_INODE, make_dir, OSInode};
pub use pipe::{make_pipe, Pipe};
pub use stdio::{Stdin, Stdout};



pub fn init_test_env() {
println!("[VFS] Mounting true Tmpfs directories in memory...");

    ROOT_DENTRY.insert(String::from("tmp"), Arc::new(TmpfsDirInode::new()));
    ROOT_DENTRY.insert(String::from("var"), Arc::new(TmpfsDirInode::new()));
}

const MAX_SYMLINK_DEPTH: usize = 8; // 地雷1：防止无限递归导致内核栈溢出

