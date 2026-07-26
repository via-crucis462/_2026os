use bitflags::*;
use alloc::vec::Vec;
use alloc::sync::Weak;
use crate::process::task::{Rlimits, TaskControlBlock};
use super::{SignalAction, SignalActions};

pub const MAX_SIG: usize = 64;

bitflags! {
    pub struct SignalFlags: u64 {
        const SIGHUP = 1 << 0; const SIGINT = 1 << 1; const SIGQUIT = 1 << 2;
        const SIGILL = 1 << 3; const SIGTRAP = 1 << 4; const SIGABRT = 1 << 5;
        const SIGBUS = 1 << 6; const SIGFPE = 1 << 7; const SIGKILL = 1 << 8;
        const SIGUSR1 = 1 << 9; const SIGSEGV = 1 << 10; const SIGUSR2 = 1 << 11;
        const SIGPIPE = 1 << 12; const SIGALRM = 1 << 13; const SIGTERM = 1 << 14;
        const SIGSTKFLT = 1 << 15; const SIGCHLD = 1 << 16; const SIGCONT = 1 << 17;
        const SIGSTOP = 1 << 18; const SIGTSTP = 1 << 19; const SIGTTIN = 1 << 20;
        const SIGTTOU = 1 << 21; const SIGURG = 1 << 22; const SIGXCPU = 1 << 23;
        const SIGXFSZ = 1 << 24; const SIGVTALRM = 1 << 25; const SIGPROF = 1 << 26;
        const SIGWINCH = 1 << 27; const SIGIO = 1 << 28; const SIGPWR = 1 << 29;
        const SIGSYS = 1 << 30; const SIGRTMIN = 1 << 31; const SIGRT_33 = 1 << 32;
        const SIGRT_34 = 1 << 33; const SIGRT_35 = 1 << 34; const SIGRT_36 = 1 << 35;
        const SIGRT_37 = 1 << 36; const SIGRT_38 = 1 << 37; const SIGRT_39 = 1 << 38;
        const SIGRT_40 = 1 << 39; const SIGRT_41 = 1 << 40; const SIGRT_42 = 1 << 41;
        const SIGRT_43 = 1 << 42; const SIGRT_44 = 1 << 43; const SIGRT_45 = 1 << 44;
        const SIGRT_46 = 1 << 45; const SIGRT_47 = 1 << 46; const SIGRT_48 = 1 << 47;
        const SIGRT_49 = 1 << 48; const SIGRT_50 = 1 << 49; const SIGRT_51 = 1 << 50;
        const SIGRT_52 = 1 << 51; const SIGRT_53 = 1 << 52; const SIGRT_54 = 1 << 53;
        const SIGRT_55 = 1 << 54; const SIGRT_56 = 1 << 55; const SIGRT_57 = 1 << 56;
        const SIGRT_58 = 1 << 57; const SIGRT_59 = 1 << 58; const SIGRT_60 = 1 << 59;
        const SIGRT_61 = 1 << 60; const SIGRT_62 = 1 << 61; const SIGRT_63 = 1 << 62;
        const SIGRTMAX = 1 << 63;
    }
}
impl SignalFlags {
    pub fn check_error(&self) -> Option<(i32, &'static str)> {
        if self.contains(Self::SIGINT) { Some((-2, "Killed, SIGINT=2")) }
        else if self.contains(Self::SIGILL) { Some((-4, "Illegal Instruction, SIGILL=4")) }
        else if self.contains(Self::SIGABRT) { Some((-6, "Aborted, SIGABRT=6")) }
        else if self.contains(Self::SIGFPE) { Some((-8, "Erroneous Arithmetic Operation, SIGFPE=8")) }
        else if self.contains(Self::SIGKILL) { Some((-9, "Killed, SIGKILL=9")) }
        else if self.contains(Self::SIGSEGV) { Some((-11, "Segmentation Fault, SIGSEGV=11")) }
        else { None }
    }
    pub fn number(&self) -> Option<usize> {
        for i in 0..=MAX_SIG {
            if self.contains(SignalFlags::from_bits(1 << i).unwrap()) { return Some(i + 1); }
        }
        None
    }
}
#[derive(Clone)]
pub struct Sigpending { queue: Vec<usize>, signals: SignalFlags, wait_chldexit: bool }
impl Sigpending {
    pub fn new() -> Self { Self { queue: Vec::new(), signals: SignalFlags::empty(), wait_chldexit: false } }
    pub fn insert(&mut self, signal: SignalFlags) { self.signals.insert(signal); if let Some(n) = signal.number() { self.queue.push(n); } }
    pub fn bits(&self) -> u64 { self.signals.bits() }
    pub fn contains(&self, signal: SignalFlags) -> bool { self.signals.contains(signal) }
    pub fn remove(&mut self, signal: SignalFlags) { self.signals.remove(signal); if let Some(n) = signal.number() { self.queue.retain(|q| *q != n); } }
    pub fn flags(&self) -> SignalFlags { self.signals }
}
pub struct Signal {
    shared_pending: Sigpending, group_exit_state: i32, thread_num: usize,
    pub next_thread: Option<Weak<TaskControlBlock>>, rlimits: Rlimits,
}
impl Signal {
    pub fn new() -> Self { Self { shared_pending: Sigpending::new(), group_exit_state: 0, thread_num: 1, next_thread: None, rlimits: Rlimits::new() } }
    pub fn fork_from(parent: &Self) -> Self { Self { shared_pending: Sigpending::new(), group_exit_state: 0, thread_num: 1, next_thread: None, rlimits: parent.rlimits.clone() } }
    pub fn add_thread(&mut self) { self.thread_num += 1; }
    pub fn insert_pending(&mut self, signal: SignalFlags) { self.shared_pending.insert(signal); }
    pub fn pending_flags(&self) -> SignalFlags { self.shared_pending.flags() }
    pub fn remove_pending(&mut self, signal: SignalFlags) { self.shared_pending.remove(signal); }
    pub fn rlimits(&self) -> &Rlimits { &self.rlimits }
    pub fn rlimits_mut(&mut self) -> &mut Rlimits { &mut self.rlimits }
}
#[derive(Clone)]
pub struct SigHand { sig_actions: SignalActions }
impl SigHand {
    pub fn new() -> Self { Self { sig_actions: SignalActions::new() } }
    pub fn action(&self, signal_index: usize) -> SignalAction { self.sig_actions.table[signal_index] }
    pub fn set_action(&mut self, signal_index: usize, action: SignalAction) { self.sig_actions.table[signal_index] = action; }
}
