pub mod pte;

use core::arch::asm;

pub const PA_WIDTH: usize = 56;
pub const VA_WIDTH: usize = 39;

pub fn flush_tlb_for_asid(asid: usize) {
	unsafe {
		asm!("sfence.vma x0, {asid}", asid = in(reg) asid);
	}
}