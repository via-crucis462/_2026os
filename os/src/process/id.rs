//! Task pid implementation.
//!
//! Assign PID to the process here. At the same time, the position of the application KernelStack
//! is determined according to the PID.

use crate::arch::config::*;
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE, PageSize};
#[cfg(target_arch = "riscv64")]
use crate::mm::flush_kernel_tlb_targets;
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
#[allow(unused)]
use core::arch::asm;

/// Maximum number of unused kernel stacks whose mappings are kept alive.
///
/// A cached stack keeps both its virtual address and physical frames, so a
/// stale TLB entry on another hart still describes the same mapping. 64 stacks
/// cover the peak concurrency of the pthread create/join benchmark while
/// keeping the retained memory bounded.
const KSTACK_CACHE_LIMIT: usize = 64;

pub struct RecycleAllocator {
    current: usize,
    recycled: Vec<usize>,
}

impl RecycleAllocator {
    pub fn new() -> Self {
        RecycleAllocator {
            current: 0,
            recycled: Vec::new(),
        }
    }
    pub fn new_with_start(start: usize) -> Self {
        RecycleAllocator {
            current: start,
            recycled: Vec::new(),
        }
    }
    pub fn alloc(&mut self) -> usize {
        if let Some(id) = self.recycled.pop() {
            id
        } else {
            //println!("RecycleAllocator: alloc new id {}", self.current);
            self.current += 1;
            self.current - 1
        }
    }
    pub fn dealloc(&mut self, id: usize) {
        assert!(id < self.current);
        assert!(
            !self.recycled.iter().any(|i| *i == id),
            "id {} has been deallocated!",
            id
        );
        //println!("RecycleAllocator: dealloc id {}", id);
        self.recycled.push(id);
    }
}

lazy_static! {
    static ref PID_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new_with_start(1));
    static ref KSTACK_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new());
    static ref KSTACK_CACHE: MPSafeCell<Vec<usize>> = MPSafeCell::new(Vec::new());
}

/// Abstract structure of PID
pub struct PidHandle(pub usize);
pub struct TIdHandle(pub usize, bool);

impl Drop for PidHandle {
    fn drop(&mut self) {
        //println!("drop pid {}", self.0);
        PID_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

impl Drop for TIdHandle {
    fn drop(&mut self) {
        //println!("drop tid {}", self.0);
        if self.1 {
            PID_ALLOCATOR.exclusive_access().dealloc(self.0);
        }
    }
}

/// Allocate a new PID
pub fn pid_alloc() -> PidHandle {
    PidHandle(PID_ALLOCATOR.exclusive_access().alloc())
}

pub fn tid_alloc() -> TIdHandle {
    TIdHandle(PID_ALLOCATOR.exclusive_access().alloc(), true)
}

pub fn tid_from_pid(pid: usize) -> TIdHandle {
    TIdHandle(pid, false)
}

/// Return (bottom, top) of a kernel stack in kernel space.
#[cfg(target_arch = "loongarch64")]
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    if app_id >= (LOWRAM_END - LOWRAM_BASE) / KERNEL_STACK_SIZE {
        panic!("Too many processes! app_id {} exceeds limit!", app_id);
    } 
    let top = LOWRAM_END - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}
#[cfg(target_arch = "riscv64")]
/// 保留最高 1GB 内核栈空间，在 map_trampoline 时会为在根页表分配第 511 个页表项，
/// 后续不能修改内核根页表，所以让内核栈始终使用第 511 个页表项映射的 1GB 内核栈空间
const KSTACK_LOWEST: usize = 0xffff_ffff_c000_0000;

#[cfg(target_arch = "riscv64")]
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let stride = KERNEL_STACK_SIZE + PAGE_SIZE;
    let offset = app_id
        .checked_mul(stride)
        .expect("kernel stack id overflow");
    let top = TRAMPOLINE
        .checked_sub(offset)
        .expect("kernel stack id exceeds the stack region");
    let bottom = top
        .checked_sub(KERNEL_STACK_SIZE)
        .expect("kernel stack id exceeds the stack region");
    if bottom < KSTACK_LOWEST {
        panic!("kernel stack id {} exceeds root entry 511 stack region", app_id);
    }
    (bottom, top)
}

/// Kernel stack for a process(task)
pub struct KernelStack(pub usize);

/// allocate a new kernel stack
pub fn kstack_alloc() -> KernelStack {
    // Reuse the complete old mapping. In particular, do not unmap/remap the
    // same virtual address to different physical frames: another hart may
    // still have a translation for this globally shared kernel page table.
    if let Some(kstack_id) = KSTACK_CACHE.exclusive_access().pop() {
        let (kstack_bottom, _) = kernel_stack_position(kstack_id);
        unsafe {
            // 清空内核栈
            core::ptr::write_bytes(kstack_bottom as *mut u8, 0, KERNEL_STACK_SIZE);
        }
        return KernelStack(kstack_id);
    }

    let kstack_id = KSTACK_ALLOCATOR.exclusive_access().alloc();
    let (kstack_bottom, kstack_top) = kernel_stack_position(kstack_id);
    warn!("kstack_alloc: allocated kernel stack {} with bottom {:#x} and top {:#x}", kstack_id, kstack_bottom, kstack_top);
    KERNEL_SPACE.insert_framed_area(
        kstack_bottom.into(),
        kstack_top.into(),
        MapPermission::R | MapPermission::W,
        PageSize::Page4K, // 内核栈用标准页
    );
    KernelStack(kstack_id)
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let mut cache = KSTACK_CACHE.exclusive_access();
        if cache.len() < KSTACK_CACHE_LIMIT {
            cache.push(self.0);
            return;
        }
        drop(cache);

        let (kernel_stack_bottom, _) = kernel_stack_position(self.0);
        let kernel_stack_bottom_va: VirtAddr = kernel_stack_bottom.into();
        let frames =
            KERNEL_SPACE.remove_area_with_start_vpn(kernel_stack_bottom_va.into());
        #[cfg(target_arch = "riscv64")]
        flush_kernel_tlb_targets();
        drop(frames);
        KSTACK_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

impl KernelStack {
    /// Return the aligned address of an object reserved at the top of this
    /// kernel stack. The kernel call stack grows below this object.
    pub fn position_for<T>(&self) -> usize
    where
        T: Sized,
    {
        let kernel_stack_top = self.get_top();
        let size = core::mem::size_of::<T>();
        let align = core::mem::align_of::<T>();
        (kernel_stack_top - size) & !(align - 1)
    }

    /// Push a variable of type T into the top of the KernelStack and return its raw pointer.
    #[allow(unused)]
    pub fn push_on_top<T>(&self, value: T) -> *mut T
    where
        T: Sized,
    {
        let sp = self.position_for::<T>();
        let ptr_mut = sp as *mut T;
        //println!("push_on_top: kernel_stack_top=0x{:x}, size={}, align={}, sp=0x{:x}", kernel_stack_top, size, align, sp);
        unsafe {
            core::ptr::write(ptr_mut, value);
        }
        //println!("push_on_top: value pushed at 0x{:x}", ptr_mut as usize);
        ptr_mut
    }
    /// Get the top of the KernelStack
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = kernel_stack_position(self.0);
        kernel_stack_top
    }
}
