use crate::drivers::net::{EthernetDevice, EthernetError};
use crate::sync::MPSafeCell;
use spin::MutexGuard;
// 由于龙芯文档中缺乏具体协议描述，这里参考 2025 RocketOS 和 Uboot 源码

/// 2K1000 GMAC MAC 寄存器偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum LA2k1000GmacReg {
    Config = 0x0000,
    FrameFilter = 0x0004,
    GmiiAddr = 0x0010,
    GmiiData = 0x0014,
    FlowControl = 0x0018,
    Version = 0x0020,
    InterruptStatus = 0x0038,
    InterruptMask = 0x003C,
    Addr0High = 0x0040,
    Addr0Low = 0x0044,
    RgsmiiStatus = 0x00D8,
}

impl LA2k1000GmacReg {
    pub const ALL: &'static [Self] = &[
        Self::Config,
        Self::FrameFilter,
        Self::GmiiAddr,
        Self::GmiiData,
        Self::FlowControl,
        Self::Version,
        Self::InterruptStatus,
        Self::InterruptMask,
        Self::Addr0High,
        Self::Addr0Low,
        Self::RgsmiiStatus,
    ];

    pub const fn offset(self) -> usize {
        self as usize
    }
}

/// 2K1000 GMAC DMA 寄存器偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum LA2k1000DmaReg {
    BusMode = 0x0000,
    TxPollDemand = 0x0004,
    RxPollDemand = 0x0008,
    RxBaseAddr = 0x000C,
    TxBaseAddr = 0x0010,
    Status = 0x0014,
    Control = 0x0018,
    Interrupt = 0x001C,
    TxCurrDesc = 0x0048,
    RxCurrDesc = 0x004C,
    TxCurrAddr = 0x0050,
    RxCurrAddr = 0x0054,
    HwFeature = 0x0058,
}

impl LA2k1000DmaReg {
    pub const ALL: &'static [Self] = &[
        Self::BusMode,
        Self::TxPollDemand,
        Self::RxPollDemand,
        Self::RxBaseAddr,
        Self::TxBaseAddr,
        Self::Status,
        Self::Control,
        Self::Interrupt,
        Self::TxCurrDesc,
        Self::RxCurrDesc,
        Self::TxCurrAddr,
        Self::RxCurrAddr,
        Self::HwFeature,
    ];

    pub const fn offset(self) -> usize {
        self as usize
    }
}

pub struct LA2k1000NetDevice {
    // TODO: 真实硬件寄存器映射等
}

// 网卡驱动的外包装
pub struct LA2k1000NetWrapper(pub MPSafeCell<LA2k1000NetDevice>, pub [u8; 6]);

impl LA2k1000NetWrapper {
    pub fn new() -> Self {
        Self(MPSafeCell::new(LA2k1000NetDevice {}), [0, 0, 0, 0, 0, 0])
    }
    pub fn visit(&self) -> MutexGuard<'_, LA2k1000NetDevice> {
        self.0.exclusive_access()
    }

    pub fn get_mac_address(&self) -> [u8; 6] {
        self.1
    }
}

impl EthernetDevice for LA2k1000NetWrapper {
    fn mac_address(&self) -> [u8; 6] {
        self.get_mac_address()
    }

    fn can_receive(&self) -> bool {
        self.0.exclusive_access().can_recv()
    }

    fn can_transmit(&self) -> bool {
        self.0.exclusive_access().can_send()
    }

    fn receive_frame(&self, _buffer: &mut [u8]) -> Result<usize, EthernetError> {
        Err(EthernetError::Busy)
    }

    fn transmit_frame(&self, _frame: &[u8]) -> Result<(), EthernetError> {
        Err(EthernetError::Busy)
    }
}

impl LA2k1000NetDevice {
    /// 是否有待接收的包
    pub fn can_recv(&self) -> bool {
        false
    }

    /// 是否可以发送
    pub fn can_send(&self) -> bool {
        false
    }

    /// 获取 MAC 地址
    pub fn mac_address(&self) -> [u8; 6] {
        [0u8; 6]
    }
}
