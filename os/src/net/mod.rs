// os/src/net/mod.rs
pub mod socket; 
pub mod netlink;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{self, Device, DeviceCapabilities};
use smoltcp::time::Instant;
use lazy_static::lazy_static;
use crate::sync::MPSafeCell;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use crate::drivers::block::NET_DEVICE;
use crate::process::wake_up_one;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spin::Mutex;
use crate::sync::WaitQueue;
use smoltcp::iface::SocketHandle;
use alloc::collections::VecDeque;


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
        //  优先从环回队列拿包
        if let Some(buf) = LOOPBACK_QUEUE.lock().pop_front() {
            return Some((RxToken { buffer: buf }, TxToken));
        }
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

        //  统一环回拦截
        if check_and_handle_loopback(&buffer) {
            return result;
        }

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
    static ref LOOPBACK_QUEUE: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
    pub static ref SOCKET_WAIT_QUEUES: Mutex<BTreeMap<SocketHandle, Arc<Mutex<WaitQueue>>>> = Mutex::new(BTreeMap::new());
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
        let loopback_addr = IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8);
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(ip_addr).unwrap();
            ip_addrs.push(loopback_addr).unwrap();
        });
        iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2)).unwrap();
        MPSafeCell::new(iface)
    };
}
/// 全通用的 L2/L3 本地环回 FIB 拦截器
fn check_and_handle_loopback(packet: &[u8]) -> bool {
    if packet.len() < 14 {
        return false;
    }


    if packet[12] == 0x08 && packet[13] == 0x00 {
        if packet.len() >= 34 {
            let dst_ip = [packet[30], packet[31], packet[32], packet[33]];
            if dst_ip == [127, 0, 0, 1] || dst_ip == [10, 0, 2, 15] {
                let mut loopback_packet = packet.to_vec();
                loopback_packet[0..6].copy_from_slice(&packet[6..12]);
                LOOPBACK_QUEUE.lock().push_back(loopback_packet);
                return true;
            }
        }
    }
    // 2. 拦截 ARP 请求 (EtherType == 0x0806, Opcode == 1)
    else if packet[12] == 0x08 && packet[13] == 0x06 {
        if packet.len() >= 42 {
            let op = [packet[20], packet[21]];
            let target_ip = [packet[38], packet[39], packet[40], packet[41]];
            // 劫持对 127.0.0.1 或本地 IP 的 ARP 请求，并就地伪造响应
            if op == [0x00, 0x01] && (target_ip == [127, 0, 0, 1] || target_ip == [10, 0, 2, 15]) {
                let mut reply = packet.to_vec();
                reply[20..22].copy_from_slice(&[0x00, 0x02]); // 修改为 ARP Reply (2)
                reply[32..38].copy_from_slice(&packet[22..28]); // Target MAC = 请求的 Sender MAC
                reply[38..42].copy_from_slice(&packet[28..32]); // Target IP = 请求中的 Sender IP
                reply[22..28].copy_from_slice(&packet[6..12]);   // Sender MAC = 本地 MAC
                reply[28..32].copy_from_slice(&target_ip);       // Sender IP = 刚才请求的目标 IP
                reply[0..6].copy_from_slice(&packet[6..12]);   // 目的 MAC
                reply[6..12].copy_from_slice(&packet[6..12]);  // 源 MAC

                LOOPBACK_QUEUE.lock().push_back(reply);
                return true;
            }
        }
    }
    false
}
pub fn net_poll() {
    // 通过大循环直至环回队列彻底清空
    // 保证在同一个 poll 调度周期内完成完整的环回包对流
    loop {
        let mut iface = NET_IFACE.exclusive_access();
        let mut sockets = SOCKET_SET.exclusive_access();
        let mut device = VirtioNetDevice;
        let timestamp = Instant::from_millis(crate::arch::timer::get_time_ms() as i64);
        
        iface.poll(timestamp, &mut device, &mut sockets);
        
        // 如果刚才的 poll 动作触发了发送并产生了新的环回包，必须继续循环让 smoltcp 接收它
        if LOOPBACK_QUEUE.lock().is_empty() {
            let queues = crate::net::SOCKET_WAIT_QUEUES.lock();
            for (handle, socket) in sockets.iter_mut() {
                let mut has_data = false;
                match socket {
                    smoltcp::socket::Socket::Raw(raw_sock) => {
                        if raw_sock.can_recv() { has_data = true; }
                    }
                    smoltcp::socket::Socket::Tcp(tcp_sock) => {
                        if tcp_sock.can_recv() { has_data = true; }
                        else if tcp_sock.is_active() && tcp_sock.state() != smoltcp::socket::tcp::State::Listen {
                        has_data = true;
                        }
                        else if !tcp_sock.may_recv() && tcp_sock.state() != smoltcp::socket::tcp::State::Listen {
                            has_data = true;
                        }
                        }
                    smoltcp::socket::Socket::Udp(udp_sock) => {
                        if udp_sock.can_recv() { has_data = true; }
                    }
                    _ => {}
                }
                if has_data {
                    if let Some(queue_arc) = queues.get(&handle) {
                        let queue_guard = queue_arc.lock();
                        if !queue_guard.is_empty() {
                            wake_up_one(queue_guard); 
                        }
                    }
                }
            }
            break;
        }
    }
}