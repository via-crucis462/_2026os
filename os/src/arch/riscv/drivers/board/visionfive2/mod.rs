//! VisionFive2 板级驱动配置

mod dwmac;
mod sdcard;

use crate::ext4fs::BlockDevice;
use sdcard::SdBlockDevice;
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;

use dwmac::DwMacWrapper;

pub type BlockDeviceImpl = SdBlockDevice;
pub type NetDeviceImpl = DwMacWrapper;

lazy_static! {
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = Arc::new(BlockDeviceImpl::new());
    pub static ref NET_DEVICE: Arc<NetDeviceImpl> = Arc::new(NetDeviceImpl::new());
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
