//! mmap syscall
//! 还包含页缓存管理器

#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use crate::process::scheduler::processor::*;
use alloc::sync::Arc;
use crate::fs::File;

pub use crate::drivers::block::cache::{
    free_up_mem_space, sync_shared_page_cache, tick_sync, PageCache, SHARED_PAGE_CACHE_MANAGER,
};

// mmap 权限标志
bitflags! {
    pub struct MMapProt: i32 {
        const PROT_NONE  = 0;
        const PROT_READ  = 1 << 0;
        const PROT_WRITE = 1 << 1;
        const PROT_EXEC  = 1 << 2;
    }
}

// mmap 映射类型标志
bitflags! {
    pub struct MMapFlags: i32 {
        const MAP_FILE           = 0;
        const MAP_SHARED         = 0x01;
        const MAP_PRIVATE        = 0x02;
        const MAP_FIXED          = 0x10;
        const MAP_ANONYMOUS      = 0x20;
        // 下面的标志待完善
        const MAP_GROWSDOWN      = 0x0100;
        const MAP_DENYWRITE      = 0x0800;
        const MAP_EXECUTABLE     = 0x1000;
        const MAP_LOCKED         = 0x2000;
        const MAP_NORESERVE      = 0x4000;
        const MAP_POPULATE       = 0x8000;
        const MAP_NONBLOCK       = 0x10000;
        const MAP_STACK          = 0x20000;
        const MAP_HUGETLB        = 0x40000;
        const MAP_SYNC           = 0x80000;
        const MAP_FIXED_NOREPLACE = 0x100000;
    }
}

/// MAP_SHARED_VALIDATE 标志值。等同于 MAP_SHARED|MAP_PRIVATE，
/// 语义为带校验的 MAP_SHARED：设置后内核会验证所有 flag 位是否已知，
/// 存在未知位导致返回 EOPNOTSUPP。
pub const MAP_SHARED_VALIDATE: i32 = 0x03;

/// 修改断点
pub fn do_brk(addr: usize) -> Result<usize, i32> {
    let task = current_processor().current().unwrap();
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize() as i32)?;
    let result = mm.write().change_program_brk(addr);
    result
}

/// 内存映射逻辑
/// 要求调用者已经完成了参数检查
pub fn do_mmap(
    addr: usize, 
    length: usize, 
    prot: MMapProt, 
    flags: MMapFlags,
    file_inner: Option<Arc<dyn File + Send + Sync>>,
    offset: usize,                     
) -> Result<usize, isize> {
    let task = current_processor().current().unwrap();
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize())?;
    let result = mm.write().mmap(addr, length, prot, flags, file_inner, offset);
    result
}

/// 要求调用者已经完成了参数检查
pub fn do_munmap(addr: usize, length: usize) -> Result<(), isize> {
    let task = current_processor().current().unwrap();
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize())?;
    let result = mm.write().munmap(addr, length);
    result
}

pub fn do_madvise_dontneed(addr: usize, length: usize) -> Result<(), isize> {
    let task = current_processor().current().unwrap();
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize())?;
    let result = mm.write().madvise_dontneed(addr, length);
    result
}
