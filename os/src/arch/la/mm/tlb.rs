//! LoongArch TLB ownership tracking and synchronous shootdowns.

use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use core::hint::spin_loop;
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use lazy_static::*;
use spin::Mutex;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct MmKey {
    token: usize,
    asid: usize,
}

#[derive(Clone, Copy)]
struct PendingShootdown {
    key: MmKey,
    generation: usize,
    sequence: usize,
}

struct TlbState {
    active_harts: BTreeMap<MmKey, usize>,
    active_mm: [Option<MmKey>; CPU_CORE_NUM],
    cached_generations: BTreeMap<(usize, MmKey), usize>,
    generations: BTreeMap<MmKey, usize>,
    pending: [Option<PendingShootdown>; CPU_CORE_NUM],
}

impl TlbState {
    fn new() -> Self {
        Self {
            active_harts: BTreeMap::new(),
            active_mm: [None; CPU_CORE_NUM],
            cached_generations: BTreeMap::new(),
            generations: BTreeMap::new(),
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

// One outstanding request per hart is sufficient when shootdowns are serialized.
static SHOOTDOWN_LOCK: Mutex<()> = Mutex::new(());
static NEXT_SEQUENCE: AtomicUsize = AtomicUsize::new(1);
static ACK_SEQUENCE: [AtomicUsize; CPU_CORE_NUM] = [const { AtomicUsize::new(0) }; CPU_CORE_NUM];

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

    crate::arch::mm::flush_tlb_for_asid(pending.key.asid);

    let mut state = TLB_STATE.exclusive_access();
    state
        .cached_generations
        .insert((hart_id, pending.key), pending.generation);
    drop(state);
    ACK_SEQUENCE[hart_id].store(pending.sequence, Ordering::Release);
}

/// Leave user mode before taking any lock that a mapping modifier may hold.
///
/// A shootdown publisher and this transition use the same state lock. Therefore
/// the hart is either included in the request and services it here, or is already
/// inactive and will validate the generation before returning to user mode.
pub fn leave_user_mm() {
    let hart_id = get_hart_id();
    let pending = {
        let mut state = TLB_STATE.exclusive_access();
        state.remove_active_hart(hart_id);
        state.pending[hart_id]
    };
    service_pending(hart_id, pending);
}

/// Service and acknowledge the request represented by the runtime TLB IPI.
pub fn handle_tlb_ipi() {
    let hart_id = get_hart_id();
    let pending = TLB_STATE.exclusive_access().pending[hart_id];
    service_pending(hart_id, pending);
}

/// Prepare `PGDL + ASID` for user mode and mark this hart active in that mm.
///
/// Returning to an unchanged address space keeps its warm TLB. A local ASID
/// invalidation is needed only after an address-space switch or generation change.
pub fn switch_mm(token: usize, asid: usize) {
    let hart_id = get_hart_id();
    let key = MmKey { token, asid };

    handle_tlb_ipi();

    let mut state = TLB_STATE.exclusive_access();
    state.remove_active_hart(hart_id);
    let generation = state.generations.get(&key).copied().unwrap_or(0);
    let cache_is_current =
        state.cached_generations.get(&(hart_id, key)).copied() == Some(generation);
    if !cache_is_current {
        // Keep the state lock across the invalidation and publication. This is
        // the switch-side half of the shootdown/switch serialization protocol.
        crate::arch::mm::flush_tlb_for_asid(asid);
        state.cached_generations.insert((hart_id, key), generation);
    }
    state.active_mm[hart_id] = Some(key);
    *state.active_harts.entry(key).or_insert(0) |= hart_bit(hart_id);
}

/// Invalidate an address space on every hart currently executing it in user mode.
///
/// The function returns only after every target has completed its `invtlb`, so
/// callers may release frames that were reachable through the old PTEs.
pub fn flush_tlb_targets(token: usize, asid: usize) {
    let _shootdown = SHOOTDOWN_LOCK.lock();
    fence(Ordering::SeqCst);

    let key = MmKey { token, asid };
    let sequence = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    assert_ne!(sequence, 0, "LoongArch TLB shootdown sequence wrapped");
    let local_bit = hart_bit(get_hart_id());

    let targets = {
        let mut state = TLB_STATE.exclusive_access();
        let generation = state
            .generations
            .entry(key)
            .and_modify(|generation| {
                *generation = generation
                    .checked_add(1)
                    .expect("LoongArch TLB generation wrapped");
            })
            .or_insert(1);
        let generation = *generation;
        let targets = state.active_harts.get(&key).copied().unwrap_or(0);

        for hart_id in 0..CPU_CORE_NUM {
            if targets & hart_bit(hart_id) != 0 {
                state.pending[hart_id] = Some(PendingShootdown {
                    key,
                    generation,
                    sequence,
                });
            }
        }

        // Publish requests before raising the interrupt. Holding TLB_STATE here
        // closes the race with a target leaving user mode.
        fence(Ordering::Release);
        let remote_targets = targets & !local_bit;
        crate::arch::la::ipi::send_tlb_shootdown(remote_targets);
        targets
    };

    if targets & local_bit != 0 {
        let pending = TLB_STATE.exclusive_access().pending[get_hart_id()];
        service_pending(get_hart_id(), pending);
    }

    let remote_targets = targets & !local_bit;
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

/// Forget cached-generation records when an mm is destroyed. If both its root
/// page and ASID are later reused, every hart treats the pair as a cold switch.
pub fn retire_mm(token: usize, asid: usize) {
    let key = MmKey { token, asid };
    let mut state = TLB_STATE.exclusive_access();
    debug_assert_eq!(state.active_harts.get(&key).copied().unwrap_or(0), 0);
    state.generations.remove(&key);
    state
        .cached_generations
        .retain(|(_, cached_key), _| *cached_key != key);
}
