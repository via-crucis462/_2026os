//! Constants in the kernel

#[allow(unused)]
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
pub const KERNEL_HEAP_SIZE: usize = 0x800_0000; // 128MB


/// the virtual addr of trapoline
pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;
/// the virtual addr of trap context
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
pub const CLOCK_FREQ: usize = 12500000;
/// riscv qemu主要内存起始地址, 注意linker.ld需要与此同步
pub const MEMORY_BASE: usize = 0x8000_0000;
/// qemu memory size
pub const MEMORY_SIZE: usize = 1<<30; // 1GB,0x4000_0000
/// the physical memory end
pub const MEMORY_END: usize = MEMORY_BASE + MEMORY_SIZE; // 0xc000_0000
/// 这里也定义一个
pub const DMA_SIZE: usize = 0;
/// The base address of control registers in Virtio_Block device
pub const MMIO: &[(usize, usize)] = &[
    (0x10001000, 0x8000), // Virtio Block
    (0x10_1000, 0x1000), // 🌟 新增：Goldfish RTC
];

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
    pub static ref SIG_RT_ADDR: usize = __call_sig_rt as *const () as usize % PAGE_SIZE + USER_TRAMPOLINE;
}