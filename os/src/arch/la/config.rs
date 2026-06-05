//! Constants in the kernel

// LA64可能有所不同，暂时复制riscv的配置

#![allow(unused)]
/// kernel address space
pub const UNCHACHED_KERNEL_BASE: usize = 0x8000_0000_0000_0000;
pub const KERNEL_BASE: usize = 0x9000_0000_0000_0000;

pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;

/// user app's stack size
pub const USER_STACK_SIZE: usize = 0x80_0000; // 8MB
/// kernel stack size
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 16;
/// kernel heap size
pub const KERNEL_HEAP_SIZE: usize = 0x800_0000; // 128MB

/// the virtual addr of trampoline
/// 由于映射窗口的存在，trampoline的地址不需要设置在高位了，直接放在内核空间的末尾就行
/// pub const TRAMPOLINE: usize = (1 << 39) - PAGE_SIZE;
/// the virtual addr of trap context 
/// pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
/// la64需要从cpu寄存器中获取计时器频率，这里先不管
pub const CLOCK_FREQ: usize = 12500000;
/// 为pci设备预留的内存空间大小
pub const DMA_SIZE: usize = 0x100_0000; // 16MB

/// qemu主要内存起始地址, 注意linker.ld需要与此同步
pub const MEMORY_BASE: usize = 0x8000_0000;
/// qemu memory size
pub const MEMORY_SIZE: usize = 1<<30; // 1GB,0x4000_0000
/// the physical memory end
pub const MEMORY_END: usize = MEMORY_BASE + MEMORY_SIZE; // 0xc000_0000

/// 查看qemu的源代码可以知道配置空间的基地址为0x2000_0000，不写成虚拟地址
pub const PCI_CONFIG_SPACE_BASE: usize = 0x2000_0000;
/// MMIO基址设置为0x4000_0000起，改成更低的地址会有问题，ai解释是qemu规定这里开始才是合法的MMIO地址
pub const PCI_MMIO_BASE: usize = 0x4000_0000;
/// MMIO范围，并非真正的内存，cpu尝试访问这些地址相当于给设备发信号
pub const MMIO: &[(usize, usize)] = &[
    (0x8000_0000_1000_0000, 0x1000_0000), // 留给UART
    (0x8000_0000_2000_0000, 0x1000_0000), // PCI配置空间
    (0x8000_0000_4000_0000, 0x1000_0000), // PCI MMIO
]; 

/// 调试用，暂不删除
pub const OFFSET_FOR_USER_APP: usize = 0;
pub const USER_APP_BASE: usize = 0x1_2000_0000;
pub const USER_STACK_TOP: usize = USER_APP_MAX_SIZE;

pub const CPU_CORE_NUM: usize = 4;

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