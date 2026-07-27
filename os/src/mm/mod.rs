//! Memory management implementation
//!
//! SV39 page-based virtual-memory architecture for RV64 systems, and
//! everything about memory management, like frame allocator, page table,
//! map area and memory set, is implemented here.
//!
//! Every task or process has a memory_set to control its virtual memory.

mod frame_allocator;
pub use frame_allocator::get_free_frames;
mod heap_allocator;
mod id;
mod memory_set;

pub mod address;
pub mod flags;
/// mmap系统调用相关
pub mod mmap;
pub mod page_table;
pub mod user_buffer;

pub use crate::arch::mm::pte;
use address::VPNRange;
pub use address::{PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
use core::hint::spin_loop;
use core::ptr::{read_volatile, write_volatile};
pub use flags::PTEFlags;
pub use frame_allocator::{frame_alloc, frame_dealloc, frame_ref_count, FrameTracker};
pub use memory_set::remap_test;
pub use memory_set::{kernel_token, MapPermission, MemorySet, KERNEL_SPACE};
#[allow(unused)]
pub use memory_set::{MapArea, MapType};
pub use page_table::*;
pub use user_buffer::UserBuffer;

/// initiate heap allocator, frame allocator and kernel space
pub fn init() {
    heap_allocator::init_heap();
    frame_allocator::init_frame_allocator();
    #[cfg(target_arch = "riscv64")]
    KERNEL_SPACE.exclusive_access().activate();
}
