//! virtio_blk device driver

mod virtio_blk;
mod virtio_net;

pub use virtio_blk::*;


use crate::{ext4fs::BlockDevice};
use crate::drivers::DeviceType;
use lazy_static::*;
#[allow(unused)]
use crate::arch::drivers::pci;
use alloc::sync::Arc;
pub type BlockDeviceImpl = virtio_blk::VirtIOBlock;
pub type NetDeviceImpl = virtio_net::VirtIONetWrapper;

use crate::drivers::block::BLOCK_DEVICE;

use crate::drivers::block::block_cache::get_block_cache;
impl BlockDevice for VirtIOBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
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
            #[cfg (target_arch = "loongarch64")]
            driver
                .read_blocks(start_sector + i, sub_buf)
                .expect("Error when reading VirtIOBlk");
            #[cfg (target_arch = "riscv64")]
            driver
                .read_block(start_sector + i, sub_buf)
                .expect("Error when reading VirtIOBlk");
        }
    }
    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        // 与 read_block 类似
        let len = buf.len();
        const SECTOR_SIZE: usize = 512;
        let sectors = len / SECTOR_SIZE;
        
        let mut driver = self.inner.exclusive_access();
        let start_sector = block_id * sectors;

        for i in 0..sectors {
            let offset = i * SECTOR_SIZE;
            let sub_buf = &buf[offset..offset + SECTOR_SIZE];
            #[cfg (target_arch = "loongarch64")]
            driver
                .write_blocks(start_sector + i, sub_buf)
                .expect("Error when writing VirtIOBlk");
            #[cfg (target_arch = "riscv64")]
            driver
                .write_block(start_sector + i, sub_buf)
                .expect("Error when writing VirtIOBlk");
        }
    }
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; 4096] = block.get_ref(0);
        let len = buf.len();
        buf.copy_from_slice(&block_data[..len]);
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let len = buf.len();
        cache.lock().modify(0, |block_data: &mut [u8; 4096]| {
            block_data[..len].copy_from_slice(buf);
        });
    }
}

#[allow(unused)]
/// Test the block device
pub unsafe fn block_device_test() {
    let mut block_device = BLOCK_DEVICE.as_ref();
    let mut write_buffer = [0u8; 512];
    let mut read_buffer = [0u8; 512];
    for i in 0..512 {
        for byte in write_buffer.iter_mut() {
            *byte = i as u8;
        }
        block_device.write_block(i as usize, &write_buffer);
        block_device.read_block(i as usize, &mut read_buffer);
        assert_eq!(write_buffer, read_buffer);
    }
    println!("block device test passed!");
}
