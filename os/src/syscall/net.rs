use crate::task::current_task;
use crate::net::socket::{TcpSocket, UnixSocket, UnixSocketType};
use crate::process::*;
use crate::syscall::Errno::*;
use crate::syscall::Arc;
use crate::mm::{try_translated_read, try_translated_write, translated_byte_buffer, UserBuffer};
use crate::syscall::errno::Errno;
use alloc::vec;
use crate::net::MsgHdr;
use crate::net::IoVec;
use crate::net::netlink::StandardNetlinkSocket;
use crate::net::socket::UdpSocket;
use crate::process::FileDescriptor;
use crate::process::FdFlags;
use crate::net::net_poll;
use core::sync::atomic::{AtomicU16, Ordering};
use crate::fs::OpenFlags;
use crate::timer::TimeVal;
use crate::get_time_ms;
use smoltcp::socket::tcp::State;
/// 获取指定 Socket 的本地地址和端口信息。
/// 将内核中 Socket 的 local_endpoint 信息格式化为 sockaddr_in 结构并拷贝回用户空间。 asd
pub fn sys_getsockname(fd: usize, addr: *mut u8, addrlen: *mut u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize(); // EBADF
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); 
    let mut user_len = unsafe {
        if let Some(ul) = try_translated_read(token, addrlen) {
            ul
        } else {
            return EFAULT.as_isize();
        }
    };
    let mut is_ip_socket = false;
    let family: u16 = 2; // AF_INET
    let mut port: u16 = 0;
    let mut ip: [u8; 4] = [0, 0, 0, 0];

    if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
        // 1. 处理 TCP Socket
       let port_lock = socket.local_port.lock();
        if let Some(p) = *port_lock {
            port = p;
        } 
        else if let Some(ep) = socket.local_endpoint() {
            port = ep.port; 
             let smoltcp::wire::IpAddress::Ipv4(v4) = ep.addr;
                ip = v4.0;
        }
        is_ip_socket = true;
    } else if let Some(socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        // 2. 处理 UDP Socket
        let port_lock = socket.local_port.lock();
        if let Some(p) = *port_lock {
            port = p;
        } else {
            let sockets = crate::net::SOCKET_SET.exclusive_access();
            let udp_sock = sockets.get::<smoltcp::socket::udp::Socket>(socket.handle);
            let ep = udp_sock.endpoint(); 
            port = ep.port;
            if let Some(smoltcp::wire::IpAddress::Ipv4(v4)) = ep.addr {
                ip = v4.0;
            }
        }
        drop(port_lock);
        is_ip_socket = true;
    }

    // 3. 如果是 TCP 或 UDP，统一组装 sockaddr_in 并写入用户态
    if is_ip_socket {
        let mut sockaddr_bytes = [0u8; 16];
        sockaddr_bytes[0..2].copy_from_slice(&family.to_ne_bytes());
        sockaddr_bytes[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr_bytes[4..8].copy_from_slice(&ip);

        unsafe {
            if let Some(user_len) = try_translated_read(token, addrlen) {
                let copy_len = (user_len as usize).min(16);
                let mut current_addr = addr as usize;
                for i in 0..copy_len {
                    if !try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]) {
                        return EFAULT.as_isize();
                    }
                    current_addr += 1;
                }   
                if !try_translated_write(token, addrlen, 16u32) {
                    return EFAULT.as_isize();
                }
            } else {
                return EFAULT.as_isize();
            }
        }
        return 0; // 成功
    }
    else if let Some(_nl_socket) = file.as_any().downcast_ref::<StandardNetlinkSocket>() {
        // Netlink 的地址结构是 sockaddr_nl，标准长度为 12 字节
        let family: u16 = 16; // AF_NETLINK = 16
        let mut sockaddr_bytes = [0u8; 12];
        sockaddr_bytes[0..2].copy_from_slice(&family.to_ne_bytes());
        sockaddr_bytes[4..8].copy_from_slice(&0u32.to_ne_bytes()); 
        let copy_len = (user_len as usize).min(12);
        let mut current_addr = addr as usize;
        for i in 0..copy_len {
            unsafe {
                if !try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]) {
                    return crate::syscall::errno::Errno::EFAULT.as_isize();
                }
            }
            current_addr += 1;
        }
        unsafe {
            if !try_translated_write(token, addrlen, 12u32) {
                return crate::syscall::errno::Errno::EFAULT.as_isize();
            }
        }
        return 0;
    }
    // 匹配 AF_UNIX 套接字 
    else if let Some(_unix_socket) = file.as_any().downcast_ref::<crate::net::socket::UnixSocket>() {
        let family: u16 = 1; 
        let mut sockaddr_bytes = [0u8; 110]; 
        sockaddr_bytes[0..2].copy_from_slice(&family.to_ne_bytes());
        let copy_len = (user_len as usize).min(110);
        let mut current_addr = addr as usize;
        for i in 0..copy_len {
            unsafe {
                if !try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]) {
                    return crate::syscall::errno::Errno::EFAULT.as_isize();
                }
            }
            current_addr += 1;
        }

        unsafe {
            if !try_translated_write(token, addrlen, copy_len as u32) {
                return crate::syscall::errno::Errno::EFAULT.as_isize();
            }
        }
        return 0;
    } 
    else {
        return ENOTSOCK.as_isize(); // ENOTSOCK (不是一个 Socket)
    }
}
/// 设置 socket 选项。
///
/// 参数含义：
/// - `fd`：目标 socket 的文件描述符。
/// - `level`：选项层级。当前识别 `SOL_SOCKET`。
/// - `optname`：具体选项名。当前支持 `SO_ATTACH_BPF`。
/// - `optval`：指向用户空间选项值缓冲区的指针。
/// - `optlen`：`optval` 缓冲区长度，单位为字节。
/// 
/// - 若 `fd` 无效，返回 `EBADF`。
/// - 若 `level == SOL_SOCKET && optname == SO_ATTACH_BPF`，则从 `optval` 中读取一个
///   `prog_fd`，并将其附着到 `UnixSocket`；要求 `optlen >= sizeof(i32)`。
/// - 对 UDP/TCP socket 的其他选项，当前为了兼容用户态探测逻辑，直接返回成功 `0`。
/// - 对非 socket 文件对象，返回 `ENOTSOCK`。
///
pub fn sys_setsockopt(
    fd: usize,
    level: usize,
    optname: usize,
    optval: *const u8,
    optlen: u32,
) -> isize {
    const SOL_SOCKET: usize = 1;
    const SO_ATTACH_BPF: usize = 50;
    const SO_RCVTIMEO: usize = 20; // 接收超时常量
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    // 1. 检查 fd 是否合法
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }

    // 2. 获取文件对象
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); // 提前释放进程锁
    if level == SOL_SOCKET && optname == SO_RCVTIMEO {
        if optlen < core::mem::size_of::<TimeVal>() as u32 || optval.is_null() {
            return Errno::EINVAL.as_isize();
        }
        // 从用户态读取 timeval 结构体
        let timeval = if let Some(tv) = try_translated_read(token, optval as *const TimeVal) {
            tv
        } else {
            return Errno::EFAULT.as_isize();
        };

        let timeout = if timeval.sec == 0 && timeval.usec == 0 {
            None
        } else {
            Some(core::time::Duration::from_secs(timeval.sec as u64) 
                 + core::time::Duration::from_micros(timeval.usec as u64))
        };

        if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
            *udp_socket.recv_timeout.lock() = timeout;
            return 0;
        }
        if let Some(tcp_socket) = file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
            // *tcp_socket.recv_timeout.lock() = timeout; 
            return 0;
        }
    }
    if level == SOL_SOCKET && optname == SO_ATTACH_BPF {
        if optlen < core::mem::size_of::<i32>() as u32 || optval.is_null() {
            return Errno::EINVAL.as_isize();
        }
        let prog_fd = if let Some(fd) = try_translated_read(token, optval as *const i32) {
            fd
        } else {
            return Errno::EFAULT.as_isize();
        };
        if prog_fd < 0 || !crate::syscall::bpf::is_socket_filter_prog_fd(prog_fd as usize) {
            return Errno::EBADF.as_isize();
        }
        if let Some(socket) = file.as_any().downcast_ref::<UnixSocket>() {
            socket.attach_bpf(prog_fd as usize);
            return 0;
        }
    }
    if let Some(_udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        return 0;
    }
    // 3. 检查这个文件到底是不是 Socket？
    if let Some(_socket) = file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
        info!(
            "[setsockopt] fd: {}, level: {}, optname: {} (e.g. 20=SO_RCVTIMEO) -> Fake Success",
            fd, level, optname
        );
        return 0; // 成功！
    } else {
        return crate::syscall::errno::Errno::ENOTSOCK.as_isize();
    }
}

