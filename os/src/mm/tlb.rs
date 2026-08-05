//! RISC-V TLB ownership tracking for address-space shootdowns.

use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use super::FrameTracker;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;
use riscv::register::satp as satp_csr;

struct SatpActive {
    token_harts: BTreeMap<usize, usize>,
    /// 已被销毁、但仍有核的 satp 指向的地址空间页表帧
    /// 
    /// 在最后一个核切换离开后才释放，避免破坏其它核的 satp
    dying: BTreeMap<usize, Vec<FrameTracker>>,
}

impl SatpActive {
    fn new() -> Self {
        Self {
            token_harts: BTreeMap::new(),
            dying: BTreeMap::new(),
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
    /// 
    /// 旧 satp 通过 csrr 读取。
    /// 
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
        let old_token = satp_csr::read().bits();
        if old_token == token {
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
    // boot 阶段直接用 csrw 不会插入到表中，忽略即可，后续不会有核使用内核页表的 token
    if let Some(old_harts) = active.token_harts.get_mut(&old_token) {
        *old_harts &= !bit;
        if *old_harts == 0 {
            active.token_harts.remove(&old_token);
            // 如果最后一个切走的，把旧页表帧释放掉
            if let Some(frames) = active.dying.remove(&old_token) {
                drop(frames);
            }
        }
    }
}

/// 移除一个已经销毁的 token，并接管其页表帧的所有权
///
/// 若仍有核的 satp 指向该页表，延迟释放帧
/// 
/// 调用者必须已对相关核完成 TLB 刷新
pub fn remove_token(token: usize, frames: Vec<FrameTracker>) {
    let mut active = SATP_ACTIVE.exclusive_access();
    let harts = active.token_harts.get(&token).copied().unwrap_or(0);
    if harts != 0 {
        active.dying.insert(token, frames);
    } else {
        // 无核引用该页表，移除条目并直接释放帧
        active.token_harts.remove(&token);
        drop(frames);
    }
}
