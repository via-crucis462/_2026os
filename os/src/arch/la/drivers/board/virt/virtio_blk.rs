//! 为la64重新实现virtio块设备驱动，完善中

# ![allow(unused)] // 目前还有一些未使用的函数和变量
use super::BlockDevice;
use crate::arch::config::UNCHACHED_KERNEL_BASE;
use crate::mm::address::VPNRange;
use crate::mm::{
    FrameTracker, KERNEL_SPACE, MapArea, PageTable, PhysAddr, PhysPageNum, StepByOne, VirtAddr, frame_alloc, frame_dealloc, kernel_token
};
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use virtio_drivers_la::transport::{self, Transport};
use core::cell::RefMut;
use core::iter::Rev;
use core::ptr::NonNull;
use virtio_drivers_la::{Hal, BufferDirection, PhysAddr as VirtioPhysAddr};
use virtio_drivers_la::transport::pci::PciTransport;
use virtio_drivers_la::device::blk::VirtIOBlk;
use crate::arch::config::*;

/// VirtIOBlock device driver strcuture for virtio_blk device
/// 已修改，新增transport接口
pub struct VirtIOBlock{
    // 相比rv64的旧版实现，使用了新版库的Transport泛型（PciTransport），
    // 新版库会由此自动完成原驱动到pci的转换
    pub inner: MPSafeCell<VirtIOBlk<VirtioHal, PciTransport>>,
}

// 维护DMA区域的内存的管理器，不过回收还没完全实现
pub struct DmaMemManager {
    pub start_ppn: PhysPageNum,
    pub end_ppn: PhysPageNum,
    // 虽然是物理页，但这里用VPNRange来管理
    pub allocated: Vec<VPNRange>,
}

extern "C"{
    fn ekernel();
}

/// 固定的DMA区域物理页管理器
/// 现在摆脱了对FA的依赖并且保证了分配的连续性
lazy_static!{
    pub static ref QUEUE_FRAMES: MPSafeCell<DmaMemManager> = unsafe { MPSafeCell::new(DmaMemManager {
        start_ppn: PhysPageNum(ekernel as *const() as usize / PAGE_SIZE),
        end_ppn: PhysAddr(ekernel as *const() as usize + DMA_SIZE).std_floor(),
        allocated: Vec::new(),
    }) };
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
        let mut manager = QUEUE_FRAMES.exclusive_access();
        let start_ppn = manager.start_ppn;
        let end_ppn = manager.end_ppn;
        // 从0开始寻找连续的pages页，直到找到一个不与已分配范围重叠的区域
        // 目前的实现很暴力，后续可以优化算法
        let mut current_ppn = manager.start_ppn;
        while current_ppn.0 + pages <= end_ppn.0 {
            let range = VPNRange::new(
                current_ppn.0.into(),
                (current_ppn.0 + pages).into()
            );
            // 检查是否与已分配的范围重叠
            if !manager.allocated.iter().any(|&r| 
                r.get_end().0 > range.get_start().0 && r.get_start().0 < range.get_end().0
            ) {
                manager.allocated.push(range);
                let paddr = PhysAddr::from(current_ppn.0 * PAGE_SIZE);
                return (
                    paddr.0 as VirtioPhysAddr,
                    // 使用窗口映射后地址
                    NonNull::new((paddr.0 | UNCHACHED_KERNEL_BASE) as *mut u8).unwrap()
                );
            }
            current_ppn = PhysPageNum(current_ppn.0 + pages);
        }
        panic!("Out of DMA memory!");
    }
    unsafe fn dma_dealloc(paddr: VirtioPhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        let paddr = paddr as usize & 0x00FF_FFFF_FFFF_FFFF;// 应对传入的是窗口地址的情况
        info!("deallocating DMA memory starts at 0x{:x} , end: 0x{:x} pages.", paddr, pages);
        let mut manager = QUEUE_FRAMES.exclusive_access();
        let start_ppn = PhysAddr(paddr as usize).std_floor(); // 硬件统一使用标准页
        let end_ppn = PhysPageNum(start_ppn.0 + pages);
        // 简化实现：只要有交集的块就直接整个删除
        if let Some(pos) = manager.allocated.iter().position(|&r| 
            r.get_end().0 > start_ppn.0 && r.get_start().0 < end_ppn.0
        ) {
            manager.allocated.remove(pos);
            info!("deallocated DMA memory, start:0x{:x}, end:0x{:x}.", start_ppn.0, end_ppn.0);
        }
        // 

        0
    }
    unsafe fn mmio_phys_to_virt(paddr: VirtioPhysAddr, _size: usize) -> NonNull<u8> {
        // MMIO 使用 Uncached 窗口 (DMW0): 0x8000_0000_0000_0000
        let va = (paddr as usize) | 0x8000_0000_0000_0000;
        NonNull::new(va as *mut u8).unwrap()
    }
    // 暂时直接返回
    unsafe fn share(buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) -> virtio_drivers_la::PhysAddr {
        (buffer.as_ptr() as *const() as usize  & 0x00FF_FFFF_FFFF_FFFF) as virtio_drivers_la::PhysAddr
    }
    unsafe fn unshare(_paddr: VirtioPhysAddr, _buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) {
        // do nothing
        // 未实现
    }
} 