// os/src/net/socket.rs
use crate::auth::{FileMode, PermStat}; // 引入权限相关的类型
use crate::fs::OpenFlags;
use crate::fs::{File, Stat}; // 引入 File trait 和 Stat
use crate::mm::UserBuffer; // 引入 UserBuffer
use crate::net::vec;
use crate::net::SocketWaitQueue;
use crate::net::SOCKET_SET;
use crate::net::SOCKET_WAIT_QUEUES;
use crate::sync::MPSafeCell;
use crate::sync::WaitQueue;
use crate::syscall::errno::Errno::*;
use crate::AtomicBool;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use lazy_static::lazy_static;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::raw::{
    PacketBuffer as RawPacketBuffer, PacketMetadata as RawPacketMetadata, Socket as RawSocketSmol,
};
use smoltcp::socket::tcp::State;
use smoltcp::socket::tcp::{Socket as TcpSocketSmol, SocketBuffer};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint};
use smoltcp::wire::{IpProtocol, IpVersion};
use spin::Mutex;

use crate::process::signal::get_pending_signals;
use crate::process::check_pending_signal;
use crate::task::suspend_current_and_run_next;

pub struct TcpSocket {
    pub handle: SocketHandle,
    // 暂存 bind 分配或指定的本地端口
    pub local_port: Mutex<Option<u16>>,
    pub read_waiters: Arc<crate::sync::MPSafeCell<WaitQueue>>,
    pub is_listener: AtomicBool,
}

