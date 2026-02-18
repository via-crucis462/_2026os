//! virtio_blk device driver

mod virtio_blk;

pub use virtio_blk::*;


use crate::{ext4fs::BlockDevice, sync::UPSafeCell};
use lazy_static::*;
#[allow(unused)]
use crate::arch::drivers::pci;

type BlockDeviceImpl = virtio_blk::VirtIOBlock;

lazy_static! {
    /// The global block device driver instance: BLOCK_DEVICE with BlockDevice trait
    pub static ref BLOCK_DEVICE: UPSafeCell<BlockDeviceImpl>
        = unsafe { UPSafeCell::new(BlockDeviceImpl::new(
            pci::scan_and_init_pci_device().expect("Failed to initialize PCI device")
        )) };
}

#[allow(unused)]
/// Test the block device
pub unsafe fn block_device_test() {
    let mut block_device = BLOCK_DEVICE.exclusive_access();
    let mut write_buffer = [0u8; 512];
    let mut read_buffer = [0u8; 512];
    for i in 0..512 {
        for byte in write_buffer.iter_mut() {
            *byte = i as u8;
        }
        block_device.write_block(i as *const () as usize, &write_buffer);
        block_device.read_block(i as *const () as usize, &mut read_buffer);
        assert_eq!(write_buffer, read_buffer);
    }
    println!("block device test passed!");
}
