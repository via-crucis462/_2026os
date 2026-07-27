//! virtio_blk device driver

mod virtio_blk;
pub mod virtio_net;
#[cfg(feature = "board_vf2")]
use crate::arch::drivers::net::DwMacWrapper;
use crate::ext4fs::BlockDevice;
use crate::sdcard;
use alloc::sync::Arc;
use lazy_static::*;
pub use virtio_blk::VirtIOBlock;

type BlockDeviceImpl = sdcard::SdBlockDevice;

#[cfg(feature = "board_vf2")]
type NetDeviceImpl = DwMacWrapper;
#[cfg(not(feature = "board_vf2"))]
type NetDeviceImpl = virtio_net::VirtIONetWrapper;

lazy_static! {
    /// The global block device driver instance: BLOCK_DEVICE with BlockDevice trait
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = Arc::new(BlockDeviceImpl::new());
    pub static ref NET_DEVICE: Arc<NetDeviceImpl> = Arc::new(NetDeviceImpl::new());
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
