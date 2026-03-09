//! block device driver

pub mod block;
pub mod pci;
pub use block::BLOCK_DEVICE;

use alloc::vec::Vec;
use lazy_static::lazy_static;
use crate::{drivers::pci::{PCIDevice,scan_bus}, sync::MPSafeCell};

lazy_static!(
    pub static ref DEVICE_MANAGER: MPSafeCell<DeviceManager> = unsafe{
        MPSafeCell::new(DeviceManager::new())
    };
);

pub fn search_pci() {
    let mut manager = DEVICE_MANAGER.exclusive_access();
    // 清除已有设备
    manager.devices.clear();
    for device in scan_bus(pci::CSpaceAccessMethod::MemoryMapped).into_iter() {
        manager.push(device);
    }
    // 列出设备，调试用
    // manager.list();
}



// pci设备管理器
pub struct DeviceManager{
    devices: Vec<PCIDevice>,
}

impl DeviceManager {
    /// 创建一个新的设备管理器
    pub fn new() -> Self {
        DeviceManager { devices: Vec::new() }
    }
    /// 添加设备
    pub fn push (&mut self, device: PCIDevice) {
        self.devices.push(device);
    }
    /// 列出设备
    pub fn list(&self) {
        for device in &self.devices {
            println!("PCI Device: vendor_id={:#x}, device_id={:#x}, class={:#x}, subclass={:#x}",
                device.id.vendor_id, device.id.device_id, device.id.class, device.id.subclass);
        }
    }
}
