// os/src/net/mod.rs
pub mod netlink;
pub mod socket;
use crate::drivers::net::EthernetDevice;
use crate::drivers::net::NET_DEVICE;
use crate::process::wake_up_one;
use crate::process::TaskStatus;
use crate::sync::MPSafeCell;
use crate::sync::WaitQueue;
use alloc::collections::VecDeque;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use smoltcp::iface::SocketHandle;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use spin::Mutex;

/// 将底层驱动封装为 smoltcp 的网卡结构体
pub struct SmoltcpDevice<'a, D: EthernetDevice> {
    device: &'a D,
}

impl<'a, D: EthernetDevice> SmoltcpDevice<'a, D> {
    pub fn new(device: &'a D) -> Self {
        Self { device }
    }
}

pub struct RxToken {
    buffer: Vec<u8>,
}

pub struct TxToken<'a, D: EthernetDevice> {
    device: &'a D,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

impl<'a, D: EthernetDevice> phy::TxToken for TxToken<'a, D> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        self.device
            .transmit_frame(&buffer)
            .expect("Failed to send network packet");
        result
    }
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

impl<D: EthernetDevice> Device for SmoltcpDevice<'_, D> {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a, D>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if !self.device.can_receive() {
            return None;
        }

        let mut buffer = vec![0u8; 2048];
        if let Ok(length) = self.device.receive_frame(&mut buffer) {
            buffer.truncate(length);
            return Some((
                RxToken { buffer },
                TxToken {
                    device: self.device,
                },
            ));
        }
        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.device.can_transmit() {
            Some(TxToken {
                device: self.device,
            })
        } else {
            None
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps.medium = smoltcp::phy::Medium::Ethernet;
        caps
    }
}

pub struct SocketWaitQueue {
    pub rx_queue: Arc<MPSafeCell<WaitQueue>>, // 读操作阻塞
    pub tx_queue: Arc<MPSafeCell<WaitQueue>>, // 写操作阻塞
}
impl SocketWaitQueue {
    pub fn new() -> Self {
        Self {
            rx_queue: Arc::new(MPSafeCell::new(WaitQueue::new())),
            tx_queue: Arc::new(MPSafeCell::new(WaitQueue::new())),
        }
    }
}
lazy_static! {
    pub static ref SOCKET_SET: MPSafeCell<SocketSet<'static>> = MPSafeCell::new(SocketSet::new(vec![]));
    // 原本是 Arc<Mutex<WaitQueue>>，现在统一改为 Arc<MPSafeCell<WaitQueue>>
    pub static ref SOCKET_WAIT_QUEUES: Mutex<BTreeMap<SocketHandle, SocketWaitQueue>> = Mutex::new(BTreeMap::new());
    /// 用户态最后一个文件引用关闭、但仍需完成 TCP FIN 握手的 socket。
    /// 这些 handle 不再有 syscall 等待者，由 net_poll 在 Closed 后回收。
    pub static ref ORPHANED_TCP_SOCKETS: Mutex<BTreeSet<SocketHandle>> =
        Mutex::new(BTreeSet::new());
    pub static ref LOOPBACK_DEVICE: MPSafeCell<smoltcp::phy::Loopback> = {
        MPSafeCell::new(smoltcp::phy::Loopback::new(smoltcp::phy::Medium::Ethernet))
    };
    pub static ref LO_IFACE: MPSafeCell<Interface> = {
        // 给 lo 接口分配一个全零的虚拟 MAC 地址
        let dummy_mac = EthernetAddress::from_bytes(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let mut config = Config::new(HardwareAddress::Ethernet(dummy_mac));

        let mut device = LOOPBACK_DEVICE.exclusive_access();
        let mut iface = Interface::new(config, &mut *device, Instant::from_millis(0));

        // 只绑定 127.0.0.1/8
        let loopback_addr = IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8);
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(loopback_addr).unwrap();
        });
        MPSafeCell::new(iface)
    };
    pub static ref NET_IFACE: MPSafeCell<Interface> = {

        let mac = NET_DEVICE.mac_address();

        let mac_addr = EthernetAddress::from_bytes(&mac);
        let mut config = Config::new(HardwareAddress::Ethernet(mac_addr));
        config.random_seed = 0x1122334455667788;
        let mut device = SmoltcpDevice::new(NET_DEVICE.as_ref());
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));


        #[cfg(board = "visionfive2")]
        let (ip_addr, gateway) = (
            IpCidr::new(IpAddress::v4(192, 168, 1, 101), 24),
            Ipv4Address::new(192, 168, 1, 100),
        );
        #[cfg(not(board = "visionfive2"))]
        let (ip_addr, gateway) = (
            IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24),
            Ipv4Address::new(10, 0, 2, 2),
        );
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(ip_addr).unwrap();
        });
        iface
            .routes_mut()
            .add_default_ipv4_route(gateway)
            .unwrap();
        MPSafeCell::new(iface)
    };
}

