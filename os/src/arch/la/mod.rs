pub mod sbi;
pub mod config;
pub mod timer;
pub mod trap;
pub mod mm;
pub mod drivers;
pub mod ipi;
pub mod cpuinfo;

/// DMA/MMIO 访问的完全内存屏障
/// 
/// 保证编译期顺序和CPU顺序完全与代码的访问顺序一致：
/// - 在此函数上方的所有内存访问在此函数前完成
/// - 在此函数下方的所有内存访问在此函数后开始
#[inline(always)]
pub fn dma_barriar() {
    use core::sync::atomic::{compiler_fence, Ordering};

    compiler_fence(Ordering::SeqCst);
    unsafe {
        core::arch::asm!("dbar 0", options(nostack, preserves_flags));
    }
    compiler_fence(Ordering::SeqCst);
}
