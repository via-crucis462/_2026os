use core::ptr::NonNull;

use super::virtio_blk::VirtioHal;
use crate::{sync::MPSafeCell, MMIO_SLOT_SIZE};
use virtio_drivers::{
    device::net::VirtIONet,
    transport::{
        mmio::{MmioTransport, VirtIOHeader},
        DeviceType, Transport,
    },
};

const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
const VIRTIO_MMIO_SLOTS: usize = 8;

// 网卡驱动的外包装
pub struct VirtIONetWrapper(
    pub MPSafeCell<VirtIONet<VirtioHal, MmioTransport<'static>, 256>>,
    pub [u8; 6],
);

impl VirtIONetWrapper {
    /// 动态扫描 MMIO 区域，找到并初始化 VirtIO 网卡
    pub fn new() -> Self {
        let transport = (0..VIRTIO_MMIO_SLOTS)
            .find_map(|index| {
                let addr = VIRTIO_MMIO_BASE + index * MMIO_SLOT_SIZE;
                let transport = unsafe {
                    MmioTransport::new(
                        NonNull::new(addr as *mut VirtIOHeader).unwrap(),
                        MMIO_SLOT_SIZE,
                    )
                }
                .ok()?;

                (transport.device_type() == DeviceType::Network).then_some(transport)
            })
            .expect("virtio-net device not found");

        let mut mac_addr = [0u8; 6];
        for (index, byte) in mac_addr.iter_mut().enumerate() {
            if let Ok(value) = transport.read_config_space::<u8>(index) {
                *byte = value;
            }
        }

        let net = VirtIONet::new(transport, 2048).expect("Failed to initialize VirtIONet");
        Self(MPSafeCell::new(net), mac_addr)
    }

    pub fn visit(&self) -> spin::MutexGuard<'_, VirtIONet<VirtioHal, MmioTransport<'static>, 256>> {
        self.0.exclusive_access()
    }

    pub fn get_mac_address(&self) -> [u8; 6] {
        self.1
    }
}
