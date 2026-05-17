#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use crate::task::processor::*;

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
        const MAP_SHARED    = 0x01;
        const MAP_PRIVATE   = 0x02;
        const MAP_FIXED     = 0x10;
        const MAP_ANONYMOUS = 0x20;
    }
}

/// 修改断点
pub fn do_brk(addr: usize) -> Result<usize, i32> {
    let task = current_processor().current().unwrap();
    let proc = task.process();
    proc.change_program_brk(addr)
}

/// 处理mmap系统调用的分配部分
pub fn do_mmap(addr: usize, length: usize, prot: MMapProt, flags: MMapFlags) -> Result<usize, i32> {
    //println!("do_mmap: addr = {:#x}, length = {}, prot = {:?}", addr, length, prot);
    let task = current_processor().current().unwrap();
    let proc = task.process();
    proc.mmap(addr, length, prot, flags)
}

pub fn do_munmap(addr: usize, length: usize) -> Result<(), i32> {
    let task = current_processor().current().unwrap();
    let proc = task.process();
    proc.munmap(addr, length)
}
// 尽管文件映射在syscall中实现，但此处设置一个shared区域
// （未实现）