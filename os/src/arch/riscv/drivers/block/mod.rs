//! virtio_blk device driver

mod virtio_blk;
pub mod virtio_net;
pub use virtio_net::VirtIONetWrapper;
pub use virtio_blk::VirtIOBlock;

use alloc::sync::Arc;
use crate::ext4fs::BlockDevice;
use lazy_static::*;

type BlockDeviceImpl = virtio_blk::VirtIOBlock;

lazy_static! {
    /// The global block device driver instance: BLOCK_DEVICE with BlockDevice trait
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = Arc::new(BlockDeviceImpl::new());
    pub static ref NET_DEVICE: Arc<VirtIONetWrapper> = Arc::new(VirtIONetWrapper::new());
}

#[allow(unused)]
/// Test the block device
pub fn block_device_test() {
    let block_device = BLOCK_DEVICE.clone();
    let mut write_buffer = [0u8; 512];
    let mut driver = NET_DEVICE.0.exclusive_access();
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
