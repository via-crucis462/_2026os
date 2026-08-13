//! LoongArch TLB ownership tracking and synchronous shootdowns.

use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use core::hint::spin_loop;
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use lazy_static::*;
use spin::Mutex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MmKey {
    token: usize,
    asid: usize,
}

#[derive(Clone, Copy)]
struct PendingShootdown {
    key: MmKey,
    sequence: usize,
}

struct TlbState {
    active_harts: BTreeMap<MmKey, usize>,
    active_mm: [Option<MmKey>; CPU_CORE_NUM],
    pending: [Option<PendingShootdown>; CPU_CORE_NUM],
}

impl TlbState {
    fn new() -> Self {
        Self {
            active_harts: BTreeMap::new(),
            active_mm: [None; CPU_CORE_NUM],
            pending: [None; CPU_CORE_NUM],
        }
    }

    fn remove_active_hart(&mut self, hart_id: usize) {
        let Some(key) = self.active_mm[hart_id].take() else {
            return;
        };
        if let Some(harts) = self.active_harts.get_mut(&key) {
            *harts &= !hart_bit(hart_id);
            if *harts == 0 {
                self.active_harts.remove(&key);
            }
        }
    }
}

lazy_static! {
    static ref TLB_STATE: MPSafeCell<TlbState> = MPSafeCell::new(TlbState::new());
}

// A hart has one pending slot, so serialize publishers until all targets ack.
static SHOOTDOWN_LOCK: Mutex<()> = Mutex::new(());
static NEXT_SEQUENCE: AtomicUsize = AtomicUsize::new(1);
static ACK_SEQUENCE: [AtomicUsize; CPU_CORE_NUM] = [const { AtomicUsize::new(0) }; CPU_CORE_NUM];

#[inline]
fn hart_bit(hart_id: usize) -> usize {
    assert!(hart_id < CPU_CORE_NUM);
    1usize << hart_id
}

fn service_pending(hart_id: usize, pending: Option<PendingShootdown>) {
    let Some(pending) = pending else {
        return;
    };
    if ACK_SEQUENCE[hart_id].load(Ordering::Acquire) == pending.sequence {
        return;
    }
    debug_assert_eq!(
        TLB_STATE.exclusive_access().pending[hart_id].map(|request| request.key),
        Some(pending.key)
    );

    // The hart may have entered the kernel for a timer/syscall just before the
    // publisher delivered this action. Clear it here as well as at trap entry,
    // so an ack always means the reusable level-triggered bit is deasserted.
    crate::arch::la::ipi::take_ipi_actions();
    // trap_return flushes again before user mode. This flush is the one that
    // permits the mapping modifier to release frames immediately after the ack.
    crate::arch::mm::flush_user_tlb();
    fence(Ordering::SeqCst);

    let mut state = TLB_STATE.exclusive_access();
    if state.pending[hart_id].map(|request| request.sequence) == Some(pending.sequence) {
        state.pending[hart_id] = None;
    }
    drop(state);
    ACK_SEQUENCE[hart_id].store(pending.sequence, Ordering::Release);
}

/// Mark this hart inactive before taking any page-table or address-space lock.
pub fn leave_user_mm() {
    let hart_id = get_hart_id();
    let pending = {
        let mut state = TLB_STATE.exclusive_access();
        state.remove_active_hart(hart_id);
        state.pending[hart_id]
    };
    service_pending(hart_id, pending);
}

/// Flush and publish the address space that this hart is about to enter.
///
/// Keeping the state lock across the flush and publication closes the race with
/// a concurrent mapping modifier: the hart is either included in its target
/// set, or observes the changed page table before entering user mode.
pub fn enter_user_mm(token: usize, asid: usize) {
    let hart_id = get_hart_id();
    let key = MmKey { token, asid };
    let pending = {
        let mut state = TLB_STATE.exclusive_access();
        state.remove_active_hart(hart_id);
        let pending = state.pending[hart_id];
        if pending.is_none() {
            crate::arch::mm::flush_user_tlb();
            state.active_mm[hart_id] = Some(key);
            *state.active_harts.entry(key).or_insert(0) |= hart_bit(hart_id);
        }
        pending
    };

    if pending.is_some() {
        service_pending(hart_id, pending);
        let mut state = TLB_STATE.exclusive_access();
        crate::arch::mm::flush_user_tlb();
        state.active_mm[hart_id] = Some(key);
        *state.active_harts.entry(key).or_insert(0) |= hart_bit(hart_id);
    }
}

/// Invalidate this address space on every hart currently executing it.
pub fn flush_tlb_targets(token: usize, asid: usize) {
    let _shootdown = SHOOTDOWN_LOCK.lock();
    fence(Ordering::SeqCst);

    let key = MmKey { token, asid };
    let sequence = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    assert_ne!(sequence, 0, "LoongArch TLB shootdown sequence wrapped");
    let local_bit = hart_bit(get_hart_id());

    let targets = {
        let mut state = TLB_STATE.exclusive_access();
        let targets = state.active_harts.get(&key).copied().unwrap_or(0);
        for hart_id in 0..CPU_CORE_NUM {
            if targets & hart_bit(hart_id) != 0 {
                debug_assert!(state.pending[hart_id].is_none());
                state.pending[hart_id] = Some(PendingShootdown { key, sequence });
            }
        }
        let remote_targets = targets & !local_bit;
        if remote_targets != 0 {
            // The blocking IOCSR write completes once the target status bit is
            // set; it does not wait for the target hart to handle the IPI.
            // Keeping the state lock here guarantees that a hart cannot ack a
            // request before its matching hardware action has been delivered.
            fence(Ordering::Release);
            crate::arch::la::dma_barriar();
            crate::arch::la::ipi::send_tlb_shootdown(remote_targets);
        }
        targets
    };

    if targets == 0 {
        return;
    }

    let remote_targets = targets & !local_bit;
    if targets & local_bit != 0 {
        let pending = TLB_STATE.exclusive_access().pending[get_hart_id()];
        service_pending(get_hart_id(), pending);
    }

    for hart_id in 0..CPU_CORE_NUM {
        if remote_targets & hart_bit(hart_id) == 0 {
            continue;
        }
        while ACK_SEQUENCE[hart_id].load(Ordering::Acquire) != sequence {
            spin_loop();
        }
    }
    fence(Ordering::SeqCst);
}

/// Remove tracking records before a token/ASID pair can be reused.
pub fn retire_mm(token: usize, asid: usize) {
    let key = MmKey { token, asid };
    let mut state = TLB_STATE.exclusive_access();
    debug_assert_eq!(state.active_harts.get(&key).copied().unwrap_or(0), 0);
    state.active_harts.remove(&key);
}
