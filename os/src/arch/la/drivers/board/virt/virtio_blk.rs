//! 为la64重新实现virtio块设备驱动，完善中

# ![allow(unused)] // 目前还有一些未使用的函数和变量
use super::BlockDevice;
use crate::mm::{
    PhysAddr,
};
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use core::cell::RefMut;
use core::iter::Rev;
use core::ptr::NonNull;
use crate::drivers::dma::DMA_MEMORY;


use virtio_drivers::{Hal, BufferDirection, PhysAddr as VirtioPhysAddr};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::{self, Transport};

/// VirtIOBlock device driver strcuture for virtio_blk device
/// 已修改，新增transport接口
pub struct VirtIOBlock{
    // 相比rv64的旧版实现，使用了新版库的Transport泛型（PciTransport），
    // 新版库会由此自动完成原驱动到pci的转换
    pub inner: MPSafeCell<VirtIOBlk<VirtioHal, PciTransport>>,
}

#[allow(unused)]
#[allow(dead_code)]
impl VirtIOBlock {
    pub unsafe fn new(transport: PciTransport) -> Self {
        let hal = VirtioHal;
        let blk = VirtIOBlk::new(transport).expect("Failed to initialize VirtIOBlk");
        Self { inner: MPSafeCell::new(blk) }
    }
    pub unsafe fn visit(&self) -> spin::MutexGuard<'_, VirtIOBlk<VirtioHal, PciTransport>> {
        self.inner.exclusive_access()
    }
}


pub struct VirtioHal;
use crate::mm;

// 部分实现，不过已经基本能跑通
unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtioPhysAddr, NonNull<u8>) {
        let buffer = DMA_MEMORY
            .exclusive_access()
            .alloc(pages)
            .unwrap_or_else(|| panic!("virtio DMA region exhausted for {} pages", pages));
        buffer.zero();
        (
            buffer.phys_addr().0 as VirtioPhysAddr,
            NonNull::new(buffer.uncached_ptr()).expect("DMA address must not be zero"),
        )
    }
    unsafe fn dma_dealloc(paddr: VirtioPhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        DMA_MEMORY
            .exclusive_access()
            .dealloc(PhysAddr::from(paddr as usize), pages)
            .then_some(0)
            .unwrap_or(-1)
    }
    unsafe fn mmio_phys_to_virt(paddr: VirtioPhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(PhysAddr::from(paddr as usize).get_uncached_addr() as *mut u8).unwrap()
    }
    // 暂时直接返回
    unsafe fn share(buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers::BufferDirection) -> virtio_drivers::PhysAddr {
        (buffer.as_ptr() as *const() as usize  & 0x00FF_FFFF_FFFF_FFFF) as virtio_drivers::PhysAddr
    }
    unsafe fn unshare(_paddr: VirtioPhysAddr, _buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers::BufferDirection) {
        // do nothing
        // 未实现
    }
}
