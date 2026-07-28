use core::ptr::NonNull;

use super::virtio_blk::VirtioHal;
use crate::drivers::net::{EthernetDevice, EthernetError};
use crate::sync::MPSafeCell;
use crate::MMIO_SLOT_SIZE;
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

impl EthernetDevice for VirtIONetWrapper {
    fn mac_address(&self) -> [u8; 6] {
        self.get_mac_address()
    }

    fn can_receive(&self) -> bool {
        self.0.exclusive_access().can_recv()
    }

    fn can_transmit(&self) -> bool {
        self.0.exclusive_access().can_send()
    }

    fn receive_frame(&self, buffer: &mut [u8]) -> Result<usize, EthernetError> {
        let mut driver = self.0.exclusive_access();
        let rx_buf = driver.receive().map_err(|_| EthernetError::Busy)?;
        let packet = rx_buf.packet();
        if packet.len() > buffer.len() {
            driver
                .recycle_rx_buffer(rx_buf)
                .map_err(|_| EthernetError::Driver)?;
            return Err(EthernetError::BufferTooSmall);
        }
        let length = packet.len();
        buffer[..length].copy_from_slice(packet);
        driver
            .recycle_rx_buffer(rx_buf)
            .map_err(|_| EthernetError::Driver)?;
        Ok(length)
    }

    fn transmit_frame(&self, frame: &[u8]) -> Result<(), EthernetError> {
        let mut driver = self.0.exclusive_access();
        if !driver.can_send() {
            return Err(EthernetError::Busy);
        }
        let mut tx_buf = driver.new_tx_buffer(frame.len());
        tx_buf.packet_mut().copy_from_slice(frame);
        driver.send(tx_buf).map_err(|_| EthernetError::Driver)
    }
}
