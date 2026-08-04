//! RISC-V TLB ownership tracking for address-space shootdowns.

use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;

struct SatpActive {
    token_harts: BTreeMap<usize, usize>,
    hart_tokens: [Option<usize>; CPU_CORE_NUM],
}

impl SatpActive {
    fn new() -> Self {
        Self {
            token_harts: BTreeMap::new(),
            hart_tokens: [None; CPU_CORE_NUM],
        }
    }
}

lazy_static! {
    /// 指定 token 地址空间活跃 cpu 位图表
    /// 
    /// 拿到这个表的锁的核才能进行地址空间切换，
    /// 因此表内方法可以视为对于多核 satp 寄存器状态是原子的。
    /// 
    /// 每次切换 satp 先标记目标 satp 本核活跃，
    /// 刷新 tlb 并切换 asid，再移除旧 satp 中的本核。
    /// 这样做能确保某核完成页表修改，希望刷新指定 satp 时，
    /// 在拿到锁获取刷新集合的瞬间，临界核或者已经自己刷过 tlb 切走，
    /// 或者刚拿到该 satp（tlb还是空的，也相当于刷了一次）。
    /// 而非临界核则根据活跃 cpu 集合正常失效相关表项。
    /// 这能够保证：**在发起请求的核看来所有 cpu 的相关 tlb 项，
    /// 在刷新请求完成后，总是在获取到目标 cpu 集的锁的瞬间之后重新填充**
    static ref SATP_ACTIVE: MPSafeCell<SatpActive> = MPSafeCell::new(SatpActive::new());
}

fn hart_bit(hart_id: usize) -> usize {
    assert!(hart_id < CPU_CORE_NUM);
    1usize << hart_id
}

/// Return a stable snapshot of harts currently running `token`.
pub fn running_harts(token: usize) -> usize {
    SATP_ACTIVE
        .exclusive_access()
        .token_harts
        .get(&token)
        .copied()
        .unwrap_or(0)
}

/// Return a snapshot of every active token and its running-hart bitset.
pub fn active_tokens() -> Vec<(usize, usize)> {
    SATP_ACTIVE
        .exclusive_access()
        .token_harts
        .iter()
        .map(|(token, harts)| (*token, *harts))
        .collect()
}

/// 切换至 token 根页表并完成活跃 cpu 的更新
/// 
/// 同步性描述见 SATP_ACTIVE 的注释
pub fn switch_mm(token: usize) {
    let hart_id = get_hart_id();
    let bit = hart_bit(hart_id);
    let old_token = {
        let mut active = SATP_ACTIVE.exclusive_access();
        let old_token = active.hart_tokens[hart_id];
        if old_token == Some(token) {
            return;
        }
        *active.token_harts.entry(token).or_insert(0) |= bit;
        old_token
    };

    unsafe {
        // qemu 会自动刷新 tlb
        asm!("csrw satp, {token}", token = in(reg) token, options(nostack));
        #[cfg(not(board = "virt"))]
        asm!("sfence.vma x0, x0", options(nostack));
    }

    let mut active = SATP_ACTIVE.exclusive_access();
    if let Some(old_token) = old_token {
        let old_harts = active
            .token_harts
            .get_mut(&old_token)
            .expect("active token missing during switch_mm");
        *old_harts &= !bit;
        if *old_harts == 0 {
            active.token_harts.remove(&old_token);
        }
    }
    active.hart_tokens[hart_id] = Some(token);
}

/// 移除一个已经销毁的 token 的活跃 cpu 集合
/// 
/// 会确认没有 cpu 仍然活跃在该 token 上，否则 panic
pub fn remove_token(token: usize) {
    let mut active = SATP_ACTIVE.exclusive_access();
    let harts = active.token_harts.remove(&token).unwrap_or(0);
    assert_eq!(harts, 0, "destroyed address space is still active");
}
