//! 为la64重新实现virtio块设备驱动，完善中

# ![allow(unused)] // 目前还有一些未使用的函数和变量
use super::BlockDevice;
use crate::arch::config::UNCHACHED_KERNEL_BASE;
use crate::mm::address::VPNRange;
use crate::mm::{
    FrameTracker, KERNEL_SPACE, MapArea, PageTable, PhysAddr, PhysPageNum, StepByOne, VirtAddr, frame_alloc, frame_dealloc, kernel_token
};
use crate::sync::UPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use virtio_drivers_la::transport::{self, Transport};
use core::cell::RefMut;
use core::iter::Rev;
use core::ptr::NonNull;
use virtio_drivers_la::{Hal, BufferDirection, PhysAddr as VirtioPhysAddr};
use virtio_drivers_la::transport::pci::PciTransport;
use virtio_drivers_la::device::blk::VirtIOBlk;

const VIRTIO0: usize = 0x10001000;
use crate::arch::config::*;
/// VirtIOBlock device driver strcuture for virtio_blk device
/// 需要实现根据pci地址的情况动态扫描与初始化，待实现
pub struct VirtIOBlock{
    inner: UPSafeCell<VirtIOBlk<VirtioHal, PciTransport>>,
}

// 维护DMA区域的内存的管理器，不过回收还没完全实现
pub struct DmaMemManager {
    pub start_ppn: PhysPageNum,
    pub end_ppn: PhysPageNum,
    pub current_ppn: PhysPageNum,
    pub allocated: Vec<VPNRange>,
}

extern "C"{
    fn ekernel();
}

/// 固定DMA区域的物理页管理器
lazy_static!{
    pub static ref QUEUE_FRAMES: UPSafeCell<DmaMemManager> = unsafe { UPSafeCell::new(DmaMemManager {
        start_ppn: PhysPageNum(ekernel as *const() as usize / PAGE_SIZE),
        end_ppn: PhysAddr(ekernel as *const() as usize + DMA_SIZE).floor(),
        current_ppn: PhysPageNum(ekernel as *const() as usize / PAGE_SIZE),
        allocated: Vec::new(),
    }) };
}


#[allow(unused)]
#[allow(dead_code)]
impl VirtIOBlock {
    pub unsafe fn new(transport: PciTransport) -> Self {
        let hal = VirtioHal;
        let blk = VirtIOBlk::new(transport).expect("Failed to initialize VirtIOBlk");
        Self { inner: UPSafeCell::new(blk) }
    }
    pub unsafe fn visit(&self) -> RefMut<'_,VirtIOBlk<VirtioHal, PciTransport>> {
        self.inner.exclusive_access()
    }
}

impl BlockDevice for VirtIOBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let len = buf.len();
        // 扇区大小
        const SECTOR_SIZE: usize = 512;
        // 4096 / 512 = 8
        let sectors = len / SECTOR_SIZE;
        
        let mut driver = self.inner.exclusive_access();
        
        let start_sector = block_id * sectors;
        // 滑动窗口说是
        for i in 0..sectors {
            let offset = i * SECTOR_SIZE;
            let sub_buf = &mut buf[offset..offset + SECTOR_SIZE];
            driver
                .read_blocks(start_sector + i, sub_buf)
                .expect("Error when reading VirtIOBlk");
        }
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        // 与 read_block 类似
        let len = buf.len();
        const SECTOR_SIZE: usize = 512;
        let sectors = len / SECTOR_SIZE;
        
        let mut driver = self.inner.exclusive_access();
        let start_sector = block_id * sectors;

        for i in 0..sectors {
            let offset = i * SECTOR_SIZE;
            let sub_buf = &buf[offset..offset + SECTOR_SIZE];
            driver
                .write_blocks(start_sector + i, sub_buf)
                .expect("Error when writing VirtIOBlk");
        }
    }
}



pub struct VirtioHal;

use crate::mm;

// 待实现
unsafe impl Hal for VirtioHal {
    // 在la64中，无须严格区分虚拟地址和物理地址，因为使用窗口映射，只关心数值即可
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtioPhysAddr, NonNull<u8>) {
        let mut manager = QUEUE_FRAMES.exclusive_access();
        let start_ppn = manager.start_ppn;
        let end_ppn = manager.end_ppn;

        let mut current_ppn = manager.current_ppn;
        while current_ppn.0 + pages <= end_ppn.0 {
            let range = VPNRange::new(
                current_ppn.0.into(),
                (current_ppn.0 + pages).into()
            );
            // 检查是否与已分配的范围重叠
            if !manager.allocated.iter().any(|&r| 
                r.get_end() > range.get_start() && r.get_start() < range.get_end()
            ) {
                manager.allocated.push(range);
                let paddr = PhysAddr::from(current_ppn.0 * PAGE_SIZE);
                manager.current_ppn = PhysPageNum(current_ppn.0 + pages);
                return (paddr.0 as VirtioPhysAddr, NonNull::new(paddr.0 as *mut u8).unwrap());
            }
            current_ppn = PhysPageNum(current_ppn.0 + pages);
        }
        panic!("Out of DMA memory!");
    }

    unsafe fn dma_dealloc(paddr: VirtioPhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        let pa = PhysAddr::from(paddr as usize);
        let mut ppn_base: PhysPageNum = pa.into();
        for _ in 0..pages {
            frame_dealloc(ppn_base);
            ppn_base.step();
        }
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: VirtioPhysAddr, _size: usize) -> NonNull<u8> {
        // MMIO 使用 Uncached 窗口 (DMW0): 0x8000_0000_0000_0000
        let va = (paddr as usize) | 0x8000_0000_0000_0000;
        NonNull::new(va as *mut u8).unwrap()
    }
    // 暂时直接返回
    unsafe fn share(buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) -> virtio_drivers_la::PhysAddr {
        (buffer.as_ptr() as *const() as usize  & 0x0000_FFFF_FFFF_FFFF) as virtio_drivers_la::PhysAddr
    }

    unsafe fn unshare(_paddr: VirtioPhysAddr, _buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) {
        // do nothing
        // 未实现
    }
} 