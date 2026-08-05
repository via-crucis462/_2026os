//! Constants in the kernel

use super::mm;
pub use mm::info::*;
use mm::KERNEL_WINDOW_BASE;

/// 映射窗口基址，是否带缓存在 rv 下无意义，仅用于和 la 保持一致
/// 
/// 规定为访问设备寄存器或 DMA 缓冲区时使用
pub const UNCACHED_KERNEL_BASE: usize = KERNEL_WINDOW_BASE;
/// 映射窗口基址，是否带缓存在 rv 下无意义，仅用于和 la 保持一致
/// 
/// 规定为访问普通内存时使用
pub const CACHED_KERNEL_BASE: usize = KERNEL_WINDOW_BASE;

#[allow(unused)]
#[cfg(board = "virt")]
pub const CPU_CORE_NUM: usize = 8;
#[cfg(board = "visionfive2")]
pub const CPU_CORE_NUM: usize = 4;

/// page size : 4KB
pub const PAGE_SIZE: usize = 0x1000;
/// page size bits: 12
pub const PAGE_SIZE_BITS: usize = 0xc;

/// user app's stack size
pub const USER_STACK_SIZE: usize = 0x80_0000; // 8MB
/// kernel stack size
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 32; // 128KB
/// kernel heap size
pub const KERNEL_HEAP_SIZE: usize = 0x6000_0000; // 1.5GB

/// the virtual addr of trapoline
pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;
/// the virtual addr of trap context
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
#[cfg(board = "virt")]
/// Platform Timer Device       : aclint-mtimer @ 10000000Hz
pub const CLOCK_FREQ: usize = 10_000_000;
#[cfg(board = "visionfive2")]
pub const CLOCK_FREQ: usize = 4000000;
/// Reserved contiguous memory for DMA-capable virtio devices.
pub const DMA_SIZE: usize = 0x100_0000;

/// 和la同步这个量，不设值
pub const OFFSET_FOR_USER_APP: usize = 0;
pub const USER_APP_BASE: usize = 0x4000_0000;
/// sv39用户地址空间end
pub const USER_APP_MAX_SIZE: usize = 1<<38;
pub const USER_TRAMPOLINE: usize = USER_APP_MAX_SIZE - PAGE_SIZE;

extern "C" {
    fn __call_sig_rt();
}

use lazy_static::lazy_static;
lazy_static! {
    pub static ref SIG_RT_ADDR: usize =
        __call_sig_rt as *const () as usize % PAGE_SIZE + USER_TRAMPOLINE;
}