pub fn net_poll() {
    let mut eth_iface = NET_IFACE.exclusive_access();
    let mut lo_iface = LO_IFACE.exclusive_access();
    let mut sockets = SOCKET_SET.exclusive_access();
    let mut eth_device = SmoltcpDevice::new(NET_DEVICE.as_ref());
    let mut lo_device = LOOPBACK_DEVICE.exclusive_access();
    let mut budget = 32;
    while budget > 0 {
        budget -= 1;
        let timestamp = Instant::from_millis(crate::arch::timer::get_time_ms() as i64);
        let lo_active = lo_iface.poll(timestamp, &mut *lo_device, &mut sockets);
        let eth_active = eth_iface.poll(timestamp, &mut eth_device, &mut sockets);
        if !lo_active && !eth_active {
            break;
        }
    }

    let mut dead_handles = alloc::vec::Vec::new();
    let queues = crate::net::SOCKET_WAIT_QUEUES.lock();
    let mut orphaned_tcp = crate::net::ORPHANED_TCP_SOCKETS.lock();
    for (handle, socket) in sockets.iter_mut() {
        let mut can_read = false;
        let mut can_write = false;
        match socket {
            smoltcp::socket::Socket::Raw(raw_sock) => {
                can_read = raw_sock.can_recv();
                can_write = raw_sock.can_send();
            }
            smoltcp::socket::Socket::Tcp(tcp_sock) => {
                let is_listening = tcp_sock.state() == smoltcp::socket::tcp::State::Listen;
                let is_closed = tcp_sock.state() == smoltcp::socket::tcp::State::Closed;
                let is_eof = !tcp_sock.may_recv() && !is_listening && !is_closed;
                // 处于 Listen 状态，且 is_active 为 true 时，有新连接到来
                let has_new_connection = is_listening && tcp_sock.is_active();
                can_read = tcp_sock.can_recv() || is_eof || has_new_connection;
                // 可写：发送缓冲区有空余空间
                can_write = tcp_sock.can_send();
            }
            smoltcp::socket::Socket::Udp(udp_sock) => {
                can_read = udp_sock.can_recv();
                can_write = udp_sock.can_send();
            }
            _ => {}
        }
        if let Some(socket_wait) = queues.get(&handle) {
            if can_read {
                let has_waiting_task = {
                    let rx_guard = socket_wait.rx_queue.exclusive_access();
                    !rx_guard.is_empty()
                };
                if has_waiting_task {
                    crate::task::wake_up_one(socket_wait.rx_queue.get_mutex());
                }
            }
            if can_write {
                let has_waiting_task = {
                    let tx_guard = socket_wait.tx_queue.exclusive_access();
                    !tx_guard.is_empty()
                };
                if has_waiting_task {
                    crate::task::wake_up_one(socket_wait.tx_queue.get_mutex());
                }
            }
        }
        if let smoltcp::socket::Socket::Tcp(tcp_socket) = socket {
            if tcp_socket.state() == smoltcp::socket::tcp::State::Closed {
                if orphaned_tcp.contains(&handle) || !queues.contains_key(&handle) {
                    dead_handles.push(handle);
                }
            }
        }
    }
    for handle in dead_handles {
        sockets.remove(handle);
        orphaned_tcp.remove(&handle);
    }
}
