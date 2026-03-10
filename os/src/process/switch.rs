//!Wrap `switch.S` as a function
use super::TaskContext;
use core::arch::global_asm;

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("../arch/riscv/task/switch.S"));
#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("../arch/la/task/switch.S"));

extern "C" {
    pub fn __switch(current_task_cx_ptr: *mut TaskContext, next_task_cx_ptr: *const TaskContext);
}
