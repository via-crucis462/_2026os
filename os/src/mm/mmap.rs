#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use crate::task::processor::PROCESSOR;

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
        const MAP_FILE      = 0;
        const MAP_SHARED    = 1 << 0;
        const MAP_PRIVATE   = 1 << 1;
        const MAP_ANONYMOUS = 1 << 2;
        const MAP_FIXED     = 1 << 4;
    }
}

/// 修改断点
pub fn do_brk(addr: usize) -> Result<usize, i32> {
    let task = PROCESSOR.exclusive_access().current().unwrap();
    task.change_program_brk(addr)
}

/// 处理mmap系统调用的分配部分
pub fn do_mmap(addr: usize, length: usize, prot: MMapProt, flags: MMapFlags) -> Result<usize, i32> {
    //println!("do_mmap: addr = {:#x}, length = {}, prot = {:?}", addr, length, prot);
    let task = PROCESSOR.exclusive_access().current().unwrap();
    let ax = task.mmap(addr, length, prot, flags);
    //println!("do_mmap: result = {:#x?}", ax);
    ax
}

pub fn do_munmap(addr: usize, length: usize) -> Result<(), i32> {
    let task = PROCESSOR.exclusive_access().current().unwrap();
    task.munmap(addr, length)
}
// 尽管文件映射在syscall中实现，但此处设置一个shared区域
// （未实现）