use super::{SignalFlags, MAX_SIG};

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct SignalAction {
    pub handler: usize,
    pub flags: usize,
    pub mask: SignalFlags,
}
impl Default for SignalAction {
    fn default() -> Self {
        Self { handler: 0, flags: 0, mask: SignalFlags::from_bits(0).unwrap() }
    }
}
#[derive(Clone)]
pub struct SignalActions { pub table: [SignalAction; MAX_SIG] }
impl SignalActions {
    pub fn new() -> Self { Self { table: [SignalAction::default(); MAX_SIG] } }
}
