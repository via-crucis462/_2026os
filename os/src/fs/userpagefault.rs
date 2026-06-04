use super::*;
use crate::process::task::TaskControlBlock;
use crate::process::{block_current_and_run_next, wake_up_task};
use crate::sync::{MPSafeCell, WaitQueue};
use crate::mm::{PageTable, UserBuffer, VirtAddr, translated_byte_buffer, try_translated_write, try_translated_read};
use alloc::vec::Vec;

// struct uffdio_api { api: u64, features: u64, ioctls: u64 }  → 24 bytes
// struct uffdio_range { start: u64, len: u64 }                → 16 bytes
// struct uffdio_register { range(16), mode: u64, ioctls: u64 } → 32 bytes
// struct uffdio_copy { dst: u64, src: u64, len: u64, mode: u64, copy: i64 } → 40 bytes

const UFFDIO_API: u32      = 0xC018AA3F;
const UFFDIO_REGISTER: u32 = 0xC020AA00;
const UFFDIO_COPY: u32     = 0xC028AA03;

/// struct uffd_msg (packed, 24 bytes for pagefault event)
///   u8  event     = 0x12 (UFFD_EVENT_PAGEFAULT)
///   u8  reserved1 = 0
///   u16 reserved2 = 0
///   u32 reserved3 = 0
///   u64 flags     = 0  (arg.pagefault.flags)
///   u64 address   = faulting_address  (arg.pagefault.address)
const UFFD_MSG_SIZE: usize = 24;
const UFFD_EVENT_PAGEFAULT: u64 = 0x12;

/// Write a uffd_msg for a pagefault event into the user buffer.
/// Returns the number of bytes written.
fn fill_uffd_msg_pagefault(buf: UserBuffer, fault_addr: usize) -> usize {
    let len = buf.len().min(UFFD_MSG_SIZE);
    let mut i = 0;
    for byte_ref in buf.into_iter() {
        if i >= len { break; }
        unsafe {
            *byte_ref = match i {
                0 => UFFD_EVENT_PAGEFAULT as u8,  // event
                1..=7 => 0,                         // reserved1, reserved2, reserved3
                8..=15 => 0,                        // flags (low bytes first)
                16..=23 => ((fault_addr as u64) >> ((i - 16) * 8)) as u8, // address (LE)
                _ => 0,
            };
        }
        i += 1;
    }
    println!("fill uffd_msg: event=0x{:x}, address=0x{:x}, len={}", UFFD_EVENT_PAGEFAULT, fault_addr, len);
    len
}

