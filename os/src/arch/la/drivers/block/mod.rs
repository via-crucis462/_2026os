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
type BlockDeviceImpl = virtio_blk::VirtIOBlock;
type NetDeviceImpl = virtio_net::VirtIONetWrapper;

lazy_static! {
    /// The global block device driver instance: BLOCK_DEVICE with BlockDevice trait
    /// 已修改：从固定mmio地址改为扫描获取
    pub static ref BLOCK_DEVICE: Arc<BlockDeviceImpl> = {
        let pci_block_device_trans = pci::scan_pci_device_to_trans(DeviceType::VirtIOBlock).expect("Failed to find PCI device");
        unsafe {
             Arc::new(BlockDeviceImpl::new(pci_block_device_trans))
        }
    };
    pub static ref NET_DEVICE: Arc<virtio_net::VirtIONetWrapper> = {
        let pci_net_device_trans = pci::scan_pci_device_to_trans(DeviceType::VIrtIONet).expect("Failed to find PCI device");
        unsafe {
            Arc::new(virtio_net::VirtIONetWrapper::new(pci_net_device_trans))
        }

    };
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
