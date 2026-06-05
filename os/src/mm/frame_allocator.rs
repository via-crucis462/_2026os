use super::{PhysAddr, PhysPageNum, PageSize};
#[allow(unused)]
use crate::arch::config::{DMA_SIZE, MEMORY_END};
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};
use lazy_static::*;


/// tracker for physical page frame allocation and deallocation
/// 现在的实现带引用计数
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
        let bytes_array = ppn.get_bytes_array_with_size(page_size);
        for i in bytes_array {
            *i = 0;
        }
        Self { ppn, page_size: page_size }
    }

    pub fn from_ppn(ppn: PhysPageNum, page_size: PageSize) -> Self {
        frame_add_ref(ppn);
        Self { ppn, page_size: page_size }
    }
    pub fn get_bytes_array(&self) -> &'static mut [u8] {
        self.ppn.get_bytes_array_with_size(self.page_size)
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
            // 按页大小释放
            frame_dealloc_raw(self.ppn, self.page_size);
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
/// 当前实现与伙伴系统有相似处
/// 如果发现偏移未对齐，则自动向上取整并把中间放进recycled
/// 同时，分配页时优先从本级回收页中取，其次从更大页拆分
/// 1G页虽然理论支持但硬件内存不满足
pub struct StackFrameAllocator {
    current: usize,
    end: usize,
    recycled_std: Vec<usize>,
    recycled_mega: Vec<usize>,
    // 注：当前内存其实不够分配1G大页，但架构支持，所以仍然这样写
    recycled_giga: Vec<usize>,
}

impl StackFrameAllocator {
    pub fn init(&mut self, l: PhysPageNum, r: PhysPageNum) {
        self.current = l.0;
        self.end = r.0;
    }
    // 按大页优先顺序回收范围内的页
    fn recycle_range(&mut self, mut pos: usize, end: usize) {
        let mega = PageSize::Page2M.num_pages();
        let giga = PageSize::Page1G.num_pages();
        while pos < end {
            if pos % giga == 0 && end - pos >= giga {
                self.recycled_giga.push(pos);
                pos += giga;
            } else if pos % mega == 0 && end - pos >= mega {
                self.recycled_mega.push(pos);
                pos += mega;
            } else {
                self.recycled_std.push(pos);
                pos += 1;
            }
        }
    }
    // 拆分大页到多个小页（未使用的放到回收栈）， 返回第一个页号
    fn split_into(base: usize, total: usize, unit: usize, recycler: &mut Vec<usize>) -> usize {
        for i in 1..total {
            recycler.push(base + i * unit);
        }
        base
    }
    /// 注意是标准页
    pub fn free_frames(&self) -> usize {
        // 未曾分配过的页框数 (end - current) + 已经被释放回收的页框数
        (self.end - self.current) +
        self.recycled_std.len()*PageSize::Page4K.num_pages() +
        self.recycled_mega.len()*PageSize::Page2M.num_pages() +
        self.recycled_giga.len()*PageSize::Page1G.num_pages()
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
        // 优先直接从标准页链表取
        if let Some(ppn) = self.recycled_std.pop() {
            Some(ppn.into())
        } else if let Some(mega_ppn) = self.recycled_mega.pop() {
            // 其次从 2MB 大页回收链表拆分
            let mega_pages = PageSize::Page2M.num_pages();
            Some(Self::split_into(mega_ppn, mega_pages, 1, &mut self.recycled_std).into())
        } else if self.current < self.end {
            self.current += 1;
            Some((self.current - 1).into())
        } else if let Some(giga_ppn) = self.recycled_giga.pop() {
            // 两级拆分
            let mega_pages = PageSize::Page2M.num_pages();
            let mega_in_giga = PageSize::Page1G.num_pages() / mega_pages;
            let first_mega = Self::split_into(giga_ppn, mega_in_giga, mega_pages, &mut self.recycled_mega);
            Some(Self::split_into(first_mega, mega_pages, 1, &mut self.recycled_std).into())
        } else {
            None
        }
    }
    fn alloc_mega(&mut self) -> Option<PhysPageNum> {
        let mega_pages = PageSize::Page2M.num_pages();
        // 优先直接取
        if let Some(ppn) = self.recycled_mega.pop() {
            return Some(ppn.into());
        // 其次拆分1G
        } else if let Some(giga_ppn) = self.recycled_giga.pop() {
            let mega_in_giga = PageSize::Page1G.num_pages() / mega_pages;
            return Some(Self::split_into(giga_ppn, mega_in_giga, mega_pages, &mut self.recycled_mega).into());
        }
        // 回收页不满足要求，确保对齐后分配新的
        let aligned_current = ((self.current + mega_pages - 1) / mega_pages) * mega_pages;
        if aligned_current + mega_pages > self.end {
            None
        } else {
            self.recycle_range(self.current, aligned_current);
            self.current = aligned_current + mega_pages;
            Some(aligned_current.into())
        }
    }
    fn alloc_giga(&mut self) -> Option<PhysPageNum> {
        let giga_pages = PageSize::Page1G.num_pages();
        if let Some(ppn) = self.recycled_giga.pop() {
            return Some(ppn.into());
        }
        // 确保对齐后分配新的
        let aligned_current = ((self.current + giga_pages - 1) / giga_pages) * giga_pages;
        if aligned_current + giga_pages > self.end {
            None
        } else {
            self.recycle_range(self.current, aligned_current);
            self.current = aligned_current + giga_pages;
            Some(aligned_current.into())
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
/// 不过目前未实现连续分配
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
