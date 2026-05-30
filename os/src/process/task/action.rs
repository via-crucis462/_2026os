use crate::task::{SignalFlags, MAX_SIG};

/// Action for a signal
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct SignalAction {
    /// 信号处理函数地址
    pub handler: usize,
    /// 信号标志位
    pub flags: usize,
    // riscv/loongarch 无此字段，火箭队直接注释废物了
    // 会导致glibc ltp跑完，收尾阶段炸掉
    // 原因是restorer原本的位置是mask应该的位置，ra被写了个mask进去
    // pub restorer: usize,
    /// 信号掩码
    pub mask: SignalFlags, 
}

impl Default for SignalAction {
    fn default() -> Self {
        Self {
            handler: 0,
            flags: 0,
            mask: SignalFlags::from_bits(0).unwrap(),
        }
    }
}

/// Signal actions
#[derive(Clone)]
pub struct SignalActions {
    /// Signal actions table
    pub table: [SignalAction; MAX_SIG + 1],
}

impl Default for SignalActions {
    fn default() -> Self {
        Self {
            table: [SignalAction::default(); MAX_SIG + 1],
        }
    }
}