impl TcpSocket {
    pub fn new() -> Self {
        let rx_buffer = SocketBuffer::new(vec![0; 8192]);
        let tx_buffer = SocketBuffer::new(vec![0; 8192]);
        let socket = TcpSocketSmol::new(rx_buffer, tx_buffer);
        let waiters = Arc::new(crate::sync::MPSafeCell::new(WaitQueue::new()));
        // 注册 handle 与等待队列时保持与 net_poll 相同的锁顺序。
        // 否则 net_poll 可能在 add 和 insert 之间看到一个 Closed 且无队列的
        // socket，把它当作孤儿删除，随后首次 get(handle) 就会 panic。
        let mut sockets = SOCKET_SET.exclusive_access();
        let mut queues = SOCKET_WAIT_QUEUES.lock();
        let handle = sockets.add(socket);
        queues.insert(handle, SocketWaitQueue::new());
        drop(queues);
        drop(sockets);
        Self {
            handle,
            local_port: Mutex::new(None),
            read_waiters: waiters,
            is_listener: AtomicBool::new(false),
        }
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

        // net_poll 的顺序是 interface -> SOCKET_SET。这里必须保持相同顺序；
        // 旧实现先锁 SOCKET_SET 再锁 LO_IFACE，会与 accept 中的 net_poll
        // 形成 ABBA 死锁，表现为客户端永久停在 syscall 203、服务端停在 accept。
        let res = if is_loopback {
            let mut lo_iface = crate::net::LO_IFACE.exclusive_access();
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
            socket.connect(lo_iface.context(), remote_ep, local_port)
        } else {
            let mut eth_iface = crate::net::NET_IFACE.exclusive_access();
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
            socket.connect(eth_iface.context(), remote_ep, local_port)
        };
        let connect_status = match res {
            Ok(_) => 0,
            Err(_) => crate::syscall::errno::Errno::ECONNREFUSED.as_isize(),
        };
        // 这里只启动握手。阻塞等待、网络轮询和信号中断由 sys_connect
        // 统一处理，避免在 socket 层形成无法被 timeout/kill 打断的循环。
        connect_status
    }
}
impl Drop for TcpSocket {
    fn drop(&mut self) {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
        let exists = sockets.iter().any(|(h, _)| h == self.handle);
        if exists {
            sockets.remove(self.handle);
        }
        queues.remove(&self.handle);
        drop(queues);
        drop(sockets);
        crate::net::net_poll();
    }
}
impl File for TcpSocket {
    fn info_type(&self) {
        println!("tcp socket");
    }
    fn is_socket(&self) -> bool {
        true
    }
    fn readable(&self) -> bool {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        let state = socket.state();
        if state == smoltcp::socket::tcp::State::Listen {
            return false;
        }
        if self.is_listener.load(core::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        let is_eof = !socket.may_recv()
            || matches!(
                state,
                smoltcp::socket::tcp::State::CloseWait
                    | smoltcp::socket::tcp::State::Closed
                    | smoltcp::socket::tcp::State::TimeWait
                    | smoltcp::socket::tcp::State::LastAck
                    | smoltcp::socket::tcp::State::Closing
            );
        socket.can_recv() || is_eof
    }
    fn writable(&self) -> bool {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
        socket.can_send() || !socket.may_send()
    }
    fn read(&self, mut buf: UserBuffer) -> usize {
        if buf.len() == 0 {
            return 0;
        }
        loop {
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
            let state = socket.state();

            if socket.can_recv() {
                let mut temp_buf = vec![0u8; buf.len()];
                match socket.recv_slice(&mut temp_buf) {
                    Ok(recv_len) if recv_len > 0 => {
                        let mut current = 0;
                        for buffer in buf.buffers.iter_mut() {
                            let copy_len = buffer.len().min(recv_len.saturating_sub(current));
                            if copy_len == 0 {
                                break;
                            }
                            buffer[..copy_len]
                                .copy_from_slice(&temp_buf[current..current + copy_len]);
                            current += copy_len;
                            if current == recv_len {
                                break;
                            }
                        }
                        drop(sockets);
                        crate::net::net_poll();
                        return current;
                    }
                    Ok(_) => {
                        drop(sockets);
                        return 0;
                    }
                    Err(smoltcp::socket::tcp::RecvError::Finished) => {
                        drop(sockets);
                        return 0;
                    }
                    Err(_e) => {
                        drop(sockets);
                        return 0;
                    }
                }
            } else if !socket.may_recv()
                || matches!(
                    state,
                    State::CloseWait
                        | State::Closed
                        | State::TimeWait
                        | State::LastAck
                        | State::Closing
                )
            {
                drop(sockets);
                return 0;
            }
            drop(sockets);
            crate::net::net_poll();
            crate::timer::check_timer_cooperative();
            let interrupting_signals = crate::task::SignalFlags::SIGALRM
                | crate::task::SignalFlags::SIGTERM
                | crate::task::SignalFlags::SIGINT
                | crate::task::SignalFlags::SIGKILL;
            if get_pending_signals().intersects(interrupting_signals) {
                return EINTR.as_isize() as usize;
            }
            crate::task::suspend_current_and_run_next();
        }
    }

    fn write(&self, buf: UserBuffer) -> usize {
        if buf.len() == 0 {
            return 0;
        }
        let mut temp_buf = vec![0u8; buf.len()];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            temp_buf[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }
        loop {
            crate::net::net_poll();
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);
            if !socket.may_send() {
                drop(sockets);
                return 0;
            }
            if socket.can_send() {
                let write_len = socket.send_slice(&temp_buf).unwrap_or(0);
                if write_len > 0 {
                    drop(sockets);
                    crate::net::net_poll();
                    return write_len;
                }
            }
            drop(sockets);
            crate::net::net_poll();
            crate::timer::check_timer_cooperative();
            let interrupting_signals = crate::task::SignalFlags::SIGALRM
                | crate::task::SignalFlags::SIGTERM
                | crate::task::SignalFlags::SIGINT
                | crate::task::SignalFlags::SIGKILL;
            if get_pending_signals().intersects(interrupting_signals) {
                return EINTR.as_isize() as usize;
            }

            // 让出 CPU，等待下一轮调度回来继续尝试发送
            crate::task::suspend_current_and_run_next();
        }
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
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
    fn getdents(&self, _buf: &mut [u8]) -> isize {
        -1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

lazy_static! {
    static ref LOCAL_UDP_SOCKETS: Mutex<BTreeMap<u16, Arc<Mutex<VecDeque<(IpEndpoint, Vec<u8>)>>>>> =
        Mutex::new(BTreeMap::new());
}
pub struct UdpSocket {
    pub handle: smoltcp::iface::SocketHandle,
    //远端地址队列
    pub remote_ep: Mutex<Option<IpEndpoint>>,
    pub local_port: Mutex<Option<u16>>,
    pub read_waiters: Arc<crate::sync::MPSafeCell<crate::sync::WaitQueue>>,
    pub write_waiters: Arc<crate::sync::MPSafeCell<crate::sync::WaitQueue>>,
    pub flags: Mutex<OpenFlags>,
    pub recv_timeout: spin::Mutex<Option<core::time::Duration>>,
}
impl Drop for UdpSocket {
    fn drop(&mut self) {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
        sockets.remove(self.handle);
        queues.remove(&self.handle);
        drop(queues);
        drop(sockets);
        crate::net::net_poll();
    }
}
impl UdpSocket {
    pub fn new() -> Self {
        // 分配 16 个包的元数据空间，和 16KB 的数据缓存空间
        let rx_buffer =
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16384]);
        let tx_buffer =
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16384]);
        let socket = udp::Socket::new(rx_buffer, tx_buffer);
        let wait_queues = crate::net::SocketWaitQueue::new();
        let rx_waiters = wait_queues.rx_queue.clone();
        let tx_waiters = wait_queues.tx_queue.clone();
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
        let handle = sockets.add(socket);
        queues.insert(handle, wait_queues);
        drop(queues);
        drop(sockets);
        Self {
            handle,
            remote_ep: Mutex::new(None),
            local_port: Mutex::new(None),
            read_waiters: rx_waiters,
            write_waiters: tx_waiters,
            flags: spin::Mutex::new(crate::fs::OpenFlags::empty()),
            recv_timeout: spin::Mutex::new(None),
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
            Ok(_) => {
                *self.local_port.lock() = Some(actual_port);
                0
            }
            Err(_) => crate::syscall::errno::Errno::EADDRINUSE.as_isize(),
        }
    }
    pub fn connect(&self, remote_ep: IpEndpoint) -> isize {
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
            Err(udp::SendError::BufferFull) => {
                crate::syscall::errno::Errno::EAGAIN.as_isize()
            }
            Err(udp::SendError::Unaddressable) => {
                crate::syscall::errno::Errno::EDESTADDRREQ.as_isize()
            }
        }
    }
    /// 处理 UdpMetadata，提取真实 Endpoint
    pub fn recvfrom(&self, buf: &mut [u8]) -> Option<(usize, IpEndpoint)> {
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
        if !socket.can_recv() {
            return None; // 暂无数据
        }
        match socket.recv() {
            Ok((data, meta)) => {
                let copy_len = usize::min(buf.len(), data.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                Some((copy_len, meta.endpoint))
            }
            Err(e) => {
                let fallback_endpoint = smoltcp::wire::IpEndpoint {
                    addr: smoltcp::wire::IpAddress::v4(0, 0, 0, 0),
                    port: 0,
                };
                return Some((0, fallback_endpoint));
            }
        }
    }
}
// 实现 File trait，使其能放进系统的 fd_table 中
impl File for UdpSocket {
    fn is_socket(&self) -> bool { true }
    fn get_flags(&self) -> OpenFlags {
        *self.flags.lock()
    }

