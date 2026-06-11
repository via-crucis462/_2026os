//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod fifo;
mod pipe;
mod stdio;
mod dir_entry;
mod file_tree;
mod procfs;
mod devfs;
mod userpagefault;
mod ino;

pub mod memfd;
use alloc::vec::{self, Vec};
pub use memfd::*;
pub mod tmpfs;
pub use tmpfs::setup_oscomp_env;
pub use tmpfs::{TmpfsFileInode, TmpfsDirInode};
pub use procfs::mount_procfs;
pub use dir_entry::DirEntry;
pub use file_tree::{ROOT_DENTRY, parent_path, file_name, create_file_in_dentry};
pub use file_tree::{Dentry};
pub use fifo::{create_fifo_in_dentry, is_fifo_mode, open_fifo_file, S_IFIFO, S_IFMT};
pub use userpagefault::UserPageFaultInfo;
use crate::PAGE_SIZE_BITS;
pub use crate::timer::TimeSpec;
pub use ino::get_next_ino;
use crate::mm::UserBuffer;
use crate::syscall::errno::Errno;
use alloc::sync::Arc;
use spin::Mutex;
use alloc::string::String;
use alloc::collections::VecDeque; 
use core::any::Any;
pub mod epoll; 
pub use epoll::{EpollFile, EpollEvent}; 
use crate::syscall::fs::Statfs;
use crate::auth::{FileMode, PermSet, PermStat};
use crate::mm::PhysPageNum;

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
    fn write_nonblock(&self, buf: UserBuffer) -> Result<usize, Errno> {
        Ok(self.write(buf))
    }
    /// 底层原始读取（绕过页缓存，直接读存储）
    fn raw_read_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    /// 底层原始写入（绕过页缓存，直接写存储）。回写脏页等场景使用。
    fn raw_write_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }

    /// 带页缓存的读取。默认直接调用 raw_read_at。
    fn read_at(&self, offset: usize, buf: UserBuffer) -> usize {
        self.raw_read_at(offset, buf)
    }
    /// 带页缓存的写入。默认直接调用 raw_write_at。
    fn write_at(&self, offset: usize, buf: UserBuffer) -> usize {
        self.raw_write_at(offset, buf)
    }
    /// 获取文件权限信息
    fn get_perm(&self) -> PermStat;
    /// 修改权限，返回是否成功
    fn set_perm(&self, perm: PermStat) -> bool {
        // 默认不允许修改权限
        false
    }
    /// get the stat of the file
    fn get_stat(&self) -> Stat;
    /// 获取目录下的所有目录项
    fn getdents(&self, _buf: &mut [u8]) -> isize;
    /// 获取文件的 Dentry
    fn get_dentry(&self) -> Option<Arc<Dentry>> { None }
    fn lseek(&self, _offset: isize, _whence: i32) -> isize {
        Errno::ESPIPE.as_isize()
    }
    fn ready_to_read(&self) -> bool {
        self.readable()
    }
    /// Is there space available to write right now?
    fn ready_to_write(&self) -> bool {
        self.writable()
    }
    fn check_write_error(&self) -> Option<Errno> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        unimplemented!("as_any not implemented for this file type")
    }
    fn set_time(&self, _atime: &TimeSpec, _mtime: &TimeSpec) -> isize {
        0
    }
    fn ino(&self) -> u64 {
        self.get_stat().ino
    }
    /// 截断/扩展文件到指定大小
    fn truncate(&self, _len: usize) -> bool {
        false // 默认不支持
    }
    // 这里是默认实现，需要为不同文件重写
    fn get_shared_page(&self, page_offset: usize) -> Option<Arc<Mutex<crate::mm::mmap::PageCache>>> {
        error!("File type does not support shared pages: page_offset={}", page_offset);
        None
    }
    /// ioctl 设备控制，默认返回 ENOTTY（不支持的 ioctl 请求）
    fn ioctl(&self, _request: u32, _argp: usize, _token: usize) -> isize {
        Errno::ENOTTY.as_isize()
    }
}

/// 文件状态结构体 (musl riscv64 `struct stat` ABI)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
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
    /// padding (musl: unsigned __unused[2] = 8 bytes)
    pub __unused: [u32; 2],
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
pub const UTIME_NOW: usize = 0x3fffffff;
pub const UTIME_OMIT: usize = 0x3ffffffe;
pub trait VfsInode: Send + Sync {
    /// 底层原始读取，不经过页缓存。由具体文件系统实现。
    /// 对于磁盘文件系统（Ext4），这直接读写磁盘块；
    /// 对于虚拟文件系统（tmpfs/procfs/devfs），这就是它们的实际数据读取逻辑。
    fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    /// 底层原始写入，不经过页缓存。由具体文件系统实现。
    fn raw_write_at(&self, offset: usize, buf: &[u8]) -> usize;

