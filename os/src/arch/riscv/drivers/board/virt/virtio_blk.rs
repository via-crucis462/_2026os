use core::ptr::NonNull;

use crate::{MEMORY_END, MMIO_SLOT_SIZE};
use crate::mm::{
    kernel_token, PageTable, PhysAddr, VirtAddr,
};
use crate::drivers::dma::DMA_MEMORY;
use crate::sync::MPSafeCell;
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

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtioPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;

        if vaddr >= 0x8000_0000
            && vaddr
                .checked_add(buffer.len())
                .is_some_and(|end| end <= MEMORY_END)
        {
            return vaddr as VirtioPhysAddr;
        }
        
        PageTable::from_token(kernel_token())
            .translate_va(VirtAddr::from(vaddr))
            .expect("virtio buffer is not mapped")
            .0 as VirtioPhysAddr
    }

    unsafe fn unshare(
        _paddr: VirtioPhysAddr,
        _buffer: NonNull<[u8]>,
        _direction: BufferDirection,
    ) {
    }
}
