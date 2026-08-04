pub mod pte;
pub mod info;

use core::arch::asm;

pub const PA_WIDTH: usize = 56;
pub const VA_WIDTH: usize = 39;

/// 将内核的物理内存映射到高半地址空间
/// 内核虚拟地址映射窗口
pub const KERNEL_WINDOW_BASE: usize = 0xffff_ffc0_0000_0000;

/// 刷新指定 asid 的 tlb
///
/// 参考 linux 的规范，这里的同步关系应该这样处理：
///
/// 1. 如果希望收紧权限，在收紧后刷新本核和其他核的 tlb，保证访问时不会使用旧权限；
/// 2. 如果希望放宽权限，无须刷新，访问时会触发异常自动处理；（缺页和权限异常）
///
/// 3. 解除映射旧页后，释放物理帧前先刷新全部核的相关 tlb；
/// 4. 映射新页可以不刷新 tlb，因为访问时会触发异常自动处理。
///
/// 举例：cow属于 3,4,2 在内存锁中完成的情况，3 后就应该先刷新 tlb，等待其他核完成后再继续进行4和2。
///
pub fn flush_tlb_for_asid(asid: usize) {
	unsafe {
		asm!("sfence.vma x0, {asid}", asid = in(reg) asid);
	}
}

pub fn remote_flush_tlb(cpu_mask: usize, start: usize, size: usize) {
	super::sbi::remote_sfence_vma(cpu_mask, start, size);
}
