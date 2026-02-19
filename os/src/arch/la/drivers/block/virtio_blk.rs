//! 为la64重新实现virtio块设备驱动，完善中

# ![allow(unused)] // 目前还有一些未使用的函数和变量
use super::BlockDevice;
use crate::arch::config::UNCHACHED_KERNEL_BASE;
use crate::mm::{
    frame_alloc, frame_dealloc, kernel_token, FrameTracker, PageTable, PhysAddr, PhysPageNum,
    StepByOne, VirtAddr,
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

#[allow(unused)]
const VIRTIO0: usize = 0x10001000;

/// VirtIOBlock device driver strcuture for virtio_blk device
/// 需要实现根据pci地址的情况动态扫描与初始化，待实现
pub struct VirtIOBlock{
    inner: UPSafeCell<VirtIOBlk<VirtioHal, PciTransport>>,
}

lazy_static! {
    static ref QUEUE_FRAMES: UPSafeCell<Vec<FrameTracker>> = unsafe { UPSafeCell::new(Vec::new()) };
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

// 待实现
unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtioPhysAddr, NonNull<u8>) {
        let mut ppn_base = PhysPageNum(0);
        for i in 0..pages {
            let frame = frame_alloc().unwrap();
            if i == 0 {
                ppn_base = frame.ppn;
            }
            assert_eq!(frame.ppn.0, ppn_base.0 + i);
            QUEUE_FRAMES.exclusive_access().push(frame);
        }
        let pa: PhysAddr = ppn_base.into();
        // 转换成窗口映射的虚拟地址
        let va_val = pa.0 | UNCHACHED_KERNEL_BASE;
        let ptr = NonNull::new(va_val as *mut u8).unwrap();
        (pa.0 as u64, ptr)
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

    unsafe fn share(buffer: core::ptr::NonNull<[u8]>, direction: virtio_drivers_la::BufferDirection) -> virtio_drivers_la::PhysAddr {
        0
    }

    unsafe fn unshare(_paddr: VirtioPhysAddr, _buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) {
        // do nothing
        // 未实现
        ()
    }
} 