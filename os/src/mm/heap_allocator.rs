//! The global allocator
use crate::arch::config::{CPU_CORE_NUM, KERNEL_HEAP_SIZE};
use buddy_system_allocator::LockedHeap;
use core::alloc::{GlobalAlloc, Layout};

#[cfg(target_arch = "riscv64")]
const LARGE_HEAP_RESERVE_SIZE: usize = 1 << 30;
#[cfg(target_arch = "loongarch64")]
const LARGE_HEAP_RESERVE_SIZE: usize = 1 << 28;
#[cfg(target_arch = "riscv64")]
const LARGE_ALLOCATION_BLOCK_SIZE: usize = 1 << 29;
#[cfg(target_arch = "loongarch64")]
const LARGE_ALLOCATION_BLOCK_SIZE: usize = 1 << 27;
const LARGE_ALLOCATION_THRESHOLD: usize = 1 << 20;
const LOCAL_HEAP_SIZE: usize = KERNEL_HEAP_SIZE - LARGE_HEAP_RESERVE_SIZE;
const LOCAL_ARENA_SIZE: usize = LOCAL_HEAP_SIZE / CPU_CORE_NUM;

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
        let heap_start = core::ptr::addr_of!(HEAP_SPACE) as *const u64 as usize;
        let offset = (ptr as usize).checked_sub(heap_start)?;
        if offset < LOCAL_HEAP_SIZE {
            Some(HeapOwner::Local(offset / LOCAL_ARENA_SIZE))
        } else if offset < KERNEL_HEAP_SIZE {
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
/// heap space ([u8; KERNEL_HEAP_SIZE])
/// 会被放到.bss段中
/// MUST be 8-byte aligned: LA264 enforces alignment, and buddy allocator
/// requires the base to be properly aligned to return valid pointers.
static mut HEAP_SPACE: [u64; KERNEL_HEAP_SIZE / 8] = [0; KERNEL_HEAP_SIZE / 8];
/// initiate heap allocator
#[allow(warnings)]
pub fn init_heap() {
    assert!(KERNEL_HEAP_SIZE > LARGE_HEAP_RESERVE_SIZE);
    assert_eq!(LOCAL_HEAP_SIZE % CPU_CORE_NUM, 0);
    let heap_start = core::ptr::addr_of!(HEAP_SPACE) as *const u64 as usize;
    let large_start = heap_start + LOCAL_HEAP_SIZE;
    let large_end = heap_start + KERNEL_HEAP_SIZE;
    let aligned_large_start = (large_start + LARGE_ALLOCATION_BLOCK_SIZE - 1)
        & !(LARGE_ALLOCATION_BLOCK_SIZE - 1);
    assert!(large_end - aligned_large_start >= LARGE_ALLOCATION_BLOCK_SIZE);
    unsafe {
        for (hart_id, arena) in HEAP_ALLOCATOR.local_arenas.iter().enumerate() {
            arena
                .lock()
                .init(heap_start + hart_id * LOCAL_ARENA_SIZE, LOCAL_ARENA_SIZE);
        }
        HEAP_ALLOCATOR
            .large_arena
            .lock()
            .init(aligned_large_start, large_end - aligned_large_start);
    }
}

#[allow(unused)]
pub fn heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    extern "C" {
        fn sbss();
        fn ebss();
    }
    let bss_range = sbss as *const () as usize..ebss as *const () as usize;
    let a = Box::new(5);
    assert_eq!(*a, 5);
    assert!(bss_range.contains(&(a.as_ref() as *const _ as usize)));
    drop(a);
    let mut v: Vec<usize> = Vec::new();
    for i in 0..500 {
        v.push(i);
    }
    for (i, val) in v.iter().take(500).enumerate() {
        assert_eq!(*val, i);
    }
    assert!(bss_range.contains(&(v.as_ptr() as usize)));
    drop(v);
    println!("heap_test passed!");
}
