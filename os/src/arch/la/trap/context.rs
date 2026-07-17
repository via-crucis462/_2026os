//! 按照loongarch64架构修改
#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// 复用riscv的设计，小幅度修改
pub struct TrapContext {
    /// General-Purpose Register x0-31
    pub r: [usize; 32],
    /// prmd, trap前状态寄存器, 和riscv不同，
    /// 保存的是上次trap前而非当前的状态
    prmd: usize,
    /// era, trap返回后下一步执行的地址
    era: usize,
}

// 封装了对两平台名称不同寄存器的访问为同名接口
impl TrapContext {
    /// 创建一个空TC
    pub fn new_bare() -> Self {
        Self {
            r: [0; 32],
            prmd: 0,
            era: 0,
        }
    }
    /// 将sp存入r3
    pub fn set_sp(&mut self, sp: usize) {
        self.r[3] = sp;
    }
    pub fn get_sp(&self) -> usize {
        self.r[3]
    }
    /// 设置返回值，a0对应r4
    pub fn set_a0(&mut self, a0: usize) {
        self.r[4] = a0;
    }
    pub fn set_ra(&mut self, ra: usize) {
        self.r[1] = ra;
    }
    pub fn set_a1(&mut self, a1: usize) {
        self.r[5] = a1;
    }
    pub fn set_a2(&mut self, a2: usize) {
        self.r[6] = a2;
    }
    /// 获取返回值
    pub fn get_a0(&self) -> usize {
        self.r[4]
    }
    pub fn get_a1(&self) -> usize {
        self.r[5]
    }
    /// 设置trap返回地址
    pub fn set_rt(&mut self, era: usize) {
        self.era = era;
    }
    /// 获取trap返回地址
    pub fn get_rt(&self) -> usize {
        self.era
    }
    /// init the trap context of an application
    pub fn app_init_context(
        entry: usize,
        sp: usize,
        _kernel_token: usize,
        _kernel_sp: usize,
        _trap_handler: usize,
    ) -> Self {
        debug!(
            "app_init_context: entry=0x{:x}, sp=0x{:x}", 
            entry, sp
        );
        // app启动需设置特权级为用户态，也就是plv=3
        // 另，开启分页在这里是无效的，高位是保留位
        // 注意，_restore函数会将prmd的值写入prmd寄存器，然后才ertn，所以不应该设置到crmd寄存器中，否则出问题
        let default_status: usize  = 0b0000_0111; 
        
        let mut cx = Self {
            r: [0; 32],
            prmd: default_status,
            era: entry,  // entry point of app
        };
        cx.set_sp(sp); // app's user stack pointer
        cx // return initial Trap Context of app
    }
     
}
