// os/src/net/socket.rs
use alloc::vec::Vec;
use core::any::Any;
use smoltcp::socket::tcp::{Socket as TcpSocketSmol, SocketBuffer};
use smoltcp::iface::SocketHandle;
use alloc::collections::{BTreeMap, VecDeque};
use spin::Mutex;
use alloc::sync::{Arc, Weak};
use lazy_static::lazy_static;
use smoltcp::wire::{IpAddress, IpEndpoint};
use crate::net::SOCKET_SET;
use crate::fs::{File, Stat};    // 引入 File trait 和 Stat
use crate::mm::UserBuffer;      // 引入 UserBuffer
use crate::net::vec;
use crate::auth::{PermStat, FileMode}; // 引入权限相关的类型
use smoltcp::socket::raw::{Socket as RawSocketSmol, PacketBuffer as RawPacketBuffer, PacketMetadata as RawPacketMetadata};
use smoltcp::wire::{IpVersion, IpProtocol};
use crate::sync::WaitQueue;
use crate::process::current_task_to_sleep;
use smoltcp::socket::udp;

pub struct TcpSocket {
    pub handle: SocketHandle,
    // 暂存 bind 分配或指定的本地端口
    pub local_port: Mutex<Option<u16>>,
}

impl TcpSocket {
    pub fn new() -> Self {
        let rx_buffer = SocketBuffer::new(vec![0; 8192]);
        let tx_buffer = SocketBuffer::new(vec![0; 8192]);
        let socket = TcpSocketSmol::new(rx_buffer, tx_buffer);
        let handle = SOCKET_SET.exclusive_access().add(socket);
        Self { handle ,local_port: Mutex::new(None)}
    }
    pub fn disconnect(&self) {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.close(); 
    }
    pub fn local_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
        let sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.local_endpoint()
    }
    pub fn remote_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
        let sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.remote_endpoint()
    }
    pub fn connect(&self, remote_ep: smoltcp::wire::IpEndpoint) -> isize {
       println!("[TCP Connect] Attempting to connect to {}, using handle {:?}", remote_ep, self.handle);
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        let is_loopback = match remote_ep.addr {
            smoltcp::wire::IpAddress::Ipv4(v4) => v4.as_bytes()[0] == 127,
            _ => false,
        };
        let mut port_lock = self.local_port.lock();
        let local_port = if let Some(p) = *port_lock {
            p
        } else {
            let new_port = (crate::arch::timer::get_time_us() % 16384 + 49152) as u16;
            *port_lock = Some(new_port);
            new_port
        };
        
        let res = 
        if is_loopback {
            let mut lo_iface = crate::net::LO_IFACE.exclusive_access();
            socket.connect(lo_iface.context(), remote_ep, local_port)
        } 
        else {
            let mut eth_iface = crate::net::NET_IFACE.exclusive_access();
            socket.connect(eth_iface.context(), remote_ep, local_port)
        };
        let connect_status = match res {
        Ok(_) => 0, 
        Err(_) => crate::syscall::errno::Errno::ECONNREFUSED.as_isize(),
    };
        drop(sockets);

       loop {
            crate::net::net_poll(); 

            let sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get::<smoltcp::socket::tcp::Socket>(self.handle);
            
            use smoltcp::socket::tcp::State;
            match socket.state() {
                State::Established => {
                    return 0; 
                }
                State::SynSent | State::SynReceived => {
                    drop(sockets);
                    crate::task::suspend_current_and_run_next(); 
                }
                _ => {
                    return crate::syscall::errno::Errno::ECONNREFUSED.as_isize();
                }
            }
        }
    }
}

impl File for TcpSocket {
    fn readable(&self) -> bool {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.can_recv() || !socket.may_recv()
    }
    fn writable(&self) -> bool {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.can_send() || !socket.may_send()
    }

