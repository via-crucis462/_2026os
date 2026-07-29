use super::*;
use alloc::sync::Arc;

use lazy_static::*;


lazy_static! {
    pub static ref NET_DEVICE: Arc<NetDeviceImpl> = Arc::new(NetDeviceImpl::new());
}

#[derive(Debug, Clone, Copy)]
pub enum EthernetError {
    Busy,
    BufferTooSmall,
    Driver,
}

pub trait EthernetDevice {
    fn mac_address(&self) -> [u8; 6];
    fn can_receive(&self) -> bool;
    fn can_transmit(&self) -> bool;
    fn receive_frame(&self, buffer: &mut [u8]) -> Result<usize, EthernetError>;
    fn transmit_frame(&self, frame: &[u8]) -> Result<(), EthernetError>;
}
