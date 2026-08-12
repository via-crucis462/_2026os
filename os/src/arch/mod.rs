#![allow(dead_code)]

#[cfg(target_arch = "riscv64")]
pub mod riscv;
#[cfg(target_arch = "loongarch64")]
pub mod la;

#[cfg(target_arch = "riscv64")]
pub use riscv::*;
#[cfg(target_arch = "loongarch64")]
pub use la::*;

/// Architecture-neutral idle-wakeup IPI facade.
///
/// The scheduler publishes all state before using this best-effort kick.  The
/// implementation is deliberately tiny so enqueue code never depends on
/// architecture-specific IPI vector details.
pub mod ipi {
	#[inline]
	pub fn send_scheduler_ipi(cpu: usize) {
		#[cfg(target_arch = "riscv64")]
		crate::arch::riscv::ipi::send_scheduler_ipi(cpu);
		#[cfg(target_arch = "loongarch64")]
		crate::arch::la::ipi::send_scheduler_ipi(cpu);
	}

	#[cfg(target_arch = "riscv64")]
	#[inline]
	pub fn init_runtime_ipi() {
		crate::arch::riscv::ipi::init_runtime_ipi();
	}

	#[cfg(target_arch = "riscv64")]
	#[inline]
	pub fn acknowledge_scheduler_ipi() {
		crate::arch::riscv::ipi::acknowledge_scheduler_ipi();
	}

	#[cfg(target_arch = "loongarch64")]
	#[inline]
	pub fn acknowledge_scheduler_ipi() {
		// LoongArch uses an IPI only as a request to return to the kernel.  The
		// user TLB is flushed before every return to userspace, so all action
		// bits observed after leaving idle may be acknowledged together.
		let _ = crate::arch::la::ipi::take_ipi_actions();
	}
}

// 其他架构
#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
compile_error!("Unsupported target arch");