/// 发起网络连接。
/// 从用户态读取目标 sockaddr_in（IP和端口），转换成大端序网络地址，并调用底层 TcpSocket 尝试建立连接。
pub fn sys_connect(fd: usize, addr: *const u8, addrlen: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();

    drop(inner); 
    if addrlen < 16 {
        return Errno::EINVAL.as_isize(); 
    }
    if addr.is_null() {
        return Errno::EFAULT.as_isize();
    }
    let family_bytes = {
        if let Some(b) = try_translated_read(token, addr as *const [u8; 2]) {
            b
        } else {
            return EFAULT.as_isize();
        }
    };
    let sa_family = u16::from_ne_bytes(family_bytes);
    const AF_UNSPEC: u16 = 0;
    const AF_INET: u16 = 2;
    if sa_family == AF_UNSPEC {
        if let Some(tcp_socket) = file.as_any().downcast_ref::<TcpSocket>() {
            tcp_socket.disconnect(); 
        }
        if let Some(upd_socket) = file.as_any().downcast_ref::<UdpSocket>() {
            upd_socket.disconnect(); 
        }
        return 0;
    }
    if sa_family != AF_INET {
        return Errno::EAFNOSUPPORT.as_isize(); 
    }
    let sockaddr = {
        if let Some(s) = try_translated_read(token, addr as *const [u8; 16]) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    let port = u16::from_be_bytes([sockaddr[2], sockaddr[3]]);
    let ip = [sockaddr[4], sockaddr[5], sockaddr[6], sockaddr[7]];
    let endpoint = smoltcp::wire::IpEndpoint::new(
        smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address(ip)),
        port
    );
    if let Some(tcp_socket) = file.as_any().downcast_ref::<TcpSocket>() {
        tcp_socket.connect(endpoint)
    } 
    else if let Some(udp_socket) = file.as_any().downcast_ref::<UdpSocket>() {
        udp_socket.connect(endpoint); 
        0 
    } else {
        Errno::ENOTSOCK.as_isize()
    }
   
}

