//! 块设备驱动模块

// 传递 arch 中的 block 驱动
pub use crate::arch::drivers::*;

pub mod block_cache;
pub mod block_dev;
pub mod async_io;

use alloc::sync::Arc;

use crate::ext4fs::{get_block_cache, BlockDevice};
use lazy_static::*;


lazy_static! {
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = Arc::new(BlockDeviceImpl::new());
}


#[allow(unused)]
/// Test the block device
/// 会破坏磁盘数据，慎用
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
