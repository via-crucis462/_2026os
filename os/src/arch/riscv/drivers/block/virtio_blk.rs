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
pub struct VirtIOBlock(MPSafeCell<VirtIOBlk<'static, VirtioHal>>);

lazy_static! {
    static ref QUEUE_FRAMES: MPSafeCell<Vec<FrameTracker>> = MPSafeCell::new(Vec::new());
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

impl VirtIOBlock {
    #[allow(unused)]
    pub fn new() -> Self {
        let mut blk_addr: usize = 0;
        
        // 和网卡一样，动态扫描 8 个槽位寻找磁盘
        for i in 1..=8 {
            let addr = 0x10000000 + 0x1000 * i;
            let header = unsafe { &mut *(addr as *mut VirtIOHeader) };
            
            if header.verify() && header.device_type() == DeviceType::Block {
                println!("[kernel] Found virtio-blk device at {:#x}", addr);
                blk_addr = addr;
                break;
            }
        }
        
        if blk_addr == 0 {
            panic!("[kernel] virtio-blk device not found!");
        }

        unsafe {
            Self(MPSafeCell::new(
                VirtIOBlk::<VirtioHal>::new(&mut *(blk_addr as *mut VirtIOHeader)).unwrap(),
            ))
        }
    }
}

pub struct VirtioHal;

impl Hal for VirtioHal {
    fn dma_alloc(pages: usize) -> usize {
        let mut ppn_base = PhysPageNum(0);
        for i in 0..pages {
            let frame = frame_alloc().unwrap();
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
            frame_dealloc(ppn_base);
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