/// 发送数据到指定地址。
/// 对于 TCP 连接，目标地址通常被忽略。该函数将用户缓冲区数据通过文件系统的 write 接口发送至网络。
pub fn sys_sendto(
    fd: usize, 
    buf: *const u8, 
    len: usize, 
    _flags: i32, 
    dest_addr: *const u8, 
    _addrlen: u32
) -> isize {
    
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);
    if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        // 1. 从用户空间拷贝出发送数据
        let mut data = vec![0u8; len];
        let user_buf = UserBuffer::new(translated_byte_buffer(token, buf, len));
        let mut current = 0;
        for buffer in user_buf.buffers.iter() {
            let copy_len = buffer.len();
            data[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }
        if dest_addr as usize != 0 {
            let sockaddr_bytes = {
                if let Some(s) = try_translated_read(token, dest_addr as *const [u8; 16]) {
                    s
                } else {
                    return EFAULT.as_isize();
                }
            };
            let port = u16::from_be_bytes([sockaddr_bytes[2], sockaddr_bytes[3]]);
            let ip = smoltcp::wire::IpAddress::v4(
                sockaddr_bytes[4], sockaddr_bytes[5], sockaddr_bytes[6], sockaddr_bytes[7]
            );
            let remote_ep = smoltcp::wire::IpEndpoint::new(ip, port);
           loop {
            let ret = udp_socket.sendto(&data, remote_ep);
            // 如果底层的 smoltcp 缓冲区满了，
            if ret == EAGAIN.as_isize() {
                // 驱动网卡收发，把缓冲区里的包发出去
                net_poll(); 
                // 挂起当前任务，切换到其他任务
                suspend_current_and_run_next(); 
                continue;
            }
            return ret;
        }
        } else {
            return Errno::EDESTADDRREQ.as_isize(); // 需要目标地址
        }
    }
    loop {
         let user_buf = UserBuffer::new(translated_byte_buffer(token, buf, len));
        let ret = file.write(user_buf) as isize;
        
        if ret == -11 {
            net_poll();
            suspend_current_and_run_next();
            continue;
        }
        net_poll(); 
        return ret;
    }
}

