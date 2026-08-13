use crate::fs::{File, Stat, Dentry};
use crate::mm::UserBuffer;
use crate::auth::{PermSet, PermStat, FileMode};
use crate::process::scheduler::wait::wake_up_all_mp;
use crate::sync::{MPSafeCell, WaitQueue};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use spin::Mutex;
use core::any::Any;

/// Epoll 事件
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EpollEvent {
    pub events: u32, // 监控的事件类型
    pub data: u64, // 用户传入的标识数据
}

/// Epoll 文件
pub struct EpollFile {
    /// 监控事件：fd -> EpollEvent
    /// 
    /// 与下面的 watched_files 保持一致性，epoll_ctl 添加/删除时同时修改两者
    /// 后续考虑合并为一张表
    pub interest_list: Mutex<BTreeMap<usize, EpollEvent>>,
    /// 文件可能在 epoll 删除前被关闭，保持文件生命周期
    /// 
    /// bug：文件被关闭后 fd 可能被重用，后续应该修改
    /// 但现在的实现能跑通 buildstorm
    pub watched_files: Mutex<BTreeMap<usize, Arc<dyn File + Send + Sync>>>,
    // Readiness state used to implement edge-triggered and one-shot delivery.
    pub last_ready: Mutex<BTreeSet<usize>>,
    pub oneshot_disabled: Mutex<BTreeSet<usize>>,
    /// 等待队列，阻塞在 epoll_wait 上的任务
    pub waiters: Arc<MPSafeCell<WaitQueue>>,
}

impl EpollFile {
    pub fn new() -> Self {
        Self {
            interest_list: Mutex::new(BTreeMap::new()),
            watched_files: Mutex::new(BTreeMap::new()),
            last_ready: Mutex::new(BTreeSet::new()),
            oneshot_disabled: Mutex::new(BTreeSet::new()),
            waiters: Arc::new(MPSafeCell::new(WaitQueue::new())),
        }
    }

    pub fn wake_waiters(&self) {
        wake_up_all_mp(&self.waiters);
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

/// Event 文件
pub struct EventFile {
    pub count: Mutex<u64>,
    poll_waiters: Arc<MPSafeCell<WaitQueue>>,
}

impl EventFile {
    pub fn new(initval: u32) -> Self {
        Self {
            count: Mutex::new(initval as u64),
            poll_waiters: Arc::new(MPSafeCell::new(WaitQueue::new())),
        }
    }
}

impl File for EventFile {
    fn info_type(&self) { println!("eventfd"); }
    fn readable(&self) -> bool { *self.count.lock() > 0 }
    fn writable(&self) -> bool { true }
    fn read(&self, buf: UserBuffer) -> usize {
        // 睡眠等待至 eventfd 计数非零，然后取走整个计数
        let value = loop {
            let mut cnt = 0;
            let mut count = self.count.lock();
            if *count == 0 {
                drop(count);
                if cnt % 1000 == 0 {
                    println!("eventfd: read blocked, loop count={}", cnt);
                }
                crate::process::suspend_current_and_run_next();
                cnt += 1;
                continue;
            }
            let value = *count;
            *count = 0;
            break value;
        };
        let mut iter = buf.into_iter();
        for i in 0..8 {
            match iter.next() {
                Some(b) => unsafe { *b = ((value >> (8 * i)) & 0xff) as u8 },
                None => return i,
            }
        }
        8
    }
    fn write(&self, buf: UserBuffer) -> usize {
        let mut iter = buf.into_iter();
        let mut value: u64 = 0;
        for i in 0..8 {
            match iter.next() {
                Some(b) => value |= (unsafe { *b } as u64) << (8 * i),
                None => return 0,
            }
        }
        let mut count = self.count.lock();
        *count = count.saturating_add(value);
        drop(count);
        wake_up_all_mp(&self.poll_waiters);
        8
    }
    fn raw_read_at(&self, _offset: usize, buf: UserBuffer) -> usize { self.read(buf) }
    fn raw_write_at(&self, _offset: usize, buf: UserBuffer) -> usize { self.write(buf) }
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
    fn poll_wait_queue(&self) -> Option<Arc<MPSafeCell<WaitQueue>>> {
        Some(self.poll_waiters.clone())
    }
    fn as_any(&self) -> &dyn Any { self }
}
