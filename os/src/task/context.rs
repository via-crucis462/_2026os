//! Implementation of [`TaskContext`]/
use crate::arch::trap::trap_return;

#[repr(C)]
/// task context structure containing some registers
pub struct TaskContext {
    /// Ret position after task switching
    pub ra: usize,
    /// Stack pointer
    pub sp: usize,
    /// s0-11 register, callee saved
    pub s: [usize; 12],// la64只有s0-s9，但不单独定义，牺牲一点空间换取简洁
}

impl TaskContext {
    /// Create a new empty task context
    pub fn zero_init() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }
    /// Create a new task context with a trap return addr and a kernel stack pointer
    pub fn goto_trap_return(kstack_ptr: usize) -> Self {
        //println!("goto_trap_return: kstack_ptr={:#x}", kstack_ptr);
        Self {
            ra: trap_return as *const () as usize,
            sp: kstack_ptr,
            s: [0; 12],
        }
    }
}