/// 接收网络数据并获取来源地址。
/// 从 Socket 读取数据流，若用户提供了 src_addr 缓冲区，则额外将远程端点（remote_endpoint）的 IP 和端口信息写回用户空间。
pub fn sys_recvfrom(
    fd: usize, 
    buf: *mut u8, 
    len: usize, 
    _flags: i32, 
    src_addr: *mut u8, 
    addrlen: *mut u32
) -> isize {
    const MSG_DONTWAIT: i32 = 0x40;
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);
    let is_nonblocking = (_flags & MSG_DONTWAIT != 0) 
        || file.get_flags().contains(crate::fs::OpenFlags::NONBLOCK);
    let file_flags = file.get_flags();
    if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        let mut data = vec![0u8; len];
        loop {
            if let Some((read_len, src_ep)) = udp_socket.recvfrom(&mut data) {
                // 把数据拷贝回用户的 buf
                if read_len > 0 {
                    let mut user_buf = UserBuffer::new(crate::mm::translated_byte_buffer_mut(token, buf, len));
                    let mut current = 0;
                    for buffer in user_buf.buffers.iter_mut() {
                        let copy_len = buffer.len().min(read_len - current);
                        buffer[..copy_len].copy_from_slice(&data[current..current + copy_len]);
                        current += copy_len;
                        if current == read_len { break; }
                    }
                }

                // 把发送方地址填回 src_addr
                if src_addr as usize != 0 && addrlen as usize != 0 {
                    let mut sockaddr_bytes = [0u8; 16];
                    sockaddr_bytes[0..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
                    sockaddr_bytes[2..4].copy_from_slice(&src_ep.port.to_be_bytes()); // 大端序端口
                    let smoltcp::wire::IpAddress::Ipv4(v4) = src_ep.addr;
                    sockaddr_bytes[4..8].copy_from_slice(&v4.0);

                    unsafe {
                        let mut user_len = {
                            if let Some(ul) = crate::mm::try_translated_read(token, addrlen) { ul } 
                            else { return Errno::EFAULT.as_isize(); }
                        };
                        let copy_len = (user_len as usize).min(16);
                        let mut current_addr = src_addr as usize;
                        for i in 0..copy_len {
                            if !crate::mm::try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]) {
                                return Errno::EFAULT.as_isize();
                            }
                            current_addr += 1;
                        }
                        user_len = 16;
                        if !crate::mm::try_translated_write(token, addrlen, user_len) {
                            return Errno::EFAULT.as_isize();
                        }
                    }
                }
                return read_len as isize;
            } else {
                if is_nonblocking {
                    return Errno::EAGAIN.as_isize(); 
                }
                net_poll(); 
                let mut sockets = crate::net::SOCKET_SET.exclusive_access();
                let smol_socket = sockets.get_mut::<smoltcp::socket::udp::Socket>(udp_socket.handle);
                let can_recv = smol_socket.can_recv();
                drop(sockets);
                if can_recv {
                continue; 
                }
                let queues = crate::net::SOCKET_WAIT_QUEUES.lock();
                if let Some(socket_wait) = queues.get(&udp_socket.handle) {
                    let rx_queue = socket_wait.rx_queue.clone();
                    drop(queues); 
                    crate::task::block_current_and_run_next(&rx_queue);
                    let task = crate::task::current_task().unwrap();
                    let task_inner = task.inner_exclusive_access();
                    if task_inner.signals.contains(crate::task::SignalFlags::SIGALRM) {
                        drop(task_inner); 
                        return crate::syscall::errno::Errno::EINTR.as_isize(); 
                    }
                    drop(task_inner);
                    continue;
                } else {
                    drop(queues);
                    crate::task::suspend_current_and_run_next();
                }
                continue;
            }
        }
    }
    // 1读取网络数据
    let user_buf = UserBuffer::new(crate::mm::translated_byte_buffer_mut(token, buf, len));
    let read_len = file.read(user_buf);
    //用户提供了 src_addr 和 addrlen填入对端的 IP 和端口信息
    if src_addr as usize != 0 && addrlen as usize != 0 {
        if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
            if let Some(ep) = socket.remote_endpoint() {
                let mut sockaddr_bytes = [0u8; 16];
                sockaddr_bytes[0..2].copy_from_slice(&2u16.to_ne_bytes()); // 协议族 AF_INET
                sockaddr_bytes[2..4].copy_from_slice(&ep.port.to_be_bytes()); // 大端序端口
               let smoltcp::wire::IpAddress::Ipv4(v4) = ep.addr;
                    sockaddr_bytes[4..8].copy_from_slice(&v4.0); // IPv4
                unsafe {
                    let mut user_len = {
                        if let Some(ul) = try_translated_read(token, addrlen) {
                            ul
                        } else {
                            return EFAULT.as_isize();
                        }
                    };
                    let copy_len = (user_len as usize).min(16);
                    let mut current_addr = src_addr as usize;
                    
                    for i in 0..copy_len {
                        if !try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]) {
                            return EFAULT.as_isize();
                        }
                        current_addr += 1;
                    }
                    user_len = 16;
                    if !try_translated_write(token, addrlen, user_len) {
                        return EFAULT.as_isize();
                    }
                }
            }
        }
    }

    read_len as isize
}