    fn read(&self, mut buf: UserBuffer) -> usize {
        loop {
            crate::net::net_poll();
            let mut sockets = SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
            if !socket.may_recv() && !socket.can_recv() {
                return 0; 
            }
            if socket.can_recv() {
                let mut temp_buf = vec![0u8; buf.len()];
                if let Ok(recv_len) = socket.recv_slice(&mut temp_buf) {
                    if recv_len > 0 {
                        println!("TcpSocket read SUCCESS: got {} bytes", recv_len);
                        let mut current = 0;
                        for buffer in buf.buffers.iter_mut() {
                            let copy_len = buffer.len().min(recv_len.saturating_sub(current));
                            if copy_len == 0 { break; }
                            buffer[..copy_len].copy_from_slice(&temp_buf[current..current + copy_len]);
                            current += copy_len;
                            if current == recv_len { break; }
                        }
                        return current; 
                    }
                }
            }
            drop(sockets); 
            crate::task::suspend_current_and_run_next();
        }
    }
    fn write(&self, buf: UserBuffer) -> usize {
        println!("TcpSocket write called with {} bytes", buf.len());
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);

        if !socket.may_send() {
            return 0; 
        }

        let mut temp_buf = vec![0u8; buf.len()];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            temp_buf[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }
        let write_len = socket.send_slice(&temp_buf).unwrap_or(0);
        drop(sockets); 
        if write_len > 0 {
             crate::net::net_poll();
            }
            write_len
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 0,
            mode: 0o140000 | 0o666, 
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 0,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2], // 如果这里报错说类型不匹配，可能需要改成 [0; 2] 或者其他数组形式
        }
    }

    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}
lazy_static! {

    static ref LOCAL_UDP_SOCKETS: Mutex<BTreeMap<u16, Arc<Mutex<VecDeque<(IpEndpoint, Vec<u8>)>>>>> = Mutex::new(BTreeMap::new());
}
pub struct UdpSocket {
    pub handle: smoltcp::iface::SocketHandle,
    //远端地址队列
    pub remote_ep: Mutex<Option<IpEndpoint>>, 
    pub local_port: Mutex<Option<u16>>,
}

impl UdpSocket {
    pub fn new() -> Self {
        // 分配 16 个包的元数据空间，和 16KB 的数据缓存空间
        let rx_buffer = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0; 16384]
        );
        let tx_buffer = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0; 16384]
        );
        let socket = udp::Socket::new(rx_buffer, tx_buffer);
        // 将 socket 加入全局协议栈 SOCKET_SET
        let handle = crate::net::SOCKET_SET.exclusive_access().add(socket);
        
        Self { 
            handle,
            remote_ep: Mutex::new(None),
            local_port: Mutex::new(None),
        }
    }
    pub fn bind(&self, port: u16) -> isize {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<udp::Socket>(self.handle);
        let mut actual_port = port;
        if actual_port == 0 {
            actual_port = (crate::arch::timer::get_time_us() % 16384 + 49152) as u16;
        }
        match socket.bind(actual_port) {
            Ok(_) =>{
                *self.local_port.lock() = Some(actual_port);
                0
            },
            Err(_) => crate::syscall::errno::Errno::EADDRINUSE.as_isize(),
        }
    }
    pub fn connect(&self, remote_ep: IpEndpoint) -> isize {
        println!("[UDP Connect] Setting remote to {}", remote_ep);
        let mut remote = self.remote_ep.lock();
        *remote = Some(remote_ep);
        let mut port_lock = self.local_port.lock();
        if port_lock.is_none() {
            // 分配临时端口 
            let ephemeral_port = (crate::arch::timer::get_time_us() % 16384 + 49152) as u16;
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
            if socket.bind(ephemeral_port).is_ok() {
                *port_lock = Some(ephemeral_port);
            }
        }
        0
    }
    pub fn disconnect(&self) -> isize {
        let mut remote = self.remote_ep.lock();
        *remote = None;
        0
    }
    pub fn sendto(&self, buf: &[u8], remote_ep: IpEndpoint) -> isize {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<udp::Socket>(self.handle);
        if !socket.can_send() {
            return crate::syscall::errno::Errno::EAGAIN.as_isize();
        }
        match socket.send_slice(buf, remote_ep) {
            Ok(_) => buf.len() as isize,
            Err(_) => crate::syscall::errno::Errno::ECONNREFUSED.as_isize(),
        }
    }
    /// 处理 UdpMetadata，提取真实 Endpoint
    pub fn recvfrom(&self, buf: &mut [u8]) -> Option<(usize, IpEndpoint)> {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<udp::Socket>(self.handle);
        if !socket.can_recv() {
            return None; // 暂无数据
        }
        match socket.recv_slice(buf) {
            Ok((len, meta)) => {
                Some((len, meta.endpoint))
            },
            Err(_) => None,
        }
    }
}
// 实现 File trait，使其能放进系统的 fd_table 中
impl File for UdpSocket {
    fn readable(&self) -> bool {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
        socket.can_recv()
    }
    fn writable(&self) -> bool {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
        socket.can_send()
    }
    
