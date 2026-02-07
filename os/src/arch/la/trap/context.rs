//! 按照loongarch64架构修改

use core::arch::asm;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// 模仿riscv设计
pub struct TrapContext {
    /// General-Purpose Register x0-31
    pub x: [usize; 32],
    // prmd, trap前状态寄存器
    pub sstatus: usize,
    /// era, 返回地址
    pub sepc: usize,
    /// Token of kernel address space
    pub kernel_token: usize,
    /// Kernel stack pointer of the current application
    pub kernel_sp: usize,
    /// Virtual address of trap handler entry point in kernel
    pub trap_handler: usize,
}

impl TrapContext {
    /// put the sp(stack pointer) into r[2] field of TrapContext
    pub fn set_sp(&mut self, sp: usize) {
        self.x[2] = sp;
    }
    /// init the trap context of an application
    pub fn app_init_context(
        entry: usize,
        sp: usize,
        kernel_token: usize,
        kernel_sp: usize,
        trap_handler: usize,
    ) -> Self {
        let mut _c = 0;
        // set CPU privilege to User after trapping back
        unsafe {
            asm!("# TODO");
        }
        let mut cx = Self {
            x: [0; 32],
            sstatus: 0,
            sepc: entry,  // entry point of app
            kernel_token,  // addr of page table
            kernel_sp,    // kernel stack
            trap_handler, // addr of trap_handler function
        };
        cx.set_sp(sp); // app's user stack pointer
        cx // return initial Trap Context of app
    }
     
}
