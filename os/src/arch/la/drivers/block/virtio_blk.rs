//! 为la64重新实现virtio块设备驱动，完善中
use super::BlockDevice;
use crate::mm::{
    frame_alloc, frame_dealloc, kernel_token, FrameTracker, PageTable, PhysAddr, PhysPageNum,
    StepByOne, VirtAddr,
};
use crate::sync::UPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use core::ptr::NonNull;
use virtio_drivers_la::{Hal, VirtIOBlk, BufferDirection, PhysAddr as VirtioPhysAddr};
use virtio_drivers_la::transport::pci::PciTransport;

#[allow(unused)]
const VIRTIO0: usize = 0x10001000;

/// VirtIOBlock device driver strcuture for virtio_blk device
/// 注意：这里使用 PciTransport，因此需要在 new 中进行 PCI 枚举和初始化
pub struct VirtIOBlock(UPSafeCell<VirtIOBlk<VirtioHal, PciTransport>>);

lazy_static! {
    static ref QUEUE_FRAMES: UPSafeCell<Vec<FrameTracker>> = unsafe { UPSafeCell::new(Vec::new()) };
}

impl BlockDevice for VirtIOBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let len = buf.len();
        // 扇区大小
        const SECTOR_SIZE: usize = 512;
        // 4096 / 512 = 8
        let sectors = len / SECTOR_SIZE;
        
        let mut driver = self.0.exclusive_access();
        
        let start_sector = block_id * sectors;
        // 滑动窗口说是
        for i in 0..sectors {
            let offset = i * SECTOR_SIZE;
            let sub_buf = &mut buf[offset..offset + SECTOR_SIZE];
            driver
                .read_block(start_sector + i, sub_buf)
                .expect("Error when reading VirtIOBlk");
        }
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        // 与 read_block 类似
        let len = buf.len();
        const SECTOR_SIZE: usize = 512;
        let sectors = len / SECTOR_SIZE;
        
        let mut driver = self.0.exclusive_access();
        let start_sector = block_id * sectors;

        for i in 0..sectors {
            let offset = i * SECTOR_SIZE;
            let sub_buf = &buf[offset..offset + SECTOR_SIZE];
            driver
                .write_block(start_sector + i, sub_buf)
                .expect("Error when writing VirtIOBlk");
        }
    }
}

// 注意：这里需要你自行实现基于PCI的初始化逻辑，目前的MMIO方式不适用于PCI
// 下面的 impl VirtIOBlock 暂时保留空架子或旧代码，编译时可能会报错，请根据实际PCI库完善
// impl VirtIOBlock { ... }

pub struct VirtioHal;

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
        let pa_val = pa.0;
        // LoongArch DMW 直接映射：PA | 0x9000_0000_0000_0000 (Cached)
        let va_val = pa_val | 0x9000_0000_0000_0000;
        let ptr = NonNull::new(va_val as *mut u8).unwrap();
        
        // 必须清零
        unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), pages * 4096).fill(0) };

        (pa_val as u64, ptr)
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

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtioPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        // 如果虚拟地址在高半核空间(DMW)，直接取低位作为物理地址
        if vaddr & 0x8000_0000_0000_0000 != 0 {
            (vaddr & 0x0000_FFFF_FFFF_FFFF) as u64
        } else {
            // 否则查页表
            PageTable::from_token(kernel_token())
            .translate_va(VirtAddr::from(vaddr))
            .unwrap()
            .0 as u64
        }
    }

    unsafe fn unshare(_paddr: VirtioPhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        // Nothing to do
    }
}
        0
    }

    fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        paddr
    }

    unsafe fn share(buffer: core::ptr::NonNull<[u8]>, direction: virtio_drivers_la::BufferDirection) -> virtio_drivers_la::PhysAddr {
        0
    }

    unsafe fn unshare(_buffer: core::ptr::NonNull<[u8]>, _direction: virtio_drivers_la::BufferDirection) {
        // no-op
    }
}