pub fn sys_socket(domain: usize, socket_type: usize, protocol: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process(); 
    let mut inner = process.inner_exclusive_access();
    // 1. 提取标志位
    let cloexec = (socket_type & 0o2000000) != 0;
    let nonblock = (socket_type & 0o4000) != 0;
    // 2. 提取真正的 socket 核心类型 (屏蔽掉标志位)
    let real_socket_type = socket_type & 0xff;
    const AF_UNIX: usize = 1;
    const AF_INET: usize = 2;
    const AF_NETLINK: usize = 16;
    if domain != AF_INET && domain != AF_UNIX && domain != AF_NETLINK {
        return crate::syscall::errno::Errno::EAFNOSUPPORT.as_isize();
    }
    // 3. 寻找空闲 FD
    let allocated_fd = inner.alloc_fd();
    // 4. 根据类型分配不同的 Socket
    let socket_file: Arc<dyn crate::fs::File> = if domain == AF_NETLINK {
        // netlink 
        Arc::new(crate::net::netlink::StandardNetlinkSocket::new(protocol as i32))
    } else if real_socket_type == 3 {
        // 如果是 RAW 套接字，分配 RawSocket，并传入协议号
        Arc::new(crate::net::socket::RawSocket::new(protocol as u8))
    } else if real_socket_type == 2 {
        // 如果是 UDP (SOCK_DGRAM)
        Arc::new(crate::net::socket::UdpSocket::new()) 
    } else {
        // 否则默认按 TCP (SOCK_STREAM) 处理
        Arc::new(crate::net::socket::TcpSocket::new()) 
    };
    let fd_desc = FileDescriptor {
        file: Some(socket_file),
        flags: FdFlags::from_bits_truncate(if nonblock { 0o4000 } else { 0 } | if cloexec { 0o2000000 } else { 0 }),
        status: if nonblock { 0o4000 } else { 0 },
    };
    let fd = if let Some(idx) = allocated_fd {
        inner.fd_table[idx] = fd_desc;
        idx
    } else {
        let idx = inner.fd_table.len();
        inner.fd_table.push(fd_desc);
        idx
    };
    warn!(
        "[kernel] sys_socket: pid={} created {} {} socket, protocol={}, nonblock={}, allocated fd={}",
        process.getpid(),                             // 当前进程 PID
        match domain {
            1 => "AF_UNIX",
            2 => "AF_INET",
            16 => "AF_NETLINK",
            _ => "UNKNOWN",
        },
        match real_socket_type { 3 => "RAW", 2 => "UDP", _ => "TCP" },// 核心类型字符串化
        protocol,                                 // 协议号
        nonblock,                                 // 是否是非阻塞
        fd                                        // 分配到的文件描述符
    );
    fd as isize
}

pub fn sys_socketpair(domain: usize, socket_type: usize, protocol: usize, sv: *mut u8) -> isize {
    const AF_UNIX: usize = 1;
    const SOCK_STREAM: usize = 1;
    const SOCK_DGRAM: usize = 2;
    const SOCK_NONBLOCK: usize = 0o4000;
    const SOCK_CLOEXEC: usize = 0o2000000;

    let task = current_task().unwrap();
    let process = task.process();
    let mut inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    if domain != AF_UNIX {
        return Errno::EAFNOSUPPORT.as_isize();
    }
    if protocol != 0 {
        return Errno::EPROTONOSUPPORT.as_isize();
    }
    let real_type = socket_type & 0xff;
    let socket_kind = match real_type {
        SOCK_STREAM => UnixSocketType::Stream,
        SOCK_DGRAM => UnixSocketType::Datagram,
        _ => return Errno::EPROTOTYPE.as_isize(),
    };

    let left_fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => return Errno::EMFILE.as_isize(),
    };
    let right_fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => {
            inner.clear_fd(left_fd);
            return Errno::EMFILE.as_isize();
        }
    };
    let (left, right) = UnixSocket::pair(socket_kind);
    let status = if (socket_type & SOCK_NONBLOCK) != 0 { SOCK_NONBLOCK } else { 0 };
    let fd_flags = FdFlags::from_bits_truncate(socket_type);
    inner.set_fd(left_fd, Arc::new(left), fd_flags, status);
    inner.set_fd(right_fd, Arc::new(right), fd_flags, status);
    drop(inner);

    let mut data = [0u8; 8];
    data[..4].copy_from_slice(&(left_fd as i32).to_ne_bytes());
    data[4..].copy_from_slice(&(right_fd as i32).to_ne_bytes());
    if !try_translated_write(token, sv as *mut [u8; 8], data) {
        return Errno::EFAULT.as_isize();
    }
    0
}
//端口分配器 POSIX 标准的临时端口范围通常是 49152 ~ 65535
static NEXT_EPHEMERAL_PORT: AtomicU16 = AtomicU16::new(49152);