    fn read(&self, mut buf: UserBuffer) -> usize {
        loop {
            crate::net::net_poll();
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
            if socket.can_recv() {
                let mut temp_buf = alloc::vec![0u8; 16384];
                match socket.recv_slice(&mut temp_buf) {
                    Ok((recv_len, _meta)) => {
                        let mut current = 0;
                        for buffer in buf.buffers.iter_mut() {
                            let copy_len = buffer.len().min(recv_len.saturating_sub(current));
                            if copy_len == 0 {
                                break;
                            }
                            buffer[..copy_len].copy_from_slice(&temp_buf[current..current + copy_len]);
                            current += copy_len;
                            if current == recv_len { 
                                break; 
                            }
                        }
                        return current; 
                    }
                    Err(_) => {
                    }
                }
            }
            drop(sockets);
            crate::task::suspend_current_and_run_next();
        }
    }

    fn write(&self, buf: UserBuffer) -> usize {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
        
        if !socket.can_send() {
            return 0;
        }
        let mut temp_buf = alloc::vec![];
        for buffer in buf.buffers.iter() {
            temp_buf.extend_from_slice(buffer);
        }
        let remote = *self.remote_ep.lock();
        if let Some(remote_ep) = remote {
            match socket.send_slice(&temp_buf, remote_ep) {
                Ok(_) => temp_buf.len(),
                Err(_) => 0,
            }
        } else {
            0 
        }
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 0, mode: 0o140000 | 0o666, // 标记为 Socket 类型
            nlink: 1, uid: 0, gid: 0, rdev: 0, __pad: 0,
            size: 0, blksize: 0, __pad2: 0, blocks: 0,
            atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }

    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }

    
    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketType {
    Stream,
    Datagram,
}

struct UnixSocketInner {
    recv_queue: VecDeque<Vec<u8>>,
    peer: Option<Weak<Mutex<UnixSocketInner>>>,
    attached_prog: Option<usize>,
    socket_type: UnixSocketType,
}

pub struct UnixSocket {
    inner: Arc<Mutex<UnixSocketInner>>,
}

impl UnixSocket {
    pub fn pair(socket_type: UnixSocketType) -> (Self, Self) {
        let left = Arc::new(Mutex::new(UnixSocketInner {
            recv_queue: VecDeque::new(),
            peer: None,
            attached_prog: None,
            socket_type,
        }));
        let right = Arc::new(Mutex::new(UnixSocketInner {
            recv_queue: VecDeque::new(),
            peer: None,
            attached_prog: None,
            socket_type,
        }));
        left.lock().peer = Some(Arc::downgrade(&right));
        right.lock().peer = Some(Arc::downgrade(&left));
        (
            Self { inner: left },
            Self { inner: right },
        )
    }

    pub fn attach_bpf(&self, prog_fd: usize) {
        self.inner.lock().attached_prog = Some(prog_fd);
    }
}