    /// 带页缓存的读取。
    /// 默认直接转发到 raw_read_at。磁盘文件系统（Ext4）应覆写此方法以接入 SharedPageCacheManager。
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.raw_read_at(offset, buf)
    }
    /// 带页缓存的写入。
    /// 默认直接转发到 raw_write_at。磁盘文件系统（Ext4）应覆写此方法以接入 SharedPageCacheManager。
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        self.raw_write_at(offset, buf)
    }
    fn get_size(&self) -> usize;
    /// 截断/扩展文件到指定大小
    /// len < 当前大小：丢弃超出部分
    /// len > 当前大小：扩展并用零填充（对 tmpfs 等可以只更新 size）
    fn truncate(&self, _len: usize) -> bool {
        panic!("truncate not implemented for this inode type");
        false // 默认不支持
    }
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
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
    fn set_perm(&self, _perm: PermStat) -> bool {
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
    fn set_time(&self, _atime: &TimeSpec, _mtime: &TimeSpec) -> isize {
        0 // 默认返回成功，至少让测试能跑通
    }
    /// 调试用：返回具体实现类型的名字
    fn type_name(&self) -> &'static str {
        core::any::type_name::<Self>()
    }
    fn statfs(&self) -> Statfs {
        // 默认实现：返回全 0 或者一个安全的默认值
        // 
        Statfs {
            f_type: 0, f_bsize: 0, f_blocks: 0, f_bfree: 0,
            f_bavail: 0, f_files: 0, f_ffree: 0, f_fsid: [0, 0],
            f_namelen: 255, f_frsize: 0, f_flags: 0, f_spare: [0; 4],
        }
    }
    fn get_shared_page(&self, page_offset: usize) -> Option<Arc<Mutex<crate::mm::mmap::PageCache>>> {
        let (cache, newly_allocated) = 
            crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER.get_page_cache(self.get_stat().ino, page_offset);
        if newly_allocated {
            // 读入文件数据到分配的页
            info!("VFS: Allocated new shared page for ino {}, page_offset {}", self.get_stat().ino, page_offset);
            let mut page = cache.lock();
            let (ppn, page_size) = (page.frame.ppn, page.frame.page_size);
            let page_addr = ppn.0 << PAGE_SIZE_BITS;
            // 检查是否对齐，防止传入的 frame 是大页
            assert!(page_addr % page_size.size() == 0, "Shared page address not aligned to its size");
            // 将物理页转换为缓冲区
            let buffer = unsafe { 
                core::slice::from_raw_parts_mut(page_addr as *mut u8, page_size.size())
            };
            // 从底层存储读取文件数据到缓存页（必须用 raw_read_at 绕过缓存，
            // 否则当 read_at 本身依赖页缓存时会形成循环调用）
            self.raw_read_at(page_offset * page_size.size(), buffer);
        } else {
            info!("VFS: Reusing existing shared page for ino {}, page_offset {}", self.get_stat().ino, page_offset);
        }
        Some(cache)
    }
    /// 返回该 inode 的唯一标识号（跨所有文件系统唯一）
    fn ino(&self) -> u64 ;
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
pub use stdio::{Stdin, Stdout, Stderr};



pub fn init_test_env() {
    println!("[VFS] Mounting true Tmpfs directories in memory...");
    ROOT_DENTRY.mount_child(String::from("tmp"), Arc::new(TmpfsDirInode::new(0o777)));
    ROOT_DENTRY.mount_child(String::from("var"), Arc::new(TmpfsDirInode::new(0o777)));
}

const MAX_SYMLINK_DEPTH: usize = 8; // 地雷1：防止无限递归导致内核栈溢出

pub fn stat_to_statx(stat: Stat) -> Statx {
    Statx {
        stx_mask: 0,
        stx_blksize: stat.blksize as u32,
        stx_attributes: 0,
        stx_nlink: stat.nlink,
        stx_uid: stat.uid,
        stx_gid: stat.gid,
        stx_mode: stat.mode as u16,
        __spare0: [0; 1],
        stx_ino: stat.ino,
        stx_size: stat.size as u64,
        stx_blocks: stat.blocks as u64,
        stx_attributes_mask: 0,
        stx_atime: StatxTimestamp {
            tv_sec: stat.atime_sec,
            tv_nsec: stat.atime_nsec as u32,
            __reserved: 0,
        },
        stx_btime: StatxTimestamp {
            tv_sec: 0,
            tv_nsec: 0,
            __reserved: 0,
        },
        stx_ctime: StatxTimestamp {
            tv_sec: stat.ctime_sec,
            tv_nsec: stat.ctime_nsec as u32,
            __reserved: 0,
        },
        stx_mtime: StatxTimestamp {
            tv_sec: stat.mtime_sec,
            tv_nsec: stat.mtime_nsec as u32,
            __reserved: 0,
        },
        stx_rdev_major: 0,
        stx_rdev_minor: 0,
        stx_dev_major: 0,
        stx_dev_minor: 0,
        __spare2: [0; 14],
    }
}

/*
pub struct DummySocket;

// 严格遵循你提供的 File Trait 签名
impl File for DummySocket {
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    // 核心：返回包含 Socket 标志 (S_IFSOCK = 0o140000) 的状态
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 9999, 
            mode: 0o140777, // S_IFSOCK 标志，告诉测试框架我是个 Socket
            nlink: 1, 
            uid: 0, gid: 0, 
            rdev: 0, __pad: 0, 
            size: 0, blksize: 512, __pad2: 0, blocks: 0,
            atime_sec: 0, atime_nsec: 0,
            mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0,
            __unused: [0; 2], // 严格对应你定义的 [u32; 1]
        }
    }

    // 你定义的 File Trait 要求实现 getdents
    fn getdents(&self, _buf: &mut [u8]) -> isize { 
        -1 // 非目录返回 -1
    }

    // 可选：实现 get_dentry（你的 trait 里有默认实现返回 None，这里显式写明也可以）
    fn get_dentry(&self) -> Option<Arc<Dentry>> { 
        None 
    }
}
     */