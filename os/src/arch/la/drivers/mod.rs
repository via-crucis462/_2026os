//! block device driver

pub mod board;
pub mod pci;
pub mod dma;
pub use board::*;

use alloc::vec::Vec;
use lazy_static::lazy_static;
use crate::{drivers::pci::{PCIDevice,scan_bus}, sync::MPSafeCell};

pub enum DeviceType {
    VirtIOBlock,
    VirtIONet,
    //
}

lazy_static!(
    pub static ref DEVICE_MANAGER: MPSafeCell<DeviceManager> = MPSafeCell::new(DeviceManager::new());
);

pub fn search_pci() {
    let mut manager = DEVICE_MANAGER.exclusive_access();
    // 清除已有设备
    manager.devices.clear();
    for device in scan_bus(pci::CSpaceAccessMethod::MemoryMapped).into_iter() {
        trace!("Found PCI device: bus={} dev={} func={}",
            device.loc.bus as u32, device.loc.device as u32, device.loc.function as u32);
        trace!("  vendor_id=0x{:x}", device.id.vendor_id as u32);
        trace!("  device_id=0x{:x}", device.id.device_id as u32);
        trace!("  class=0x{:x} subclass=0x{:x}", device.id.class as u32, device.id.subclass as u32);
        manager.push(device);
        trace!("  device pushed to manager");
    }
    println!("Total PCI devices found: {}", manager.devices.len() as u64);
    // 列出设备，调试用
    manager.list();
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
            println!("PCI Device: vendor_id=0x{:x}, device_id=0x{:x}, class=0x{:x}, subclass=0x{:x}",
                device.id.vendor_id as u32, device.id.device_id as u32, device.id.class as u32, device.id.subclass as u32);
            info!("Location: bus={} dev={} func={}", device.loc.bus as u32, device.loc.device as u32, device.loc.function as u32);
        }
    }
    pub fn get_devices(&self) -> &Vec<PCIDevice> {
        &self.devices
    }
}
