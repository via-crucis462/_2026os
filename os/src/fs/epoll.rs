use crate::fs::{File, Stat, Dentry};
use crate::mm::UserBuffer;
use crate::auth::{PermSet, PermStat, FileMode};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spin::Mutex;
use core::any::Any;

// 1. Linux 标准的 Epoll 事件结构
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64, // 用户态传入的 payload，原样退回
}

// 2. 真正的 Epoll 文件系统抽象
pub struct EpollFile {
    // 监控列表：fd -> EpollEvent
    pub interest_list: Mutex<BTreeMap<usize, EpollEvent>>,
}

impl EpollFile {
    pub fn new() -> Self {
        Self { interest_list: Mutex::new(BTreeMap::new()) }
    }
}

impl File for EpollFile {
    fn info_type(&self) { println!("epoll"); }
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { false }
    fn read(&self, _buf: UserBuffer) -> usize { 0 }
    fn pread(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn write(&self, _buf: UserBuffer) -> usize { 0 }
    fn raw_read_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn raw_write_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        // 匿名 Inode 的标准返回
        Stat {
            dev: 0, ino: 0, mode: 0, nlink: 1, uid: 0, gid: 0, rdev: 0, __pad: 0,
            size: 0, blksize: 0, __pad2: 0, blocks: 0, atime_sec: 0, atime_nsec: 0,
            mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }
    fn get_dentry(&self) -> Option<Arc<Dentry>> { None }
    fn lseek(&self, _offset: isize, _whence: i32) -> isize { -1 }
    fn as_any(&self) -> &dyn Any { self }
}

// 3. 真实的 EventFd 抽象（极其常用）
pub struct EventFile {
    pub count: Mutex<u64>,
}

impl EventFile {
    pub fn new(initval: u32) -> Self {
        Self { count: Mutex::new(initval as u64) }
    }
}

impl File for EventFile {
    fn info_type(&self) { println!("eventfd"); }
    fn readable(&self) -> bool { *self.count.lock() > 0 }
    fn writable(&self) -> bool { true }
    fn read(&self, _buf: UserBuffer) -> usize { 0 }
    fn pread(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn write(&self, _buf: UserBuffer) -> usize { 0 }
    fn raw_read_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn raw_write_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 0, mode: 0, nlink: 1, uid: 0, gid: 0, rdev: 0, __pad: 0,
            size: 0, blksize: 0, __pad2: 0, blocks: 0, atime_sec: 0, atime_nsec: 0,
            mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }
    fn get_dentry(&self) -> Option<Arc<Dentry>> { None }
    fn lseek(&self, _offset: isize, _whence: i32) -> isize { -1 }
    fn as_any(&self) -> &dyn Any { self }
}