fn alloc_ephemeral_port() -> u16 {
    let mut port = NEXT_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
    if  port < 49152 {
        NEXT_EPHEMERAL_PORT.store(49152, Ordering::Relaxed);
        port = 49152;
    }
    port
}
pub fn sys_bind(fd: usize, addr: *const u8, _addr_len: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); // 提早释放锁
    if let Some(_netlink_sock) = file.as_any().downcast_ref::<StandardNetlinkSocket>() {
        // 对于简化的 Netlink 实现，不需要真实的端口绑定逻辑，返回成功即可
        return 0;
    }
    //  Raw Socket，防止等会儿 ping 的时候报同样的错
    if let Some(_raw_sock) = file.as_any().downcast_ref::<crate::net::socket::RawSocket>() {
        return 0;
    }
    if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        // 从 addr 中安全读取端口信息
        let mut port = {
            if let Some(p) = try_translated_read(token, (addr as usize + 2) as *const u16) {
                u16::from_be(p)
            } else {
                return EFAULT.as_isize();
            }
        };
        if port == 0 {
            port = alloc_ephemeral_port();
        }
        
        // 绑定端口
        return udp_socket.bind(port);
    }
    if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
        // 1. 从用户态读取 16 字节的 sockaddr_in
        let sockaddr = {
            if let Some(s) = try_translated_read(token, addr as *const [u8; 16]) {
                s
            } else {
                return EFAULT.as_isize();
            }
        };
        // 2. 解析大端序的端口号
        let mut port = u16::from_be_bytes([sockaddr[2], sockaddr[3]]);
        if port == 0 {
            port = alloc_ephemeral_port();
        }
       *socket.local_port.lock() = Some(port);
        return 0;
        0
    } else {
        Errno::ENOTSOCK.as_isize()
    }
}

pub fn sys_listen(fd: usize, _backlog: i32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }
    //  提取 file 
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);
    // 检查是否是 TCP Socket
    if let Some(socket) = file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
        // 暂存的本地端口
        let mut port_lock = socket.local_port.lock();
        let port = if let Some(p) = *port_lock {
            p
        } else {
            // POSIX 标准允许未 bind 直接 listen，此时 OS 需隐式分配端口
            let new_port = alloc_ephemeral_port();
            *port_lock = Some(new_port);
            new_port
        };
        drop(port_lock);
        // 获取 smoltcp 内部的 socket，并真正listen 
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let smol_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(socket.handle);
        // smoltcp 会在此刻将状态机切换为 Listen
        match smol_socket.listen(port) {
           Ok(_) => {
                socket.is_listener.store(true, Ordering::SeqCst);
                0
            },
            Err(_) => crate::syscall::errno::Errno::EINVAL.as_isize(), // 可能是因为 socket 已经连接或关闭
        }
    } 
    // 拦截 UDP Socket 
    else if file.as_any().downcast_ref::<crate::net::socket::UdpSocket>().is_some() {
        crate::syscall::errno::Errno::EOPNOTSUPP.as_isize()
    } 
    else {
        crate::syscall::errno::Errno::ENOTSOCK.as_isize()
    }
}