    fn set_flags(&self, flags: OpenFlags) -> bool {
        *self.flags.lock() = flags;
        true
    }
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
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
            if socket.can_recv() {
                // 注意这里分配的临时缓冲区大小要足够容纳一个 UDP 报文
                let mut temp_buf = alloc::vec![0u8; 16384];
                match socket.recv_slice(&mut temp_buf) {
                    Ok((recv_len, _meta)) => {
                        let mut current = 0;
                        for buffer in buf.buffers.iter_mut() {
                            let copy_len = buffer.len().min(recv_len.saturating_sub(current));
                            if copy_len == 0 {
                                break;
                            }
                            buffer[..copy_len]
                                .copy_from_slice(&temp_buf[current..current + copy_len]);
                            current += copy_len;
                            if current == recv_len {
                                break;
                            }
                        }
                        drop(sockets);
                        crate::net::net_poll();
                        return current;
                    }
                    Err(_) => {} // 如果出现异常，跳过，去下面尝试挂起
                }
            }
            // 没读到数据，准备阻塞
            drop(sockets);
            crate::net::net_poll();

            let queues = crate::net::SOCKET_WAIT_QUEUES.lock();
            if let Some(socket_wait) = queues.get(&self.handle) {
                let rx_queue = socket_wait.rx_queue.clone();
                drop(queues);
                crate::task::block_current_and_run_next(rx_queue.get_mutex());
                let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
                if let Some(socket_wait) = queues.get(&self.handle) {
                    let mut rx_guard = socket_wait.rx_queue.exclusive_access();
                    rx_guard.remove_by_tid(crate::task::current_task().unwrap().gettid());
                }
                drop(queues);
            } else {
                drop(queues);
                crate::task::suspend_current_and_run_next();
            }
        }
    }
    fn write(&self, buf: UserBuffer) -> usize {
        // 先把用户缓冲区的数据拷贝出来，避免在 loop 里面反复拷贝
        let mut temp_buf = alloc::vec![0u8; buf.len()];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            temp_buf[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }

        let remote = *self.remote_ep.lock();
        if remote.is_none() {
            return 0; // 或者返回 ENOTCONN 错误码
        }
        let remote_ep = remote.unwrap();
        loop {
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(self.handle);
            if socket.can_send() {
                match socket.send_slice(&temp_buf, remote_ep) {
                    Ok(_) => {
                        let len = temp_buf.len();
                        drop(sockets);
                        crate::net::net_poll();
                        return len;
                    }
                    Err(e) => {
                        warn!("[Debug Write] Send failed! Error: {:?}", e);
                    }
                }
            }
            drop(sockets);
            crate::net::net_poll();
            let queues = crate::net::SOCKET_WAIT_QUEUES.lock();
            if let Some(socket_wait) = queues.get(&self.handle) {
                let tx_queue = socket_wait.tx_queue.clone();
                drop(queues);
                crate::task::block_current_and_run_next(tx_queue.get_mutex());
            } else {
                drop(queues);
                crate::task::suspend_current_and_run_next();
            }
        }
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 0,
            mode: 0o140000 | 0o666, // 标记为 Socket 类型
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
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        -1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn info_type(&self) {
        info!("UdpSocket: handle = {}", self.handle);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketType {
    Stream,
    Datagram,
}

const UNIX_SOCKET_RECV_LIMIT: usize = 64 * 1024;

struct UnixSocketInner {
    recv_queue: VecDeque<Vec<u8>>,
    recv_bytes: usize,
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
            recv_bytes: 0,
            peer: None,
            attached_prog: None,
            socket_type,
        }));
        let right = Arc::new(Mutex::new(UnixSocketInner {
            recv_queue: VecDeque::new(),
            recv_bytes: 0,
            peer: None,
            attached_prog: None,
            socket_type,
        }));
        left.lock().peer = Some(Arc::downgrade(&right));
        right.lock().peer = Some(Arc::downgrade(&left));
        (Self { inner: left }, Self { inner: right })
    }

    pub fn attach_bpf(&self, prog_fd: usize) {
        self.inner.lock().attached_prog = Some(prog_fd);
    }
}

