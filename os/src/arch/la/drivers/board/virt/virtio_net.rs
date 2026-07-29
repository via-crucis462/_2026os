use super::virtio_blk::VirtioHal;
use crate::drivers::net::{EthernetDevice, EthernetError};
use virtio_drivers::device::net::VirtIONet;
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::Transport;

use spin::Mutex;
// 网卡驱动的外包装
pub struct VirtIONetWrapper{
    pub inner: Mutex<VirtIONet<VirtioHal, PciTransport, 256>>,
}

impl VirtIONetWrapper {
    pub fn new() -> Self {
        let hal = VirtioHal;
        let transport = super::pci::scan_and_init_pci_device_to_trans(
            super::DeviceType::VirtIONet
        ).expect("Failed to find PCI Net device");
        let net = VirtIONet::new(transport, 2048)
            .expect("Failed to initialize VirtIONet");
        Self { inner: Mutex::new(net) }
    }
    pub fn visit(&self) -> spin::MutexGuard<'_, VirtIONet<VirtioHal, PciTransport, 256>> {
        self.inner.lock()
    }
}

impl EthernetDevice for VirtIONetWrapper {
    fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().mac_address()
    }

    fn can_receive(&self) -> bool {
        self.inner.lock().can_recv()
    }

    fn can_transmit(&self) -> bool {
        self.inner.lock().can_send()
    }

    fn receive_frame(&self, buffer: &mut [u8]) -> Result<usize, EthernetError> {
        let mut driver = self.inner.lock();
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
        let mut driver = self.inner.lock();
        if !driver.can_send() {
            return Err(EthernetError::Busy);
        }

        let mut tx_buf = driver.new_tx_buffer(frame.len());
        tx_buf.packet_mut().copy_from_slice(frame);
        driver.send(tx_buf).map_err(|_| EthernetError::Driver)
    }
}
