//! 龙芯 TLB 刷新机制
//! 
//! 每个 cpu 在 trap 路径维护自己是否“处于用户态”，并在每次回到用户态前都清空本核 TLB，
//! 发送核间tlb刷新请求时，只要观察到所有核都回到过内核态，就可以认为 tlb 刷新请求已经完成。

use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use core::future::Pending;
use core::hint::spin_loop;
use core::sync::atomic::{fence, Ordering};
use lazy_static::*;
use spin::Mutex;

struct ShootdownState {
    /// Harts that may currently execute with user translations.
    user_harts: usize,
    /// Harts that must leave user mode before the current shootdown completes.
    pending_harts: usize,
}

impl ShootdownState {
    const fn new() -> Self {
        Self {
            user_harts: 0,
            pending_harts: 0,
        }
    }
}

lazy_static! {
    static ref SHOOTDOWN_STATE: MPSafeCell<ShootdownState> = MPSafeCell::new(ShootdownState::new());
}

// 保证在同时只有一个核可以发起刷新请求
static SHOOTDOWN_LOCK: Mutex<()> = Mutex::new(());

#[inline]
fn hart_bit(hart_id: usize) -> usize {
    assert!(hart_id < CPU_CORE_NUM);
    1usize << hart_id
}

/// 离开用户态时标记本核已经处理了 tlb 刷新请求
pub fn leave_user_mm() {
    let hart_id = get_hart_id();
    let bit = hart_bit(hart_id);
    let mut state = SHOOTDOWN_STATE.exclusive_access();

    state.user_harts &= !bit;
    if state.pending_harts & bit != 0 {
        crate::arch::mm::flush_user_tlb();
        state.pending_harts &= !bit;
    }
}

/// 进入用户态前标记本核，并刷新所有非全局 TLB 项
pub fn enter_user_mm() {
    let hart_id = get_hart_id();
    let bit = hart_bit(hart_id);
    let mut state = SHOOTDOWN_STATE.exclusive_access();

    // 进行全量刷新
    crate::arch::mm::flush_user_tlb();

    state.user_harts |= bit;
}

/// 刷新所有核的非全局 TLB 项
/// 
/// 等待所有核都回到过内核态后，才认为刷新完成
pub fn flush_tlb_targets() {
    // 同时只允许一个核请求并等待核间刷新
    let _serial = SHOOTDOWN_LOCK.lock();

    // Publish the preceding PTE stores before choosing target harts.
    fence(Ordering::SeqCst);

    let targets = {
        let mut state = SHOOTDOWN_STATE.exclusive_access();
        let local_bit = hart_bit(get_hart_id());

        // 按协议此前的 flush_tlb_targets() 应该已经完成
        assert_eq!(state.pending_harts, 0, "overlapping LoongArch shootdown");
        state.pending_harts = state.user_harts;
        state.pending_harts
    };

    if targets == 0 {
        fence(Ordering::SeqCst);
        return;
    }

    // Make the PTE update and pending bitmap visible before the IOCSR write.
    crate::arch::la::dma_barriar();
    crate::arch::la::ipi::send_tlb_shootdown(targets);

    let mut pending = targets;
    loop {
        pending &= SHOOTDOWN_STATE.exclusive_access().pending_harts;
        if pending == 0 {
            break;
        }
        spin_loop();
    }

    fence(Ordering::SeqCst);
}
