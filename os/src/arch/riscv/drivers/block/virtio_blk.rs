use super::BlockDevice;
use crate::mm::{
    frame_alloc, frame_dealloc, kernel_token, FrameTracker, PageTable, PhysAddr, PhysPageNum,
    StepByOne, VirtAddr,
};
use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;
use virtio_drivers::{Hal, VirtIOBlk, VirtIOHeader};
use virtio_drivers::DeviceType;
#[allow(unused)]
const VIRTIO0: usize = 0x10001000;
/// VirtIOBlock device driver strcuture for virtio_blk device
pub struct VirtIOBlock{
    pub inner: MPSafeCell<VirtIOBlk<'static, VirtioHal>>
}

lazy_static! {
    static ref QUEUE_FRAMES: MPSafeCell<Vec<FrameTracker>> = MPSafeCell::new(Vec::new());
}


impl VirtIOBlock {
    #[allow(unused)]
    pub fn new() -> Self {
        let mut blk_addr: usize = 0;
        
        // 和网卡一样，动态扫描 8 个槽位寻找磁盘
        for i in 1..=8 {
            let addr = 0x10000000 + 0x1000 * i;
            let header = unsafe { &mut *(addr as *mut VirtIOHeader) };
            
            if header.verify() && header.device_type() == DeviceType::Block {
                info!("[kernel] Found virtio-blk device at 0x{:x}", addr);
                blk_addr = addr;
                break;
            }
        }
        
        if blk_addr == 0 {
            panic!("[kernel] virtio-blk device not found!");
        }

        unsafe {
            Self { inner: MPSafeCell::new(
                VirtIOBlk::<VirtioHal>::new(&mut *(blk_addr as *mut VirtIOHeader)).unwrap(),
            ) }
        }
    }
}

pub struct VirtioHal;

impl Hal for VirtioHal {
    fn dma_alloc(pages: usize) -> usize {
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
        pa.0
    }

    fn dma_dealloc(pa: usize, pages: usize) -> i32 {
        let pa = PhysAddr::from(pa);
        let mut ppn_base: PhysPageNum = pa.into();
        for _ in 0..pages {
            frame_dealloc(ppn_base, crate::mm::PageSize::Page4K);
            ppn_base.step();
        }
        0
    }

    fn phys_to_virt(addr: usize) -> usize {
        addr
    }

    fn virt_to_phys(vaddr: usize) -> usize {
        PageTable::from_token(kernel_token())
            .translate_va(VirtAddr::from(vaddr))
            .unwrap()
            .0
    }
}
