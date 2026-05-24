//! Implementation of [`TrapContext`]
use riscv::register::sstatus::{self, Sstatus, SPP, FS};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// trap context structure containing sstatus, sepc and registers
pub struct TrapContext {
    /// General-Purpose Register x0-31
    pub x: [usize; 32],
    /// Supervisor Status Register
    sstatus: Sstatus,
    /// Supervisor Exception Program Counter
    pub sepc: usize,
    /// Token of kernel address space
    pub kernel_satp: usize,
    /// Kernel stack pointer of the current application
    pub kernel_sp: usize,
    /// Virtual address of trap handler entry point in kernel
    pub trap_handler: usize,

    pub hart_id: usize, // 保存当前线程所在核的id
}


// 封装了对两平台名称不同寄存器的访问为同名接口
impl TrapContext {
    /// put the sp(stack pointer) into x\[2\] field of TrapContext
    pub fn set_sp(&mut self, sp: usize) {
        self.x[2] = sp;
    }
    pub fn get_sp(&self) -> usize {
        self.x[2]
    }
    /// 设置返回值，a0对应x10
    pub fn set_a0(&mut self, a0: usize) {
        self.x[10] = a0;
    }
    pub fn set_ra(&mut self, ra: usize) {
        self.x[1] = ra; // RISC-V 中 x1 是 Return Address
    }
    pub fn set_a1(&mut self, a1: usize) {
        self.x[11] = a1;
    }
    /// 获取返回值
    pub fn get_a0(&self) -> usize {
        self.x[10]
    }
    pub fn get_a1(&self) -> usize {
        self.x[11]
    }
    /// 设置trap返回地址
    pub fn set_rt(&mut self, sepc: usize) {
        self.sepc = sepc;
    }
    /// 获取trap返回地址
    pub fn get_rt(&self) -> usize {
        self.sepc
    }
    /// init the trap context of an application
    pub fn app_init_context(
        entry: usize,
        sp: usize,
        kernel_satp: usize,
        kernel_sp: usize,
        trap_handler: usize,
    ) -> Self {
        unsafe {

            sstatus::set_fs(FS::Clean); 

            let mut sstatus = sstatus::read();
 
            sstatus.set_spp(SPP::User); 

            let mut cx = Self {
                x: [0; 32],
                sstatus,
                sepc: entry,
                kernel_satp,
                kernel_sp,
                trap_handler,
                hart_id: 0,// 在__restore时由tp写入，trap时恢复到tp
            };
            cx.set_sp(sp);
            cx
        }
    }
}
