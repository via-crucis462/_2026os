//! Task pid implementation.
//!
//! Assign PID to the process here. At the same time, the position of the application KernelStack
//! is determined according to the PID.

use crate::arch::config::*;
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE, PageSize};
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
#[allow(unused)]
use core::arch::asm;

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
        self.recycled.push(id);
    }
}

lazy_static! {
    static ref PID_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new_with_start(1));
    static ref TID_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new());
    static ref KSTACK_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new());    
}

/// Abstract structure of PID
pub struct PidHandle(pub usize);
pub struct TIdHandle(pub usize);

impl Drop for PidHandle {
    fn drop(&mut self) {
        //println!("drop pid {}", self.0);
        PID_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

impl Drop for TIdHandle {
    fn drop(&mut self) {
        //println!("drop tid {}", self.0);
        TID_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

/// Allocate a new PID
pub fn pid_alloc() -> PidHandle {
    PidHandle(PID_ALLOCATOR.exclusive_access().alloc())
}

pub fn tid_alloc() -> TIdHandle {
    TIdHandle(TID_ALLOCATOR.exclusive_access().alloc())
}

/// Return (bottom, top) of a kernel stack in kernel space.
#[cfg(target_arch = "loongarch64")]
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let top = 0x800_0000 - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}
#[cfg(target_arch = "riscv64")]
// 内核空间地址
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let top = TRAMPOLINE - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}

/// Kernel stack for a process(task)
pub struct KernelStack(pub usize);

/// allocate a new kernel stack
pub fn kstack_alloc() -> KernelStack {
    let kstack_id = KSTACK_ALLOCATOR.exclusive_access().alloc();
    let (kstack_bottom, kstack_top) = kernel_stack_position(kstack_id);
    debug!("kstack_alloc: allocated kernel stack {} with bottom {:#x} and top {:#x}", kstack_id, kstack_bottom, kstack_top);
    KERNEL_SPACE.exclusive_access().insert_framed_area(
        kstack_bottom.into(),
        kstack_top.into(),
        MapPermission::R | MapPermission::W,
        PageSize::Page4K, // 内核栈用标准页
    );
    KernelStack(kstack_id)
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let (kernel_stack_bottom, _) = kernel_stack_position(self.0);
        let kernel_stack_bottom_va: VirtAddr = kernel_stack_bottom.into();
        KERNEL_SPACE
            .exclusive_access()
            .remove_area_with_start_vpn(kernel_stack_bottom_va.into());
        KSTACK_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

impl KernelStack {
    /// Push a variable of type T into the top of the KernelStack and return its raw pointer
    /// 为la64调整，rv64需要改回去，暂时不改
    #[allow(unused)]
    #[cfg(target_arch = "loongarch64")]
    pub fn push_on_top<T>(&self, value: T) -> *mut T
    where
        T: Sized,
    {
        let kernel_stack_top = self.get_top();
        let size = core::mem::size_of::<T>();
        let align = core::mem::align_of::<T>();
        let sp = (kernel_stack_top - size) & !(align - 1);
        let ptr_mut = sp as *mut T;
        //println!("push_on_top: kernel_stack_top={:#x}, size={}, align={}, sp={:#x}", kernel_stack_top, size, align, sp);
        unsafe {
            core::ptr::write(ptr_mut, value);
        }
        //println!("push_on_top: value pushed at {:#x}", ptr_mut as usize);
        ptr_mut
    }
    /// Get the top of the KernelStack
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = kernel_stack_position(self.0);
        kernel_stack_top
    }
}
