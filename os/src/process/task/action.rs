use crate::task::{SignalFlags, MAX_SIG};

/// Action for a signal
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct SignalAction {
    /// 信号处理函数地址 (8 bytes)
    pub handler: usize,
    /// 信号标志位 (8 bytes)
    pub flags: usize,
    /// 蹦床函数地址 (8 bytes)
    pub restorer: usize,
    /// 信号掩码 (8 bytes)
    pub mask: SignalFlags, 
}
impl Default for SignalAction {
    fn default() -> Self {
        Self {
            handler: 0,
            flags: 0,
            mask: SignalFlags::from_bits(40).unwrap(),
            restorer: 0,
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