pub struct UserPageFaultInfo {
    //发生缺页的线程
    pub faulting_task: MPSafeCell<Option<Arc<TaskControlBlock>>>,
    // 缺页地址
    pub faulting_address: MPSafeCell<usize>,
    // 等待缺页事件的线程们（阻塞在 read(uffd) 上的 handler）
    pub read_waiters: MPSafeCell<WaitQueue>,
    // 已注册的内存区域
    pub registered_ranges: MPSafeCell<Vec<(usize, usize)>>,  // (start, len)
    // 模式，非阻塞或阻塞
    pub block: bool,
}
impl File for UserPageFaultInfo {
    /// the file readable?
    fn readable(&self) -> bool {
        true
    }
    /// the file writable?
    fn writable(&self) -> bool {
        false
    }
    /// read from the file to buf, return the number of bytes read
    fn read(&self, buf: UserBuffer) -> usize {
        // 阻塞直到有缺页事件
        if !self.faulting_task.exclusive_access().is_some() {
            block_current_and_run_next(&self.read_waiters);
        }
        // 缺页已发生：读取 faulting_address，构造 uffd_msg 写入用户缓冲区
        let fault_addr = *self.faulting_address.exclusive_access();
        fill_uffd_msg_pagefault(buf, fault_addr)
    }
    /// write to the file from buf, return the number of bytes written
    fn pread(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    fn write(&self, _buf: UserBuffer) -> usize { 0 }
    fn write_nonblock(&self, buf: UserBuffer) -> Result<usize, Errno> {
        Ok(self.write(buf))
    }
    /// read from the file to buf at a given offset, return the number of bytes read
    fn read_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    /// write to the file from buf at a given offset, return the number of bytes written
    fn write_at(&self, _offset: usize, _buf: UserBuffer) -> usize { 0 }
    /// 获取文件权限信息
    fn get_perm(&self) -> PermStat{
        PermStat {
            mode: FileMode::from_bits_truncate(0o444), // 只读权限
            uid: 0,
            gid: 0,
        }
    }
    /// 修改权限，返回是否成功
    fn set_perm(&self, perm: PermStat) -> bool {
        // 默认不允许修改权限
        false
    }
    /// get the stat of the file
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0,
            ino: 0,
            mode: 0o444, // 只读权限
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 0,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2],
        }
    }
    /// 获取目录下的所有目录项
    fn getdents(&self, _buf: &mut [u8]) -> isize { 0 }
    /// 获取文件的 Dentry
    fn get_dentry(&self) -> Option<Arc<Dentry>> { None }
    fn lseek(&self, _offset: isize, _whence: i32) -> isize {
        Errno::ESPIPE.as_isize()
    }
    //顶层read直接处理非阻塞读取
    fn ready_to_read(&self) -> bool {
        self.faulting_task.exclusive_access().is_some()
    }
    /// Is there space available to write right now?
    fn ready_to_write(&self) -> bool {
        self.writable()
    }
    fn check_write_error(&self) -> Option<Errno> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn set_time(&self, _atime: &TimeSpec, _mtime: &TimeSpec) -> isize {
        0
    }
    // 获取该文件指定页偏移的物理页号。
    // 如果没有，文件内部负责分配一个并存起来。
    fn get_shared_page(&self, page_offset: usize) -> Option<PhysPageNum> {
        None // 默认不支持
    }
    fn ioctl(&self, request: u32, argp: usize, token: usize) -> isize {
        let a = request;
        match request {
            UFFDIO_API => {
                let api: u64 = try_translated_read(token, argp as *const u64).unwrap_or(0);
                if api != 0xAA {
                    return Errno::EINVAL.as_isize();
                }
                // features = 0, ioctls = _UFFDIO_REGISTER | _UFFDIO_COPY
                let ioctls: u64 = (1 << _UFFDIO_REGISTER_BIT) | (1 << _UFFDIO_COPY_BIT);
                try_translated_write(token, (argp + 8) as *mut u64, 0u64);
                try_translated_write(token, (argp + 16) as *mut u64, ioctls);
                0
            }
            UFFDIO_REGISTER => {
                let start: u64 = try_translated_read(token, argp as *const u64).unwrap_or(0);
                let len: u64 = try_translated_read(token, (argp + 8) as *const u64).unwrap_or(0);
                let mode: u64 = try_translated_read(token, (argp + 16) as *const u64).unwrap_or(0);
                if mode & 1 == 0 {
                    return Errno::EINVAL.as_isize();
                }
                self.registered_ranges.exclusive_access()
                    .push((start as usize, len as usize));
                let ioctls: u64 = (1 << _UFFDIO_COPY_BIT);
                try_translated_write(token, (argp + 24) as *mut u64, ioctls);
                0
            }
            UFFDIO_COPY => {
                let dst: u64 = try_translated_read(token, argp as *const u64).unwrap_or(0);
                let src: u64 = try_translated_read(token, (argp + 8) as *const u64).unwrap_or(0);
                let len: u64 = try_translated_read(token, (argp + 16) as *const u64).unwrap_or(0);
                let mode: u64 = try_translated_read(token, (argp + 24) as *const u64).unwrap_or(0);

                // 先提取 proc 引用，释放锁后再操作，避免死锁
                let proc = {
                    let guard = self.faulting_task.exclusive_access();
                    guard.as_ref().map(|t| t.process())
                };
                let proc = match proc {
                    Some(p) => p,
                    None => return Errno::EINVAL.as_isize(),
                };

                // 1. 为缺页地址建立物理页映射
                let _ = proc.mmap(
                    dst as usize, core::cmp::max(len as usize, 4096),
                    crate::mm::mmap::MMapProt::PROT_READ | crate::mm::mmap::MMapProt::PROT_WRITE,
                    crate::mm::mmap::MMapFlags::MAP_ANONYMOUS
                        | crate::mm::mmap::MMapFlags::MAP_PRIVATE
                        | crate::mm::mmap::MMapFlags::MAP_FIXED,
                    None, 0,
                );

                // 2. 从 src 拷贝数据到 dst
                let src_bufs = translated_byte_buffer(token, src as *const u8, len as usize);
                let mut total = 0;
                for src_buf in src_bufs.iter() {
                    let mut dst_bufs = crate::mm::translated_byte_buffer_mut(
                        token, (dst as usize + total) as *const u8, src_buf.len());
                    for (d, s) in dst_bufs.iter_mut().zip(core::iter::repeat(src_buf)) {
                        d.copy_from_slice(s);
                    }
                    total += src_buf.len();
                }

                // 3. 写回已拷贝字节数
                try_translated_write(token, (argp + 32) as *mut i64, len as i64);

                // 4. 唤醒缺页线程
                if mode & 1 == 0 {
                    let mut guard = self.faulting_task.exclusive_access();
                    if let Some(task) = guard.take() {
                        drop(guard);
                        wake_up_task(task);
                    }
                }
                len as isize
            }
            _ => Errno::ENOTTY.as_isize(),
        }
    }
}

const _UFFDIO_REGISTER_BIT: u8 = 0;
const _UFFDIO_COPY_BIT: u8 = 3;

impl UserPageFaultInfo{
    pub fn new(block: bool) -> Self {
        Self {
            faulting_task: MPSafeCell::new(None),
            faulting_address: MPSafeCell::new(0),
            read_waiters: MPSafeCell::new(WaitQueue::new()),
            registered_ranges: MPSafeCell::new(Vec::new()),
            block,
        }
    }
}