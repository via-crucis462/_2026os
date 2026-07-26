use core::ptr::NonNull;

use super::{BlockDevice, BLOCK_DEVICE};
use crate::MMIO_SLOT_SIZE;
use crate::mm::{
    kernel_token, PageTable, PhysAddr, VirtAddr,
};
use crate::drivers::dma::DMA_MEMORY;
use crate::ext4fs::{get_block_cache, BLOCK_SZ};
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

impl BlockDevice for VirtIOBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len(), BLOCK_SZ, "block buffer must be {} bytes", BLOCK_SZ);
        self.inner
            .exclusive_access()
            .read_blocks(block_id * (BLOCK_SZ / 512), buf)
            .expect("virtio block read failed");
    }

    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(buf.len(), BLOCK_SZ, "block buffer must be {} bytes", BLOCK_SZ);
        self.inner
            .exclusive_access()
            .write_blocks(block_id * (BLOCK_SZ / 512), buf)
            .expect("virtio block write failed");
    }

    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert!(buf.len() <= BLOCK_SZ);
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; BLOCK_SZ] = block.get_ref(0);
        buf.copy_from_slice(&block_data[..buf.len()]);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert!(buf.len() <= BLOCK_SZ);
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        cache.lock().modify(0, |block_data: &mut [u8; BLOCK_SZ]| {
            block_data[..buf.len()].copy_from_slice(buf);
        });
    }
}
