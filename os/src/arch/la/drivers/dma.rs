use crate::mm::{
    PhysAddr, PhysPageNum,
    address::SimpleRange,
};
use alloc::vec::Vec;

use lazy_static::lazy_static;

use crate::sync::MPSafeCell;

use super::super::config::*;

extern "C"{
    fn ekernel();
}

/// 固定的DMA区域物理页管理器
lazy_static!{
    pub static ref QUEUE_FRAMES: MPSafeCell<DmaMemManager> = unsafe {
        let start = PhysAddr::from(ekernel as *const () as usize);
        let end = PhysAddr::from(ekernel as *const () as usize + DMA_SIZE);
        let spn = start.std_ceil();
        let epn = end.std_floor();
        MPSafeCell::new(DmaMemManager {
            start_ppn: spn,
            end_ppn: epn,
            allocated: Vec::new(),
        }
    )};
}

/// 维护DMA区域的内存的管理器，回收还没完全实现
/// 可保证分配的连续性
/// 以标准页为单位，分配的页数由调用者指定
pub struct DmaMemManager {
    /// DMA 区域起点（含）
    pub start_ppn: PhysPageNum,
    /// DMA 区域终点（不含）
    pub end_ppn: PhysPageNum,
    /// 已分配的左闭右开物理页号区间
    pub allocated: Vec<SimpleRange<PhysPageNum>>,
}

impl DmaMemManager {
    /// 分配连续的DMA页
    pub fn alloc(&mut self, pages: usize) -> Option<DmaBuffer> {
        if pages == 0 {
            return None;
        }
        let mut current_ppn = self.start_ppn;
        while current_ppn.0 + pages <= self.end_ppn.0 {
            let range = SimpleRange::<PhysPageNum>::new(
                current_ppn.0.into(),
                (current_ppn.0 + pages).into()
            );
            // 检查是否与已分配的范围重叠
            if !self.allocated.iter().any(|&r| 
                r.get_end().0 > range.get_start().0 && r.get_start().0 < range.get_end().0
            ) {
                self.allocated.push(range);
                
                return Some(DmaBuffer{
                    phys_addr: range.get_start().into(),
                    pages: pages,
                });
            }
            current_ppn.0 += 1;
        }
        None
    }
}

/// DMA 缓冲区
/// 
/// 需要分配器保证分配的连续性
pub struct DmaBuffer {
    /// 起始物理地址
    phys_addr: PhysAddr,
    /// 4K标准页数目
    pages: usize,
}

impl DmaBuffer {
    pub const fn uncached_ptr(&self) -> *mut u8 {
        (self.phys_addr.0 | UNCHACHED_KERNEL_BASE) as *mut u8
    }
    pub const fn cached_ptr(&self) -> *mut u8 {
        (self.phys_addr.0 | CACHED_KERNEL_BASE) as *mut u8
    }
    pub const fn phys_addr(&self) -> PhysAddr {
        self.phys_addr
    }
    pub const fn pages(&self) -> usize {
        self.pages
    }
}