pub fn sys_accept(fd: usize, addr: *mut u8, addrlen: *mut u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let token = process.inner_exclusive_access().memory_set.token(); 
    let (file, status) = {
            let inner = process.inner_exclusive_access();
            const O_PATH: usize = 0o10000000; 
            
            if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
                return crate::syscall::errno::Errno::EBADF.as_isize();
            }
            let status = inner.fd_table[fd].status;
            if (status & O_PATH) != 0 {
                return crate::syscall::errno::Errno::EBADF.as_isize();
            }
            (inner.fd_table[fd].file.as_ref().unwrap().clone(), status)
        };
    const O_NONBLOCK: usize = 0o4000;
    let fatal_signals = crate::task::SignalFlags::SIGKILL 
                | crate::task::SignalFlags::SIGTERM 
                | crate::task::SignalFlags::SIGINT 
                | crate::task::SignalFlags::SIGALRM;
    if let Some(orig_socket) = file.as_any().downcast_ref::<TcpSocket>() {
        let mut local_port = 0;
        let mut remote_ep = None;
        let start_time_ms = crate::timer::get_time_ms(); 
        // 设定一个超时时间， 12 秒
        let timeout_ms = 12_000;
        loop {
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let smol_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(orig_socket.handle);
            let state = smol_socket.state();
            drop(sockets);
            crate::net::net_poll(); 
            let mut is_established = false;
            {
                let mut sockets = crate::net::SOCKET_SET.exclusive_access();
                let smol_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(orig_socket.handle);
                let state = smol_socket.state();
                if state == State::Established || state == State::SynReceived|| state == State::CloseWait {
                    is_established = true;
                    if let Some(ep) = smol_socket.local_endpoint() {
                        local_port = ep.port;
                    }
                    remote_ep = smol_socket.remote_endpoint();
                }
                   
            }
            if is_established {
                break; 
            }
            let current_time_ms = crate::timer::get_time_ms();
            if current_time_ms - start_time_ms > timeout_ms {
                return crate::syscall::errno::Errno::EINTR.as_isize();
            }
            if (status & O_NONBLOCK) != 0 {
                return crate::syscall::errno::Errno::EAGAIN.as_isize();
            }
            crate::task::suspend_current_and_run_next();
            let task = current_task().unwrap();
            let pending_signals = task.inner_exclusive_access().signals; // 获取当前挂起的信号
            if pending_signals.intersects(fatal_signals) {
                return Errno::EINTR.as_isize();
            }
        }
        let mut inner = process.inner_exclusive_access();
        let new_fd = match inner.alloc_fd() {
            Some(idx) => idx,
            None => return crate::syscall::errno::Errno::EMFILE.as_isize(), 
        };
        let new_listener = Arc::new(TcpSocket::new());
        {
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let smol_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(new_listener.handle);
            if local_port != 0 {
                let _ = smol_socket.listen(local_port); // 让新 Socket 接管并监听原端口
            }
        }
        orig_socket.is_listener.store(false, core::sync::atomic::Ordering::SeqCst);
        inner.fd_table[fd].file = Some(new_listener); 
        inner.fd_table[new_fd] = FileDescriptor {
            file: Some(file.clone()), 
            flags: FdFlags::empty(),
            status: 0,
        };
        drop(inner); 
        // 把客户端的 IP 和端口写回给 addr 指针
        if addr as usize != 0 && addrlen as usize != 0 {
            if let Some(ep) = remote_ep {
                let family: u16 = 2; // AF_INET
                let port = ep.port;
                let mut ip = [0u8; 4];
                let smoltcp::wire::IpAddress::Ipv4(v4) = ep.addr;
                ip = v4.0;
                let mut sockaddr_bytes = [0u8; 16];
                sockaddr_bytes[0..2].copy_from_slice(&family.to_ne_bytes());
                sockaddr_bytes[2..4].copy_from_slice(&port.to_be_bytes());
                sockaddr_bytes[4..8].copy_from_slice(&ip);

                unsafe {
                    if let Some(user_len) = crate::mm::try_translated_read(token, addrlen) {
                        let copy_len = (user_len as usize).min(16);
                        let mut current_addr = addr as usize;
                        for i in 0..copy_len {
                            let _ = crate::mm::try_translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]);
                            current_addr += 1;
                        }
                        let _ = crate::mm::try_translated_write(token, addrlen, 16u32);
                    }
                }
            }
        }
        return new_fd as isize;
    } else {
        return crate::syscall::errno::Errno::ENOTSOCK.as_isize();
    }
}
/// 发送复杂消息 (Scatter IO)
/// 系统调用号: 211
pub fn sys_sendmsg(fd: usize, msg_ptr: *const MsgHdr, _flags: i32) -> isize {
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);
    if !file.writable() {
        return crate::syscall::errno::Errno::EACCES.as_isize();
    }
    let msg = crate::mm::translated_read(token, msg_ptr);
    let mut buffers = alloc::vec::Vec::new();
    for i in 0..msg.msg_iovlen {
        let iov_ptr = (msg.msg_iov + i * core::mem::size_of::<IoVec>()) as *const IoVec;
        let iov = crate::mm::translated_read(token, iov_ptr);
        
        if iov.iov_len > 0 {

            let mut iov_bufs = crate::mm::translated_byte_buffer(token, iov.iov_base as *const u8, iov.iov_len);
            buffers.append(&mut iov_bufs);
        }
    }


    let user_buf = crate::mm::UserBuffer::new(buffers);
    file.write(user_buf) as isize
}

/// 接收复杂消息 (Gather IO)
/// 系统调用号: 212
pub fn sys_recvmsg(fd: usize, msg_ptr: *mut MsgHdr, _flags: i32) -> isize {
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);

    /*if !file.readable() {
        return crate::syscall::errno::Errno::EACCES.as_isize();
    }*/

    // 1. 读出 MsgHdr 控制结构
    let mut msg = crate::mm::translated_read(token, msg_ptr);

    // 2. 遍历提取用户的读缓冲 (IoVec)
    let mut buffers = alloc::vec::Vec::new();
    for i in 0..msg.msg_iovlen {
        let iov_ptr = (msg.msg_iov + i * core::mem::size_of::<IoVec>()) as *const IoVec;
        let iov = crate::mm::translated_read(token, iov_ptr);
        
        if iov.iov_len > 0 {

            let mut iov_bufs = crate::mm::translated_byte_buffer_mut(token, iov.iov_base as *mut u8, iov.iov_len);
            buffers.append(&mut iov_bufs);
        }
    }


    let user_buf = crate::mm::UserBuffer::new(buffers);
    let read_len = file.read(user_buf);

    if read_len > 0 && msg.msg_name != 0 && msg.msg_namelen >= 12 {
        // 构造合法的 sockaddr_nl
        if file.as_any().is::<StandardNetlinkSocket>(){
        let mut sa_nl = [0u8; 12];
        sa_nl[0..2].copy_from_slice(&16u16.to_ne_bytes()); // nl_family = AF_NETLINK
        sa_nl[4..8].copy_from_slice(&0u32.to_ne_bytes());  // nl_pid = 0
        
        // 安全地将 12 字节写入用户态提供的 msg_name 指针
        let mut name_bufs = crate::mm::translated_byte_buffer_mut(token, msg.msg_name as *mut u8, 12);
        let mut current = 0;
        for buf in name_bufs.iter_mut() {
            let copy_len = buf.len().min(12 - current);
            buf[..copy_len].copy_from_slice(&sa_nl[current..current + copy_len]);
            current += copy_len;
            if current == 12 { break; }
        }
        
        // 更新实际写回的名字长度
        msg.msg_namelen = 12; 
        crate::mm::translated_write(token, msg_ptr, msg);
        }
    }

    // 如果读不到数据，返回 EAGAIN 让应用层重试
    if read_len == 0 {
        if (_flags & 0x40) != 0 { // MSG_DONTWAIT
            return EAGAIN.as_isize(); 
        } else {

            return 0; 
        }
    }
    read_len as isize
}

