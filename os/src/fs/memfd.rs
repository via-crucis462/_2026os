use alloc::sync::Arc;
use spin::Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use alloc::collections::BTreeMap;

use crate::mm::address::VPNRange;
use crate::mm::{FrameTracker, UserBuffer, VirtPageNum, translated_byte_buffer_mut, translated_write};
use crate::auth::{PermStat, FileMode};
use crate::arch::config::PAGE_SIZE;
use crate::task::current_user_token;
use crate::syscall::errno::*;
use super::{Dentry, File, VfsInode, TmpfsFileInode};
use super::tmpfs::TMPFS_INO_COUNTER;

pub struct MemFdInode {
    ino: usize,
    range: Arc<Mutex<VPNRange>>,
    token: usize, // 用户根页表
    perms: Mutex<PermStat>, // 权限信息
}

impl MemFdInode {
    /// 创建一个新的 MemFdInode，接受起始和结束的虚拟页号
    pub fn new(start: VirtPageNum, end: VirtPageNum) -> Self {
        Self {
            ino: TMPFS_INO_COUNTER.fetch_add(1, Ordering::SeqCst),
            range: Arc::new(Mutex::new(VPNRange::new(start, end))),
            token: current_user_token(),
            perms: Mutex::new(PermStat::new(FileMode::from_bits_truncate(0o100777), 0, 0)), // 默认权限
        }
    }
    /// 获取虚拟页号范围
    pub fn get_range(&self) -> VPNRange {
        self.range.lock().clone()
    }
    /// 设置虚拟页号范围，需要调用者确保合法性
    pub fn set_range(&mut self, start: VirtPageNum, end: VirtPageNum) {
        *self.range.lock() = VPNRange::new(start, end);
    }
    pub fn write(&mut self, offset: usize, data: UserBuffer) -> isize {
        let range = self.get_range();
        let start_addr = range.get_start().0 * PAGE_SIZE;
        let end_addr = range.get_end().0 * PAGE_SIZE;
        let write_start = (start_addr + offset) as *const u8;
        if write_start as usize + data.len() > end_addr {
            return Errno::EFAULT.as_isize() ; // 超出范围
        }
        let mut buffer = translated_byte_buffer_mut(self.token, write_start, data.len());
        let mut i = 0;
        let mut j = 0;
        for buf in buffer.iter_mut() {
            for ch in buf.iter_mut() {
                if let Some(da) = data.buffers[i].get(j) {
                    *ch = unsafe {*da};
                    i += 1;
                } else {
                    i = 0;
                    j += 1;
                    break;
                }
            }
        }
        data.len() as isize
    }
}
