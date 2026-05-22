//! 内存管理相关syscall，暂未完全迁移

use super::*;
use Errno::*;

pub fn sys_shmget(key: usize, size: usize, flags: i32) -> isize {
    ENOSYS.as_isize()
}

pub fn sys_shmctl(shmid: u32, cmd: i32, flags: i32) -> isize {
    ENOSYS.as_isize()
}