// Linux 标准的网络套接字常量定义
const SOL_SOCKET: i32 = 1;  // 配置层级：通用套接字层
const SO_SNDBUF: i32 = 7;  // 选项名称：发送缓冲区大小
const SO_RCVBUF: i32 = 8;  // 选项名称：接收缓冲区大小

///  获取套接字选项参数
/// - fd: 套接字文件描述符
/// - level: 协议栈层级 
/// - optname: 欲查询的选项名 
/// - optval: 指向用户态缓冲区的指针，用于接收查询结果
/// - optlen: 指向用户态 u32 的指针，输入时表示 optval 的最大容量，输出时表示实际写入的长度
pub fn sys_getsockopt(
    fd: usize, 
    level: i32, 
    optname: i32, 
    optval: *mut u8, 
    optlen: *mut u32
) -> isize {
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }
   
    let _file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);


    const SOL_SOCKET: i32 = 1;  // 通用套接字
    const SO_SNDBUF: i32 = 7;  // 发送缓冲区大小
    const SO_RCVBUF: i32 = 8;  // 接收缓冲区大小
    let mut len = crate::mm::translated_read(token, optlen);
    if level == SOL_SOCKET {
        match optname {
            
            SO_SNDBUF | SO_RCVBUF => {
                if len < 4 {
                    return EINVAL.as_isize(); 
                }
                // 统一回报 16384 字节 (4 字节 i32 结构)
                let buffer_size: i32 = 16384; 
                let bytes = buffer_size.to_ne_bytes();
                let mut val_bufs = crate::mm::translated_byte_buffer_mut(token, optval, 4);
                let mut current = 0;
                for buf in val_bufs.iter_mut() {
                    let copy_len = buf.len().min(4 - current);
                    buf[..copy_len].copy_from_slice(&bytes[current..current + copy_len]);
                    current += copy_len;
                    if current == 4 { break; }
                }
                crate::mm::translated_write(token, optlen, 4u32);
                return 0; 
            }
            _ => {
                if len >= 4 {
                    let bytes = 0i32.to_ne_bytes();
                    let mut val_bufs = crate::mm::translated_byte_buffer_mut(token, optval, 4);
                    let mut current = 0;
                    for buf in val_bufs.iter_mut() {
                        let copy_len = buf.len().min(4 - current);
                        buf[..copy_len].copy_from_slice(&bytes[current..current + copy_len]);
                        current += copy_len;
                        if current == 4 { break; }
                    }
                    crate::mm::translated_write(token, optlen, 4u32);
                }
                return 0;
            }
        }
    }
    if len >= 4 {
        let bytes = 0i32.to_ne_bytes();
        let mut val_bufs = crate::mm::translated_byte_buffer_mut(token, optval, 4);
        let mut current = 0;
        for buf in val_bufs.iter_mut() {
            let copy_len = buf.len().min(4 - current);
            buf[..copy_len].copy_from_slice(&bytes[current..current + copy_len]);
            current += copy_len;
            if current == 4 { break; }
        }
        crate::mm::translated_write(token, optlen, 4u32);
    }
    
    0 
}

pub fn sys_shutdown(fd: usize, how: i32) -> isize {
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize(); 
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); 

    if let Some(tcp_wrapper) = file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
        if how == 1 || how == 2 {
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(tcp_wrapper.handle);
            socket.close();
            drop(sockets); 
            net_poll();
            let mut sockets = crate::net::SOCKET_SET.exclusive_access();
            let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(tcp_wrapper.handle);
            drop(sockets);
        }
    } else if let Some(_udp_wrapper) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
    } else {
        return ENOTSOCK.as_isize(); 
    }
    0
}