impl File for UnixSocket {
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    fn read(&self, mut buf: UserBuffer) -> usize {
        if buf.len() == 0 {
            return 0;
        }
        //修改语义为轮询式读取：如果当前没有数据可读，就让出 CPU 给其他进程，等被唤醒后再来尝试读取。
        let packet = loop {
            let mut inner = self.inner.lock();
            if let Some(packet) = inner.recv_queue.pop_front() {
                inner.recv_bytes = inner.recv_bytes.saturating_sub(packet.len());
                break packet;
            }
            let peer_closed = inner.peer.as_ref().and_then(Weak::upgrade).is_none();
            drop(inner);

            if peer_closed {
                return 0;
            }
            if check_pending_signal() {
                return 0;
            }
            suspend_current_and_run_next();
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
        if copied < packet.len() {
            let mut inner = self.inner.lock();
            if inner.socket_type == UnixSocketType::Stream {
                let remaining = packet[copied..].to_vec();
                inner.recv_bytes += remaining.len();
                inner.recv_queue.push_front(remaining);
            }
        }
        copied
    }

    fn write(&self, buf: UserBuffer) -> usize {
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
            return 0;
        };

        let write_len = loop {
            let peer_inner = peer.lock();
            let available = UNIX_SOCKET_RECV_LIMIT.saturating_sub(peer_inner.recv_bytes);
            if available > 0 {
                break payload_len.min(available);
            }
            drop(peer_inner);

            if check_pending_signal() {
                return 0;
            }
            suspend_current_and_run_next();
        };

        let mut payload = vec![0u8; write_len];
        let mut payload_len = 0usize;
        for segment in buf.buffers.iter() {
            let copy_len = segment.len().min(write_len - payload_len);
            let end = payload_len + copy_len;
            payload[payload_len..end].copy_from_slice(&segment[..copy_len]);
            payload_len = end;
            if payload_len == write_len {
                break;
            }
        }

        if payload_len == 0 {
            return 0;
        }

        let attached_prog = {
            let mut peer_inner = peer.lock();
            let prog_fd = peer_inner.attached_prog;
            match peer_inner.socket_type {
                UnixSocketType::Datagram | UnixSocketType::Stream => {
                    peer_inner.recv_bytes += payload_len;
                    peer_inner.recv_queue.push_back(payload);
                }
            }
            prog_fd
        };

        if let Some(prog_fd) = attached_prog {
            let _ = crate::syscall::bpf::run_socket_filter_program(prog_fd);
        }
        payload_len
    }

