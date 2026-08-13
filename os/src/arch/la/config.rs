//! Constants in the kernel

#![allow(unused)]


pub use super::mm::info::*;


/// kernel address space
pub const UNCACHED_KERNEL_BASE: usize = 0x8000_0000_0000_0000;
pub const CACHED_KERNEL_BASE: usize = 0x9000_0000_0000_0000;
/// 可能的映射窗口掩码
pub const WINDOW_MASK: usize = 0xF000_0000_0000_0000;

pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;

/// User stack virtual reservation. Physical pages and page-table leaves are
/// created on demand by the ordinary VMA fault path.
pub const USER_STACK_SIZE: usize = 0x8000_0000; // 2 GiB
/// kernel stack size
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 16;
/// kernel heap size
pub const KERNEL_HEAP_SIZE: usize = 0x6000_0000; // 1.5GiB

/// the virtual addr of trampoline
/// 由于映射窗口的存在，trampoline的地址不需要设置在高位了，直接放在内核空间的末尾就行
/// pub const TRAMPOLINE: usize = (1 << 39) - PAGE_SIZE;
/// the virtual addr of trap context 
/// pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;

pub const CLOCK_FREQ: usize = 100000000;

const fn parse_cpu_num(value: &str) -> usize {
    let bytes = value.as_bytes();
    assert!(!bytes.is_empty(), "CPU_NUM must not be empty");
    let mut cpu_num = 0;
    let mut index = 0;
    while index < bytes.len() {
        assert!(bytes[index].is_ascii_digit(), "CPU_NUM must be a decimal integer");
        cpu_num = cpu_num * 10 + (bytes[index] - b'0') as usize;
        index += 1;
    }
    assert!(cpu_num > 0, "CPU_NUM must be greater than zero");
    assert!(cpu_num <= usize::BITS as usize, "CPU_NUM exceeds the CPU mask width");
    cpu_num
}

pub const CPU_CORE_NUM: usize = match option_env!("CPU_NUM") {
    Some(value) => parse_cpu_num(value),
    None => 1,
};

/// 为 DMA 设备预留的内存空间大小
pub const DMA_SIZE: usize = 0x100_0000; // 16MB

#[cfg(board = "virt")]
pub const UART_PHYS: usize = 0x1fe001e0;
#[cfg(board = "2k1000")]
pub const UART_PHYS: usize = 0x1fe20000;

/// 可用内存分为两部分
/// 
/// 给内核栈使用
pub const LOWRAM_BASE: usize = BANK0_START_EFFECTIVE;
pub const LOWRAM_END: usize = BANK0_END_EFFECTIVE;
/// 内核和用户帧分配使用
/// 主要内存起始地址, 注意linker.ld需要与此同步
pub const MEMORY_BASE: usize = BANK1_START_EFFECTIVE;
/// the physical memory end
pub const MEMORY_END: usize = BANK1_END_EFFECTIVE;
/// 可用主内存大小
pub const MEMORY_SIZE: usize = BANK1_END_EFFECTIVE - BANK1_START_EFFECTIVE;

/// PCI 配置空间 / MMIO 相关常量已移至 arch/la/mm/info/{lavirt,la2k1000}.rs
/// 经由 pub use super::mm::info::* 按板级条件编译引入

/// 调试用，暂不删除
pub const OFFSET_FOR_USER_APP: usize = 0;
pub const USER_APP_BASE: usize = 0x1_2000_0000;
pub const USER_STACK_TOP: usize = USER_APP_MAX_SIZE;

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
