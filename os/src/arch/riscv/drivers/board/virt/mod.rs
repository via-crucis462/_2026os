//! qemu virt 板级驱动配置

mod virtio_blk;
mod virtio_net;

use crate::ext4fs::BlockDevice;
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;

use virtio_blk::VirtIOBlock;
use virtio_net::VirtIONetWrapper;

type BlockDeviceImpl = VirtIOBlock;
type NetDeviceImpl = VirtIONetWrapper;

lazy_static! {
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = Arc::new(BlockDeviceImpl::new());
    pub static ref NET_DEVICE: Arc<NetDeviceImpl> = Arc::new(NetDeviceImpl::new());
}

pub const BLOCK_SZ: usize = 4096;

use crate::drivers::block::block_cache::get_block_cache;
use crate::ext4fs::BLOCK_SZ as EXT4_BLOCK_SZ;
impl BlockDevice for VirtIOBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len(), EXT4_BLOCK_SZ, "block buffer must be {} bytes", EXT4_BLOCK_SZ);
        self.inner
            .exclusive_access()
            .read_blocks(block_id * (EXT4_BLOCK_SZ / 512), buf)
            .expect("virtio block read failed");
    }

    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(buf.len(), EXT4_BLOCK_SZ, "block buffer must be {} bytes", EXT4_BLOCK_SZ);
        self.inner
            .exclusive_access()
            .write_blocks(block_id * (EXT4_BLOCK_SZ / 512), buf)
            .expect("virtio block write failed");
    }

    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; BLOCK_SZ] = block.get_ref(0);
        let len = buf.len();
        buf.copy_from_slice(&block_data[..len]);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let len = buf.len();
        cache.lock().modify(0, |block_data: &mut [u8; BLOCK_SZ]| {
            block_data[..len].copy_from_slice(buf);
        });
    }
}

#[allow(unused)]
/// Test the block device
pub fn block_device_test() {
    let block_device = BLOCK_DEVICE.clone();
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
