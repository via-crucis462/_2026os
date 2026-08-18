//! The global allocator
use crate::arch::config::{CACHED_KERNEL_BASE, CPU_CORE_NUM, PAGE_SIZE};
use crate::mm::boot_memory;
use buddy_system_allocator::LockedHeap;
use core::alloc::{GlobalAlloc, Layout};

const LARGE_ALLOCATION_THRESHOLD: usize = 1 << 20;

struct ArenaLayout {
    heap_start: usize,
    heap_end: usize,
    local_arena_size: usize,
    local_total_size: usize,
}

/// 把启动阶段确定的物理堆区间转换为内核窗口地址，并划分各分配域。
fn arena_layout() -> ArenaLayout {
    let memory = boot_memory();
    let heap_start = memory.heap_start | CACHED_KERNEL_BASE;
    let heap_end = memory.heap_end | CACHED_KERNEL_BASE;
    let heap_size = heap_end - heap_start;
    // 三分之一平均分给各 hart 的本地堆，三分之二留给大块分配。
    let local_arena_size = (heap_size / 3 / CPU_CORE_NUM) & !(PAGE_SIZE - 1);
    let local_total_size = local_arena_size * CPU_CORE_NUM;
    assert!(local_arena_size >= PAGE_SIZE);
    assert!(heap_size - local_total_size >= PAGE_SIZE);
    ArenaLayout {
        heap_start,
        heap_end,
        local_arena_size,
        local_total_size,
    }
}

struct PerHartHeap {
    local_arenas: [LockedHeap; CPU_CORE_NUM],
    large_arena: LockedHeap,
}

enum HeapOwner {
    Local(usize),
    Large,
}

impl PerHartHeap {
    const fn empty() -> Self {
        Self {
            local_arenas: [const { LockedHeap::empty() }; CPU_CORE_NUM],
            large_arena: LockedHeap::empty(),
        }
    }

    fn owner(&self, ptr: *mut u8) -> Option<HeapOwner> {
        // 释放可能发生在不同 hart 上，必须按指针所在区间找到原分配器。
        let layout = arena_layout();
        let offset = (ptr as usize).checked_sub(layout.heap_start)?;
        if offset < layout.local_total_size {
            Some(HeapOwner::Local(offset / layout.local_arena_size))
        } else if (ptr as usize) < layout.heap_end {
            Some(HeapOwner::Large)
        } else {
            None
        }
    }
}

unsafe impl GlobalAlloc for PerHartHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE_ALLOCATION_THRESHOLD {
            return GlobalAlloc::alloc(&self.large_arena, layout);
        }

        let local_hart = crate::get_hart_id() % CPU_CORE_NUM;
        let ptr = GlobalAlloc::alloc(&self.local_arenas[local_hart], layout);
        if !ptr.is_null() {
            return ptr;
        }

        GlobalAlloc::alloc(&self.large_arena, layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let owner = self
            .owner(ptr)
            .expect("kernel heap pointer does not belong to any arena");
        match owner {
            HeapOwner::Local(arena) => {
                GlobalAlloc::dealloc(&self.local_arenas[arena], ptr, layout)
            }
            HeapOwner::Large => GlobalAlloc::dealloc(&self.large_arena, ptr, layout),
        }
    }
}

#[global_allocator]
/// heap allocator instance
static HEAP_ALLOCATOR: PerHartHeap = PerHartHeap::empty();

#[alloc_error_handler]
/// panic when heap allocation error occurs
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    let (local_used, local_total) = HEAP_ALLOCATOR.local_arenas.iter().fold(
        (0, 0),
        |(used, total), arena| {
            let heap = arena.lock();
            (
                used + heap.stats_alloc_actual(),
                total + heap.stats_total_bytes(),
            )
        },
    );
    let large_heap = HEAP_ALLOCATOR.large_arena.lock();
    let large_used = large_heap.stats_alloc_actual();
    let large_total = large_heap.stats_total_bytes();
    drop(large_heap);
    let (cache_pages, idmap, lru) =
        crate::drivers::block::cache::SHARED_PAGE_CACHE_MANAGER.stats();
    panic!(
        "Heap allocation error, layout = {:?}, local used = {}/{} bytes, large used = {}/{} bytes, cache_pages={:?} idmap={:?} lru={:?}",
        layout, local_used, local_total, large_used, large_total, cache_pages, idmap, lru
    );
}
/// 使用启动阶段划出的运行期堆区间初始化各伙伴分配器。
#[allow(warnings)]
pub fn init_heap() {
    let layout = arena_layout();
    let large_start = layout.heap_start + layout.local_total_size;
    unsafe {
        for (hart_id, arena) in HEAP_ALLOCATOR.local_arenas.iter().enumerate() {
            arena.lock().init(
                layout.heap_start + hart_id * layout.local_arena_size,
                layout.local_arena_size,
            );
        }
        HEAP_ALLOCATOR
            .large_arena
            .lock()
            .init(large_start, layout.heap_end - large_start);
    }
}

#[allow(unused)]
pub fn heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    let layout = arena_layout();
    let heap_range = layout.heap_start..layout.heap_end;
    let a = Box::new(5);
    assert_eq!(*a, 5);
    assert!(heap_range.contains(&(a.as_ref() as *const _ as usize)));
    drop(a);
    let mut v: Vec<usize> = Vec::new();
    for i in 0..500 {
        v.push(i);
    }
    for (i, val) in v.iter().take(500).enumerate() {
        assert_eq!(*val, i);
    }
    assert!(heap_range.contains(&(v.as_ptr() as usize)));
    drop(v);
    println!("heap_test passed!");
}
