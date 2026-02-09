//! Constants in the kernel

// LA64可能有所不同，暂时复制riscv的配置

#![allow(unused)]
/// kernel address space
pub const UNCHACHED_KERNEL_BASE: usize = 0x8000_0000_0000_0000;
pub const KERNEL_BASE: usize = 0x9000_0000_0000_0000;

/// user app's stack size
pub const USER_STACK_SIZE: usize = 4096 * 2;
/// kernel stack size
pub const KERNEL_STACK_SIZE: usize = 4096 * 2;
/// kernel heap size
pub const KERNEL_HEAP_SIZE: usize = 0x200_0000;

/// page size : 4KB 
pub const PAGE_SIZE: usize = 0x1000;
/// page size bits: 12
pub const PAGE_SIZE_BITS: usize = 0xc;
/// the virtual addr of trampoline
/// 对用户程序，转成va时会自动置零高半部分，、
/// 但这里/2使其本身就位于低半地址空间，更方便
pub const TRAMPOLINE: usize = usize::MAX / 2 - PAGE_SIZE + 1;
/// the virtual addr of trap context 
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
pub const CLOCK_FREQ: usize = 12500000;
/// the physical memory end
pub const MEMORY_END: usize = 0x9000_0000_0800_0000;

