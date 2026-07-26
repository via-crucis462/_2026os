use core::ptr::NonNull;

use super::BlockDevice;
use crate::{BLOCK_MMIO_SIZE, MMIO_SLOT_SIZE};
use crate::mm::{
    frame_alloc, frame_dealloc, kernel_token, FrameTracker, PageTable, PhysAddr, PhysPageNum,
    StepByOne, VirtAddr,
};
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use virtio_drivers::{Hal, transport::mmio::{MmioTransport, VirtIOHeader}, BufferDirection, PhysAddr as VirtioPhysAddr};
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::Transport;
use virtio_drivers::transport::DeviceType;

#[allow(unused)]
const VIRTIO0: usize = 0x10001000;
/// VirtIOBlock device driver strcuture for virtio_blk device
pub struct VirtIOBlock{
    pub inner: MPSafeCell<VirtIOBlk<VirtioHal, MmioTransport<'static>>>
}

lazy_static! {
    static ref QUEUE_FRAMES: MPSafeCell<Vec<FrameTracker>> = MPSafeCell::new(Vec::new());
}


impl VirtIOBlock {
    #[allow(unused)]
    pub fn new() -> Self {
        let mut index = 0;
        // 和网卡一样，动态扫描 8 个槽位寻找磁盘
        let inner = loop {
            if index >= 8 {
                panic!("virtio_blk: no device found");
            }
            let addr = VIRTIO0 + 0x1000 * index;

            let trans = if let Ok(trans) = unsafe {
                MmioTransport::new(
                    NonNull::new(addr as *mut VirtIOHeader).unwrap(),
                    MMIO_SLOT_SIZE
                )
            } {
                if trans.device_type() != DeviceType::Block {
                    index += 1;
                    continue;
                }
                trans
            } else {
                index += 1;
                continue;
            };
            if let Ok(blk) = VirtIOBlk::<VirtioHal, MmioTransport<'static>>::new(
                    trans
            ) {
                break blk;
            } else {
                index += 1;
                continue;
            }
        };
        VirtIOBlock {
            inner: MPSafeCell::new(inner)
        }
    }
}

pub struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (VirtioPhysAddr, NonNull<u8>) {
        let mut ppn_base = PhysPageNum(0);
        for i in 0..pages {
            let frame = frame_alloc(crate::mm::PageSize::Page4K).unwrap();
            if i == 0 {
                ppn_base = frame.ppn;
            }
            // 这里假设frame_alloc分配的物理页是连续的
            // 可能有问题
            assert_eq!(frame.ppn.0, ppn_base.0 + i);
            QUEUE_FRAMES.exclusive_access().push(frame);
        }
        let pa: PhysAddr = ppn_base.into();
        ((pa.0 as u64).into(), NonNull::new(pa.0 as *mut u8).unwrap())
    }

    unsafe fn dma_dealloc(pa: u64, va: NonNull<u8>, pages: usize) -> i32 {
        let pa = PhysAddr::from(pa as usize);
        let mut ppn_base: PhysPageNum = pa.into();
        for _ in 0..pages {
            frame_dealloc(ppn_base, crate::mm::PageSize::Page4K);
            ppn_base.step();
        }
        0
    }
    unsafe fn mmio_phys_to_virt(addr: u64, _size: usize) -> NonNull<u8> {
        NonNull::new(addr as *mut u8).unwrap()
    }
    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> VirtioPhysAddr {
        buffer.as_ptr() as *const () as usize as VirtioPhysAddr
    }
    unsafe fn unshare(paddr: VirtioPhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
        
    }
}
