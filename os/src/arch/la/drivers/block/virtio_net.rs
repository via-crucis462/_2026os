use super::virtio_blk::VirtioHal; 
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;
use virtio_drivers_la::device::net::VirtIONet;
use virtio_drivers_la::transport::pci::PciTransport;

// 网卡驱动的外包装
pub struct VirtIONetWrapper(pub MPSafeCell<VirtIONet<VirtioHal, PciTransport, 256>>);

impl VirtIONetWrapper {
    /// pci扫描创建新网卡驱动
    pub unsafe fn new(transport: PciTransport) -> Self {
        let hal = VirtioHal;
        let net = VirtIONet::new(transport, 2048).expect("Failed to initialize VirtIONet");
        Self (MPSafeCell::new(net)) 
    }
    pub unsafe fn visit(&self) -> spin::MutexGuard<'_, VirtIONet<VirtioHal, PciTransport, 256>> {
        self.0.exclusive_access()
    }
}