impl File for UnixSocket {
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut inner = self.inner.lock();
        let Some(packet) = inner.recv_queue.pop_front() else {
            return 0;
        };
        let mut copied = 0usize;
        for segment in buf.buffers.iter_mut() {
            let copy_len = segment.len().min(packet.len().saturating_sub(copied));
            if copy_len == 0 {
                break;
            }
            segment[..copy_len].copy_from_slice(&packet[copied..copied + copy_len]);
            copied += copy_len;
        }
        copied
    }

    fn write(&self, buf: UserBuffer) -> usize {
        println!("UnixSocket write called with {} bytes", buf.len());
        let mut payload = vec![0u8; buf.len()];
        let mut payload_len = 0usize;
        for segment in buf.buffers.iter() {
            let end = payload_len + segment.len();
            payload[payload_len..end].copy_from_slice(segment);
            payload_len = end;
        }

        let peer = {
            let inner = self.inner.lock();
            inner.peer.as_ref().and_then(Weak::upgrade)
        };
        let Some(peer) = peer else {
            println!("UnixSocket write failed: no peer connected");
            return 0;
        };

        let attached_prog = {
            let mut peer_inner = peer.lock();
            let prog_fd = peer_inner.attached_prog;
            match peer_inner.socket_type {
                UnixSocketType::Datagram | UnixSocketType::Stream => {
                    peer_inner.recv_queue.push_back(payload);
                }
            }
            prog_fd
        };

        if let Some(prog_fd) = attached_prog {
            let _ = crate::syscall::bpf::run_socket_filter_program(prog_fd);
        }
        println!("UnixSocket wrote {} bytes to peer", payload_len);
        payload_len
    }

    fn ready_to_read(&self) -> bool {
        !self.inner.lock().recv_queue.is_empty()
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 0,
            mode: 0o140000 | 0o666,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 0,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2],
        }
    }

    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let mode = FileMode::from_bits_truncate(stat.mode as u16);
        PermStat { mode, uid: stat.uid, gid: stat.gid }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}
pub struct RawSocket {
    pub handle: SocketHandle,
    pub rx_wait_queue: Arc<Mutex<WaitQueue>>,
    pub local_rx_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
}


