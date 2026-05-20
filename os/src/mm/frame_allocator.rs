use super::{PhysAddr, PhysPageNum, PageSize};
#[allow(unused)]
use crate::arch::config::{DMA_SIZE, MEMORY_END};
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use riscv::addr::page;
use core::fmt::{self, Debug, Formatter};
use lazy_static::*;


/// tracker for physical page frame allocation and deallocation
pub struct FrameTracker {
    /// physical page number
    pub ppn: PhysPageNum,
    /// 页大小
    pub page_size: PageSize,
}

impl FrameTracker {
    /// Create a new FrameTracker
    pub fn new(ppn: PhysPageNum, page_size: PageSize) -> Self {
        // page cleaning
        let bytes_array = ppn.get_bytes_array();
        for i in bytes_array {
            *i = 0;
        }
        Self { ppn, page_size: page_size }
    }

    pub fn from_ppn(ppn: PhysPageNum, page_size: PageSize) -> Self {
        frame_add_ref(ppn);
        Self { ppn, page_size: page_size }
    }
}

impl Clone for FrameTracker {
    fn clone(&self) -> Self {
        Self::from_ppn(self.ppn, self.page_size)
    }
}

impl Debug for FrameTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        let remain = frame_release_ref(self.ppn);
        if remain == 0 {
            // 按页大小释放所有组成的基本页
            let page_size = self.page_size;
            frame_dealloc_raw(self.ppn, page_size);
        }
    }
}

trait FrameAllocator {
    fn new() -> Self;
    fn alloc(&mut self, page_size: PageSize) -> Option<PhysPageNum>;
    fn alloc_std(&mut self) -> Option<PhysPageNum>;
    fn alloc_mega(&mut self) -> Option<PhysPageNum>;
    fn alloc_giga(&mut self) -> Option<PhysPageNum>;
    fn dealloc(&mut self, ppn: PhysPageNum, page_size: PageSize);
    // 连续分配，暂时不实现
    fn alloc_con(&mut self, pages: usize) -> Option<PhysPageNum>;
}
/// an implementation for frame allocator
pub struct StackFrameAllocator {
    current: usize,
    end: usize,
    recycled_std: Vec<usize>,
    recycled_mega: Vec<usize>,
    recycled_giga: Vec<usize>,
}

impl StackFrameAllocator {
    pub fn init(&mut self, l: PhysPageNum, r: PhysPageNum) {
        self.current = l.0;
        self.end = r.0;
        // trace!("last {} Physical Frames.", self.end - self.current);
    }
    pub fn free_frames(&self) -> usize {
        // 未曾分配过的页框数 (end - current) + 已经被释放回收的页框数
        self.end - self.current + self.recycled_std.len() + self.recycled_mega.len() + self.recycled_giga.len()
    }
}
pub fn get_free_frames() -> usize {
    FRAME_ALLOCATOR.exclusive_access().free_frames()
}
impl FrameAllocator for StackFrameAllocator {
    fn new() -> Self {
        Self {
            current: 0,
            end: 0,
            recycled_std: Vec::new(),
            recycled_mega: Vec::new(),
            recycled_giga: Vec::new(),
        }
    }
    fn alloc(&mut self, page_size: PageSize) -> Option<PhysPageNum> {
        match page_size {
            PageSize::Page4K => self.alloc_std(),
            PageSize::Page2M => self.alloc_mega(),
            PageSize::Page1G => self.alloc_giga(),
        }
    }
    fn alloc_std(&mut self) -> Option<PhysPageNum> {
        if let Some(ppn) = self.recycled_std.pop() {
            Some(ppn.into())
        } else if self.current == self.end {
            None
        } else {
            self.current += 1;
            Some((self.current - 1).into())
        }
    }
    fn alloc_mega(&mut self) -> Option<PhysPageNum> {
        let mega_pages = PageSize::Page2M.num_pages();
        if let Some(ppn) = self.recycled_mega.pop() {
            Some(ppn.into())
        } else if self.current + mega_pages > self.end {
            None
        } else {
            self.current += mega_pages;
            Some((self.current - mega_pages).into())
        }
    }
    fn alloc_giga(&mut self) -> Option<PhysPageNum> {
        let giga_pages = PageSize::Page1G.num_pages();
        if let Some(ppn) = self.recycled_giga.pop() {
            Some(ppn.into())
        } else if self.current + giga_pages > self.end {
            None
        } else {
            self.current += giga_pages;
            Some((self.current - giga_pages).into())
        }
    }
    fn dealloc(&mut self, ppn: PhysPageNum, page_size: PageSize) {
        let ppn_val = ppn.0;
        // validity check: 按页大小检查对应回收链表防止重复释放
        let already_freed = match page_size {
            PageSize::Page4K => self.recycled_std.contains(&ppn_val),
            PageSize::Page2M => self.recycled_mega.contains(&ppn_val),
            PageSize::Page1G => self.recycled_giga.contains(&ppn_val),
        };
        if ppn_val >= self.end || already_freed {
            panic!("Frame ppn={:#x} has not been allocated!", ppn_val);
        }           
        // 按页大小回收
        match page_size {
            PageSize::Page4K => self.recycled_std.push(ppn_val),
            PageSize::Page2M => self.recycled_mega.push(ppn_val),
            PageSize::Page1G => self.recycled_giga.push(ppn_val),
        }
    }
    // 待实现
    fn alloc_con(&mut self, _pages: usize) -> Option<PhysPageNum> {
        None
    }
}