    fn ready_to_read(&self) -> bool {
        let inner = self.inner.lock();
        !inner.recv_queue.is_empty() || inner.peer.as_ref().and_then(Weak::upgrade).is_none()
    }

    fn ready_to_write(&self) -> bool {
        let peer = {
            let inner = self.inner.lock();
            inner.peer.as_ref().and_then(Weak::upgrade)
        };
        let Some(peer) = peer else {
            return true;
        };
        let peer_inner = peer.lock();
        peer_inner.recv_bytes < UNIX_SOCKET_RECV_LIMIT
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
        PermStat {
            mode,
            uid: stat.uid,
            gid: stat.gid,
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        -1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn info_type(&self) {
        info!("info_type: UnixSocket");
    }
}
pub struct RawSocket {
    pub handle: SocketHandle,
    pub rx_wait_queue: Arc<Mutex<WaitQueue>>,
    pub local_rx_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
    pub read_waiters: Arc<crate::sync::MPSafeCell<crate::sync::WaitQueue>>,
    pub write_waiters: Arc<crate::sync::MPSafeCell<crate::sync::WaitQueue>>,
}

impl RawSocket {
    /// protocol 对应 IP 层协议号，例如 IPPROTO_ICMP = 1
    pub fn new(protocol: u8) -> Self {
        // 分配接收和发送缓冲区，需携带 Metadata 以保存报文边界
        let rx_buffer = RawPacketBuffer::new(vec![RawPacketMetadata::EMPTY; 32], vec![0; 8192]);
        let tx_buffer = RawPacketBuffer::new(vec![RawPacketMetadata::EMPTY; 32], vec![0; 8192]);

        // 创建 smoltcp 的 Raw Socket，绑定到 IPv4 和指定的协议号
        let socket = RawSocketSmol::new(
            IpVersion::Ipv4,
            IpProtocol::from(protocol),
            rx_buffer,
            tx_buffer,
        );

        let rx_wait_queue = Arc::new(Mutex::new(WaitQueue::new()));
        let wait_queues = crate::net::SocketWaitQueue::new();
        let rx_waiters = wait_queues.rx_queue.clone();
        let tx_waiters = wait_queues.tx_queue.clone();
        let mut sockets = SOCKET_SET.exclusive_access();
        let mut queues = SOCKET_WAIT_QUEUES.lock();
        let handle = sockets.add(socket);
        queues.insert(handle, wait_queues);
        drop(queues);
        drop(sockets);
        Self {
            handle,
            rx_wait_queue,
            // 初始化环回队列
            local_rx_buffer: Arc::new(Mutex::new(VecDeque::new())),
            read_waiters: rx_waiters,
            write_waiters: tx_waiters,
        }
    }
}

impl File for RawSocket {
    fn info_type(&self) {
        println!("raw socket");
    }
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
                    if copy_len == 0 {
                        break;
                    }
                    buffer[..copy_len].copy_from_slice(&packet[current..current + copy_len]);
                    current += copy_len;
                    if current == len {
                        break;
                    }
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
                        if copy_len == 0 {
                            break;
                        }
                        buffer[..copy_len]
                            .copy_from_slice(&recv_slice[current..current + copy_len]);
                        current += copy_len;
                        if current == len {
                            break;
                        }
                    }
                    return current;
                }
            }
            drop(sockets);
            crate::net::net_poll();
            crate::task::block_current_and_run_next(self.read_waiters.get_mutex());
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
        if data.len() >= 20 {
            // 确保至少有完整的 IP 头
            // 提取目标 IP (IP 报文第 16-19 字节)
            let dst_ip_bytes = [data[16], data[17], data[18], data[19]];
            // 查表：判断目标 IP 是否属于网卡上的 IP 之一
            let is_local = {
                let iface = crate::net::NET_IFACE.exclusive_access();
                iface.ip_addrs().iter().any(|cidr| {
                    let smoltcp::wire::IpAddress::Ipv4(ipv4) = cidr.address();
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
                                ((data[i] as u32) << 8) | (data[i + 1] as u32)
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
                let has_waiting_task = {
                    let queue_guard = self.rx_wait_queue.lock();
                    !queue_guard.is_empty()
                };
                if has_waiting_task {
                    crate::process::wake_up_one(&self.rx_wait_queue);
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
            dev: 0,
            ino: 0,
            mode: 0o140000 | 0o666, // 标记为 Socket
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
        PermStat {
            mode,
            uid: stat.uid,
            gid: stat.gid,
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        -1
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