impl RawSocket {
    /// protocol 对应 IP 层协议号，例如 IPPROTO_ICMP = 1
    pub fn new(protocol: u8) -> Self {
        // 分配接收和发送缓冲区，需携带 Metadata 以保存报文边界
        let rx_buffer = RawPacketBuffer::new(
            vec![RawPacketMetadata::EMPTY; 32],
            vec![0; 8192]
        );
        let tx_buffer = RawPacketBuffer::new(
            vec![RawPacketMetadata::EMPTY; 32],
            vec![0; 8192]
        );
        
        // 创建 smoltcp 的 Raw Socket，绑定到 IPv4 和指定的协议号
        let socket = RawSocketSmol::new(
            IpVersion::Ipv4, 
            IpProtocol::from(protocol), 
            rx_buffer, 
            tx_buffer
        );
        
        // 加入全局 SocketSet 中进行调度
        let handle = SOCKET_SET.exclusive_access().add(socket);
        let rx_wait_queue = Arc::new(Mutex::new(WaitQueue::new()));
        crate::net::SOCKET_WAIT_QUEUES.lock().insert(handle, rx_wait_queue.clone());
       Self { 
            handle,
            rx_wait_queue, 
            // 初始化环回队列
            local_rx_buffer: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl File for RawSocket {
    fn readable(&self) -> bool {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<RawSocketSmol>(self.handle);
        socket.can_recv()
    }

    fn writable(&self) -> bool {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<RawSocketSmol>(self.handle);
        socket.can_send()
    }

    fn read(&self, mut buf: UserBuffer) -> usize {
        loop {
            // 优先检查有没有本地环回的包
            let mut local_queue = self.local_rx_buffer.lock();
            if let Some(packet) = local_queue.pop_front() {
                let len = packet.len();
                let mut current = 0;
                for buffer in buf.buffers.iter_mut() {
                    let copy_len = buffer.len().min(len.saturating_sub(current));
                    if copy_len == 0 { break; }
                    buffer[..copy_len].copy_from_slice(&packet[current..current + copy_len]);
                    current += copy_len;
                    if current == len { break; }
                }
                return current;
            }
            drop(local_queue);
            // 获取全局 sockets 锁去检查数据
            let mut sockets = SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<RawSocketSmol>(self.handle);
            
            if socket.can_recv() {
                if let Ok(recv_slice) = socket.recv() {
                    let len = recv_slice.len();
                    let mut current = 0;
                    // 分散读：将底层的 IP 报文拷贝到用户态的 IoVec 数组中
                    for buffer in buf.buffers.iter_mut() {
                        let copy_len = buffer.len().min(len.saturating_sub(current));
                        if copy_len == 0 { break; }
                        buffer[..copy_len].copy_from_slice(&recv_slice[current..current + copy_len]);
                        current += copy_len;
                        if current == len { break; }
                    }
                    return current; 
                }
            }
            drop(sockets); 
            let queue_guard = self.rx_wait_queue.lock();     
            current_task_to_sleep(queue_guard);
        }
    }

    fn write(&self, buf: UserBuffer) -> usize {
        let total_len = buf.len();
        let mut data = vec![0u8; total_len];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            data[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }

        // FIB 路由短路与 ICMP 回环交付
        if data.len() >= 20 { // 确保至少有完整的 IP 头
            // 提取目标 IP (IP 报文第 16-19 字节)
            let dst_ip_bytes = [data[16], data[17], data[18], data[19]];
            // 查表：判断目标 IP 是否属于网卡上的 IP 之一
            let is_local = {
                let iface = crate::net::NET_IFACE.exclusive_access();
                iface.ip_addrs().iter().any(|cidr| {
                    let smoltcp::wire::IpAddress::Ipv4(ipv4) = cidr.address() ;
                    ipv4.0 == dst_ip_bytes
                    
                })
            };
            if is_local {
                // 如果协议号是 1 (ICMP)
                if data[9] == 1 {
                    let ihl = (data[0] & 0x0F) as usize * 4;
                    // 确保包长包含完整的 ICMP 头，且类型为 8 (Echo Request)
                    if data.len() >= ihl + 8 && data[ihl] == 8 {
                        // 交换 Src IP 和 Dst IP
                        for i in 0..4 {
                            let temp = data[12 + i];
                            data[12 + i] = data[16 + i];
                            data[16 + i] = temp;
                        }
                        // 将 ICMP Type 修改为 0 (Echo Reply)
                        data[ihl] = 0;
                        // 重算 ICMP Checksum (设为 0，然后计算 Payload 的 16 位累加反码)
                        data[ihl + 2] = 0;
                        data[ihl + 3] = 0;
                        let mut sum = 0u32;
                        let mut i = ihl;
                        while i < data.len() {
                            let word = if i + 1 < data.len() {
                                ((data[i] as u32) << 8) | (data[i+1] as u32)
                            } else {
                                (data[i] as u32) << 8
                            };
                            sum = sum.wrapping_add(word);
                            i += 2;
                        }
                        while (sum >> 16) > 0 {
                            sum = (sum & 0xFFFF) + (sum >> 16);
                        }
                        let cksum = !sum as u16;
                        data[ihl + 2] = (cksum >> 8) as u8;
                        data[ihl + 3] = (cksum & 0xFF) as u8;
                    }
                }
                // 将回环的包塞入本地队列
                self.local_rx_buffer.lock().push_back(data);
                // 唤醒可能正在阻塞的进程 (Ping 进程)
                let queue_guard = self.rx_wait_queue.lock();
                if !queue_guard.is_empty() {
                    crate::process::wake_up_one(queue_guard); 
                }
                // 直接返回成功，不再向外网卡发送
                return total_len;
            }
        }
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<RawSocketSmol>(self.handle);
        
        if socket.can_send() {
            let total_len = buf.len();
            // 聚集写：将用户态多段内存拼凑成一个完整的报文
            let mut data = vec![0u8; total_len];
            let mut current = 0;
            for buffer in buf.buffers.iter() {
                let copy_len = buffer.len();
                data[current..current + copy_len].copy_from_slice(buffer);
                current += copy_len;
            }
            
            // 发射原始数据包
            if let Ok(_) = socket.send_slice(&data) {
                return total_len;
            }
        }
        0
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 0, mode: 0o140000 | 0o666, // 标记为 Socket
            nlink: 1, uid: 0, gid: 0, rdev: 0, __pad: 0,
            size: 0, blksize: 0, __pad2: 0, blocks: 0,
            atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }

    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let mode = FileMode::from_bits_truncate(stat.mode as u16);
        PermStat { mode, uid: stat.uid, gid: stat.gid }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }
    fn as_any(&self) -> &dyn Any { self }
}

