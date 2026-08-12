//! Runtime inter-processor interrupts used by the scheduler.
//!
//! SBI delivers this notification as an S-mode software interrupt (SSIP).
//! The receiver acknowledgement deliberately stays here, separate from the
//! scheduler's lock-free pending-bit handling.

use core::arch::asm;

/// Request that `cpu` leave scheduler WFI.
///
/// The caller publishes the runnable task and the scheduler pending state
/// before invoking this function.  SBI may coalesce notifications, therefore
/// correctness must rely on that published state rather than on IPI counts.
#[inline]
pub fn send_scheduler_ipi(cpu: usize) {
    if cpu < crate::arch::config::CPU_CORE_NUM {
        let _ = crate::arch::sbi::sbi_wakeup_hart(cpu);
    }
}

/// Enable supervisor software interrupts on the local hart.
#[inline]
pub fn init_runtime_ipi() {
    unsafe {
        riscv::register::sie::set_ssoft();
    }
    if !crate::arch::sbi::init_scheduler_ipi() {
        warn!("[sched] SBI IPI extension unavailable; remote wakeups wait for the next timer interrupt");
    }
}

/// Acknowledge the local SBI software interrupt.
///
/// SSIP is writable from S-mode on the platforms supported by this kernel.
/// Use an inline CSR clear because the `riscv` crate intentionally exposes
/// `sip` as read-only on some targets.
#[inline]
pub fn acknowledge_scheduler_ipi() {
    unsafe {
        asm!("csrc sip, {ssip}", ssip = const (1usize << 1), options(nostack));
    }
}
