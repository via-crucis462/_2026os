pub mod pte;
pub mod info;

use core::arch::asm;

pub const PA_WIDTH: usize = 56;
pub const VA_WIDTH: usize = 39;

/// 将内核的物理内存映射到高半地址空间
/// 内核虚拟地址映射窗口
pub const KERNEL_WINDOW_BASE: usize = 0xffff_ffc0_0000_0000;

pub fn flush_tlb_for_asid(asid: usize) {
	unsafe {
		asm!("sfence.vma x0, {asid}", asid = in(reg) asid);
	}
}