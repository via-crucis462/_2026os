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
