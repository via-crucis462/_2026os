//! Shared reserved DMA memory manager.

use alloc::vec::Vec;
use lazy_static::lazy_static;

use crate::{
    arch::config::{DMA_SIZE, MEMORY_END, PAGE_SIZE},
    mm::{address::SimpleRange, PhysAddr, PhysPageNum},
    sync::MPSafeCell,
};

extern "C" {
    fn ekernel();
}

/// 维护DMA区域的内存的管理器，回收还没完全实现
/// 可保证分配的连续性
/// 以标准页为单位，分配的页数由调用者指定
pub struct DmaMemManager {
    /// DMA 区域起点（含）
    start_ppn: PhysPageNum,
    /// DMA 区域终点（不含）
    end_ppn: PhysPageNum,
    /// 已分配的左闭右开物理页号区间
    allocated: Vec<SimpleRange<PhysPageNum>>,
}

impl DmaMemManager {
    fn new(start: PhysAddr, end: PhysAddr) -> Self {
        let start_ppn = start.std_ceil();
        let end_ppn = end.std_floor();
        assert!(start_ppn.0 < end_ppn.0, "DMA region is empty");

        Self {
            start_ppn,
            end_ppn,
            allocated: Vec::new(),
        }
    }

    /// 分配连续的DMA页
    pub fn alloc(&mut self, pages: usize) -> Option<DmaBuffer> {
        if pages == 0 || pages > self.end_ppn.0 - self.start_ppn.0 {
            return None;
        }

        let last_start = self.end_ppn.0 - pages;
        for start in self.start_ppn.0..=last_start {
            let range = SimpleRange::new(PhysPageNum(start), PhysPageNum(start + pages));
            // 检查是否与已分配的范围重叠
            let overlaps = self.allocated.iter().any(|allocated| {
                allocated.get_end().0 > range.get_start().0
                    && allocated.get_start().0 < range.get_end().0
            });
            if overlaps {
                continue;
            }

            self.allocated.push(range);
            return Some(DmaBuffer {
                phys_addr: range.get_start().into(),
                pages,
            });
        }

        None
    }

    /// 释放区域内的DMA页
    pub fn dealloc(&mut self, phys_addr: PhysAddr, pages: usize) -> bool {
        if pages == 0 || !phys_addr.std_aligned() {
            return false;
        }

        let start_ppn = phys_addr.std_floor();
        let end_ppn = PhysPageNum(start_ppn.0 + pages);
        let Some(index) = self
            .allocated
            .iter()
            .position(|range| range.get_start() == start_ppn && range.get_end() == end_ppn)
        else {
            return false;
        };

        self.allocated.swap_remove(index);
        true
    }
}

/// DMA 缓冲区
///
/// 需要分配器保证分配的连续性
#[derive(Clone, Copy)]
pub struct DmaBuffer {
    /// 起始物理地址
    phys_addr: PhysAddr,
    /// 4K标准页数目
    pages: usize,
}

impl DmaBuffer {
    pub const fn phys_addr(self) -> PhysAddr {
        self.phys_addr
    }

    pub const fn pages(self) -> usize {
        self.pages
    }

    pub fn cached_ptr(self) -> *mut u8 {
        self.phys_addr.get_cached_addr() as *mut u8
    }

    pub fn uncached_ptr(self) -> *mut u8 {
        self.phys_addr.get_uncached_addr() as *mut u8
    }

    /// Clears the allocation through the architecture's uncached kernel mapping.
    pub fn zero(self) {
        unsafe {
            core::ptr::write_bytes(self.uncached_ptr(), 0, self.pages * PAGE_SIZE);
        }
    }
}

lazy_static! {
    /// 固定的DMA区域物理页管理器
    pub static ref DMA_MEMORY: MPSafeCell<DmaMemManager> = {
        let start = PhysAddr::from(ekernel as *const () as usize);
        let end = PhysAddr::from(start.0 + DMA_SIZE);
        assert!(end.0 <= MEMORY_END, "DMA region exceeds physical memory");
        MPSafeCell::new(DmaMemManager::new(start, end))
    };
}
