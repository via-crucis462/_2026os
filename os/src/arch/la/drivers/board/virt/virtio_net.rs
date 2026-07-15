use super::virtio_blk::VirtioHal; 
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;
use virtio_drivers_la::device::net::VirtIONet;
use virtio_drivers_la::transport::pci::PciTransport;
use virtio_drivers_la::transport::Transport;
// 网卡驱动的外包装
pub struct VirtIONetWrapper(
    pub MPSafeCell<VirtIONet<VirtioHal, PciTransport, 256>>, 
    pub [u8; 6]
);

impl VirtIONetWrapper {
    pub unsafe fn new(mut transport: PciTransport) -> Self {
        let hal = VirtioHal;
        let mut mac_addr = [0u8; 6];
        for i in 0..6 {
            if let Ok(val) = transport.read_config_space::<u8>(i as usize) {
                mac_addr[i] = val;
            }
        }
        let net = VirtIONet::new(transport, 2048).expect("Failed to initialize VirtIONet");
        Self(MPSafeCell::new(net), mac_addr) 
    }
    pub unsafe fn visit(&self) -> spin::MutexGuard<'_, VirtIONet<VirtioHal, PciTransport, 256>> {
        self.0.exclusive_access() 
    }

    pub fn get_mac_address(&self) -> [u8; 6] {
        self.1 
    }
}