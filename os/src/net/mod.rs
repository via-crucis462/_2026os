// os/src/net/mod.rs
pub mod socket; // 声明我们有一个 socket 子模块！

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{self, Device, DeviceCapabilities};
use smoltcp::time::Instant;
use lazy_static::lazy_static;
use crate::sync::MPSafeCell;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use crate::drivers::block::NET_DEVICE;

pub struct VirtioNetDevice;

pub struct RxToken {
    buffer: Vec<u8>,
}
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IoVec {
    pub iov_base: usize, // 指向缓冲区的指针
    pub iov_len: usize,  // 缓冲区长度
}

/// recvmsg / sendmsg 的核心控制结构体
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MsgHdr {
    pub msg_name: usize,       // 目标/源地址指针 (sockaddr)
    pub msg_namelen: u32,      // 地址长度
    pub _pad1: u32,            // 64 位对齐填充位
    pub msg_iov: usize,        // IoVec 数组指针
    pub msg_iovlen: usize,     // IoVec 数组的元素个数
    pub msg_control: usize,    // 辅助数据指针 (不用管它)
    pub msg_controllen: usize, // 辅助数据长度
    pub msg_flags: i32,        // 接收标志位
    pub _pad2: i32,
}
pub struct TxToken;

impl Device for VirtioNetDevice {
    type RxToken<'a> = RxToken where Self: 'a;
    type TxToken<'a> = TxToken where Self: 'a;
    #[cfg(target_arch = "riscv64")]
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut driver = NET_DEVICE.0.exclusive_access();
        if driver.can_recv() {
            let mut buf = vec![0u8; 2048];
            
            if let Ok(len) = driver.recv(&mut buf) {
                buf.truncate(len); 
                return Some((RxToken { buffer: buf }, TxToken));
            }   
        }
        None
    }
    #[cfg(target_arch = "loongarch64")]
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut driver = NET_DEVICE.0.exclusive_access();
        if driver.can_recv() {
            if let Ok(buf) = driver.receive() {
                let bytes = buf.as_bytes();
                let mut vec_buf = bytes.to_vec();
                return Some((RxToken { buffer: vec_buf }, TxToken));
            }
        }
        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let driver = NET_DEVICE.0.exclusive_access();
        if driver.can_send() {
            Some(TxToken)
        } else {
            None
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps
    }
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

impl phy::TxToken for TxToken {
    #[cfg(target_arch = "riscv64")]
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer); 
        let mut driver = NET_DEVICE.0.exclusive_access();
        driver.send(&buffer).expect("Failed to send network packet");
        result
    }
    #[cfg(target_arch = "loongarch64")]
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut driver = NET_DEVICE.0.exclusive_access();
        let mut tx_buf = driver.new_tx_buffer(len);
        let result = f(tx_buf.packet_mut()); 
        driver.send(tx_buf).expect("Failed to send network packet");
        result
    }
}

lazy_static! {
    pub static ref SOCKET_SET: MPSafeCell<SocketSet<'static>> = MPSafeCell::new(SocketSet::new(vec![]));

    pub static ref NET_IFACE: MPSafeCell<Interface> = {

        #[cfg(target_arch = "riscv64")]
        let mac = NET_DEVICE.0.exclusive_access().mac();

        #[cfg(target_arch = "loongarch64")]
        let mac = NET_DEVICE.0.exclusive_access().mac_address();
        
        let mac_addr = EthernetAddress::from_bytes(&mac);
        let mut config = Config::new(HardwareAddress::Ethernet(mac_addr));
        config.random_seed = 0x1122334455667788; 
        let mut device = VirtioNetDevice;
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));

        let ip_addr = IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24);
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(ip_addr).unwrap();
        });
        iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2)).unwrap();
        MPSafeCell::new(iface)
    };
}

pub fn net_poll() {
    let mut iface = NET_IFACE.exclusive_access();
    let mut sockets = SOCKET_SET.exclusive_access();
    let mut device = VirtioNetDevice;
    let timestamp = Instant::from_millis(crate::arch::timer::get_time_ms() as i64);
    iface.poll(timestamp, &mut device, &mut sockets);
}