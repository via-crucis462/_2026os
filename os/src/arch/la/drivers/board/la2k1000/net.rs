use crate::sync::MPSafeCell;
use spin::MutexGuard;
use alloc::vec::Vec;

pub struct LA2k1000NetDevice {
    // TODO: 真实硬件寄存器映射等
}

// 网卡接收令牌
pub struct LA2k1000RxToken {
    buffer: Vec<u8>,
}

impl LA2k1000RxToken {
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer
    }
}

// 网卡发送令牌
pub struct LA2k1000TxToken {
    buffer: Vec<u8>,
}

impl LA2k1000TxToken {
    pub fn packet_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }
}

// 网卡驱动的外包装
pub struct LA2k1000NetWrapper(
    pub MPSafeCell<LA2k1000NetDevice>, 
    pub [u8; 6]
);

impl LA2k1000NetWrapper {
    pub unsafe fn new() -> Self {
        Self(
            MPSafeCell::new(LA2k1000NetDevice{}),
            [0, 0, 0, 0, 0, 0]
        )
    }
    pub unsafe fn visit(&self) -> MutexGuard<'_, LA2k1000NetDevice> {
        self.0.exclusive_access()
    }

    pub fn get_mac_address(&self) -> [u8; 6] {
        self.1 
    }
}

impl LA2k1000NetDevice {
    /// 是否有待接收的包
    pub fn can_recv(&self) -> bool {
        false
    }

    /// 接收一个网络包
    pub fn receive(&mut self) -> Result<LA2k1000RxToken, &'static str> {
        Err("LA2k1000 net receive: not implemented")
    }

    /// 是否可以发送
    pub fn can_send(&self) -> bool {
        false
    }

    /// 申请一个发送缓冲区
    pub fn new_tx_buffer(&mut self, len: usize) -> LA2k1000TxToken {
        LA2k1000TxToken {
            buffer: alloc::vec![0u8; len],
        }
    }

    /// 发送网络包
    pub fn send(&mut self, _tx_buf: LA2k1000TxToken) -> Result<(), &'static str> {
        Err("LA2k1000 net send: not implemented")
    }

    /// 获取 MAC 地址
    pub fn mac_address(&self) -> [u8; 6] {
        [0u8; 6]
    }
}