type FrameAllocatorImpl = StackFrameAllocator;

lazy_static! {
    /// frame allocator instance through lazy_static!
    pub static ref FRAME_ALLOCATOR: MPSafeCell<FrameAllocatorImpl> =
        MPSafeCell::new(FrameAllocatorImpl::new());
    static ref FRAME_REF_COUNTS: MPSafeCell<BTreeMap<usize, usize>> =
        MPSafeCell::new(BTreeMap::new());
}
/// initiate the frame allocator using `ekernel` and `MEMORY_END`
pub fn init_frame_allocator() {
    extern "C" {
        fn ekernel();
    }
    // 为DMA预留空间
    #[cfg(target_arch = "loongarch64")]
    let frame_start = ekernel as *const() as usize + DMA_SIZE;
    #[cfg(target_arch = "riscv64")]
    let frame_start = ekernel as *const() as usize;
    
    FRAME_ALLOCATOR.exclusive_access().init(
        PhysAddr::from(frame_start).std_ceil(),
        PhysAddr::from(MEMORY_END).std_floor(),
    );
}

/// Allocate a physical page frame in FrameTracker style
pub fn frame_alloc(page_size: PageSize) -> Option<FrameTracker> {
    let ppn = {
        let mut allocator = FRAME_ALLOCATOR.exclusive_access();
        allocator.alloc(page_size)
    }?;
    FRAME_REF_COUNTS.exclusive_access().insert(ppn.0, 1);
    Some(FrameTracker::new(ppn, page_size))
}
/// 连续分配物理页帧，返回起始物理地址, 只允许标准页
#[allow(unused)]
pub fn frame_alloc_con(pages: usize) -> Option<PhysPageNum> {
    FRAME_ALLOCATOR.exclusive_access().alloc_con(pages)
}

/// Deallocate a physical page frame with a given ppn
pub fn frame_dealloc(ppn: PhysPageNum, page_size: PageSize) {
    let remain = frame_release_ref(ppn);
    if remain == 0 {
        frame_dealloc_raw(ppn, page_size);
    }
}

pub fn frame_add_ref(ppn: PhysPageNum) {
    let mut ref_counts = FRAME_REF_COUNTS.exclusive_access();
    let counter = ref_counts.entry(ppn.0).or_insert(0);
    *counter += 1;
}

pub fn frame_ref_count(ppn: PhysPageNum) -> usize {
    FRAME_REF_COUNTS
        .exclusive_access()
        .get(&ppn.0)
        .copied()
        .unwrap_or(0)
}

fn frame_release_ref(ppn: PhysPageNum) -> usize {
    let mut ref_counts = FRAME_REF_COUNTS.exclusive_access();
    let counter = ref_counts
        .get_mut(&ppn.0)
        .unwrap_or_else(|| panic!("Frame ppn={:#x} refcount missing", ppn.0));
    assert!(*counter > 0, "Frame ppn={:#x} refcount underflow", ppn.0);
    *counter -= 1;
    let remain = *counter;
    if remain == 0 {
        ref_counts.remove(&ppn.0);
    }
    remain
}

fn frame_dealloc_raw(ppn: PhysPageNum, page_size: PageSize) {
    FRAME_ALLOCATOR.exclusive_access().dealloc(ppn, page_size);
}

#[allow(unused)]
/// a simple test for frame allocator
pub fn frame_allocator_test() {
    let mut v: Vec<FrameTracker> = Vec::new();
    for i in 0..5 {
        let frame = frame_alloc(PageSize::Page4K).unwrap();
        println!("{:?}", frame);
        v.push(frame);
    }
    v.clear();
    for i in 0..5 {
        let frame = frame_alloc(PageSize::Page4K).unwrap();
        println!("{:?}", frame);
        v.push(frame);
    }
    drop(v);
    println!("frame_allocator_test passed!");
}
