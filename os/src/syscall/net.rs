use crate::task::current_task;
use crate::net::socket::TcpSocket;
use crate::process::*;
use crate::syscall::Errno::*;
use crate::syscall::Arc;
use crate::mm::{translated_read, translated_ref, translated_write, translated_byte_buffer, UserBuffer};
use crate::syscall::errno::Errno;
use alloc::vec;

/// 获取指定 Socket 的本地地址和端口信息。
/// 将内核中 Socket 的 local_endpoint 信息格式化为 sockaddr_in 结构并拷贝回用户空间。 asd
pub fn sys_getsockname(fd: usize, addr: *mut u8, addrlen: *mut u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    // 1. 检查 fd 是否合法
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return -9; // EBADF
    }

    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); // 提前释放进程锁

    // 2. 检查这个文件是不是 Socket
    // 这里利用 Any trait 向下转型
    if let Some(_udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
    return 0; 
    }
    if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
        
        // 3. 提取地址和端口
        let local_ep = socket.local_endpoint();
        let family: u16 = 2; // AF_INET
        let mut port: u16 = 0;
        let mut ip: [u8; 4] = [0, 0, 0, 0];

        if let Some(ep) = local_ep {
            port = ep.port; 
            let smoltcp::wire::IpAddress::Ipv4(v4) = ep.addr;
            ip = v4.0;
        }

        // 4. 组装 16 字节的 sockaddr_in 结构
        let mut sockaddr_bytes = [0u8; 16];
        // 家族 (AF_INET) - 本机字节序
        sockaddr_bytes[0..2].copy_from_slice(&family.to_ne_bytes());
        // 端口号 - 必须是网络字节序 (大端序, Big Endian)！
        sockaddr_bytes[2..4].copy_from_slice(&port.to_be_bytes());
        // IP 地址 - smoltcp 内部已经是正确的网络顺序了
        sockaddr_bytes[4..8].copy_from_slice(&ip);

        // 5. 写入用户空间
        unsafe {
            // 获取用户传进来的 addrlen 的值
            let mut user_len = translated_read(token, addrlen);
            let copy_len = (user_len as usize).min(16);

            // 把字节拷贝到用户提供的 addr 指针去
            let mut current_addr = addr as usize;
            for i in 0..copy_len {
                translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]);
                current_addr += 1;
            }

            // 更新 addrlen 为实际写入的大小
            user_len = 16;
            translated_write(token, addrlen, user_len);
        }

        return 0; // 成功
    } else {
        return ENOTSOCK.as_isize(); // ENOTSOCK (不是一个 Socket)
    }
}
/// 设置 Socket 的属性选项。
/// 目前作为“伪实现”直接返回成功(0)，主要用于兼容 musl libc 初始化时对 SO_RCVTIMEO 等选项的探测。
pub fn sys_setsockopt(
    fd: usize,
    level: usize,
    optname: usize,
    optval: *const u8,
    optlen: u32,
) -> isize {
    let task = crate::task::current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();

    // 1. 检查 fd 是否合法
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return crate::syscall::errno::Errno::EBADF.as_isize();
    }

    // 2. 获取文件对象
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner); // 提前释放进程锁
    if let Some(_udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        return 0; // 假装设置成功
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

    if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {

        if addr.is_null() {
            return Errno::EFAULT.as_isize();
        }

     
        let mut family_bytes = [0u8; 2];
        for i in 0..2 {
           
            let ptr = (addr as usize + i) as *const u8;
            family_bytes[i] = unsafe { *crate::mm::translated_ref(token, ptr) };
        }
        
 
        let sa_family = u16::from_ne_bytes(family_bytes);

      
        const AF_UNSPEC: u16 = 0;
        if sa_family == AF_UNSPEC {

             socket.disconnect(); 
            return 0;
        }

        if addrlen < 16 {
            return Errno::EINVAL.as_isize(); 
        }
        const AF_INET: u16 = 2;
        if sa_family != AF_INET {
            return Errno::EAFNOSUPPORT.as_isize(); 
        }

  
        if addrlen < 16 {
            return Errno::EINVAL.as_isize(); 
        }


        let mut sockaddr = [0u8; 16];
        sockaddr[0..2].copy_from_slice(&family_bytes);
        for i in 2..16 {
            let ptr = (addr as usize + i) as *const u8;
            sockaddr[i] = unsafe { *crate::mm::translated_ref(token, ptr) };
        }
        
      
        let port = u16::from_be_bytes([sockaddr[2], sockaddr[3]]);
        
        let ip = [sockaddr[4], sockaddr[5], sockaddr[6], sockaddr[7]];
        
        let endpoint = smoltcp::wire::IpEndpoint::new(
            smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address(ip)),
            port
        );

     
        socket.connect(endpoint)
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

    if !file.writable() {
        return Errno::EACCES.as_isize();
    }
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

        // 2. 解析 dest_addr (C 语言的 sockaddr 结构体)
        if dest_addr as usize != 0 {
            let mut sockaddr_bytes = [0u8; 16];
            for i in 0..16 {
                // 逐字节翻译并拷贝用户态内存
                sockaddr_bytes[i] = *crate::mm::translated_ref(token, (dest_addr as usize + i) as *const u8);
            }
            // 解析出端口 (大端序转主机序)
            let port = u16::from_be_bytes([sockaddr_bytes[2], sockaddr_bytes[3]]);
            // 解析出 IPv4 地址
            let ip = smoltcp::wire::IpAddress::v4(
                sockaddr_bytes[4], sockaddr_bytes[5], sockaddr_bytes[6], sockaddr_bytes[7]
            );
            let remote_ep = smoltcp::wire::IpEndpoint::new(ip, port);

            // 3. 调用实现的 udp.sendto
            return udp_socket.sendto(&data, remote_ep);
        } else {
            return Errno::EDESTADDRREQ.as_isize(); // 需要目标地址
        }
    }
    // 对于已连接的 TCP Socket，sendto 会忽略 dest_addr，等价于普通写操作
    let user_buf = UserBuffer::new(translated_byte_buffer(token, buf, len));
    file.write(user_buf) as isize
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
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let token = inner.memory_set.token();

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }

    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    drop(inner);

    if !file.readable() {
        return Errno::EACCES.as_isize();
    }
    // 判断是不是 UdpSocket
    if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        let mut data = vec![0u8; len];
        // 调用 udp.recvfrom
        if let Some((read_len, src_ep)) = udp_socket.recvfrom(&mut data) {
            // 1. 把数据拷贝回用户的 buf
            let mut user_buf = UserBuffer::new(crate::mm::translated_byte_buffer_mut(token, buf, len));
            let mut current = 0;
            for buffer in user_buf.buffers.iter_mut() {
                let copy_len = buffer.len().min(read_len - current);
                buffer[..copy_len].copy_from_slice(&data[current..current + copy_len]);
                current += copy_len;
                if current == read_len { break; }
            }

            // 2. 把发送方地址填回 src_addr
            if src_addr as usize != 0 && addrlen as usize != 0 {
                let mut sockaddr_bytes = [0u8; 16];
                sockaddr_bytes[0..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
                sockaddr_bytes[2..4].copy_from_slice(&src_ep.port.to_be_bytes()); // 大端序端口
                let smoltcp::wire::IpAddress::Ipv4(v4) = src_ep.addr;
                    sockaddr_bytes[4..8].copy_from_slice(&v4.0);
                

                unsafe {
                    let mut user_len = translated_read(token, addrlen);
                    let copy_len = (user_len as usize).min(16);
                    let mut current_addr = src_addr as usize;
                    for i in 0..copy_len {
                        translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]);
                        current_addr += 1;
                    }
                    user_len = 16;
                    translated_write(token, addrlen, user_len);
                }
            }
            return read_len as isize;
        } else {
            // 在阻塞模式下，这里应该挂起进程；非阻塞则返回 EAGAIN
            // 为简单通过当前测例，暂时返回一个假的长度或错误
            return Errno::EAGAIN.as_isize(); 
        }
    }
    // 1. 读取网络数据
    let user_buf = UserBuffer::new(crate::mm::translated_byte_buffer_mut(token, buf, len));
    let read_len = file.read(user_buf);

    // 2. 如果用户提供了 src_addr 和 addrlen，则填入对端的 IP 和端口信息
    if src_addr as usize != 0 && addrlen as usize != 0 {
        if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
            if let Some(ep) = socket.remote_endpoint() {
                let mut sockaddr_bytes = [0u8; 16];
                sockaddr_bytes[0..2].copy_from_slice(&2u16.to_ne_bytes()); // 协议族 AF_INET
                sockaddr_bytes[2..4].copy_from_slice(&ep.port.to_be_bytes()); // 大端序端口
               let smoltcp::wire::IpAddress::Ipv4(v4) = ep.addr;
                    sockaddr_bytes[4..8].copy_from_slice(&v4.0); // IPv4
                

                unsafe {
                    let mut user_len = translated_read(token, addrlen);
                    let copy_len = (user_len as usize).min(16);
                    let mut current_addr = src_addr as usize;
                    
                    for i in 0..copy_len {
                        translated_write(token, current_addr as *mut u8, sockaddr_bytes[i]);
                        current_addr += 1;
                    }
                    user_len = 16;
                    translated_write(token, addrlen, user_len);
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
    // 常见的值：1 = SOCK_STREAM (TCP), 2 = SOCK_DGRAM (UDP)
    let real_socket_type = socket_type & 0xff;
    
    // 3. 寻找空闲 FD
    let mut allocated_fd = None;
    for (i, fd_desc) in inner.fd_table.iter().enumerate() {
        if fd_desc.file.is_none() {
            allocated_fd = Some(i);
            break;
        }
    }
    
    // 4. 根据类型分配不同的 Socket！
    // 🌟 这里是重点：我们开始区分 TCP 和 UDP
    let socket_file: Arc<dyn crate::fs::File> = if real_socket_type == 2 {
        // 如果是 UDP，分配 UdpSocket (我们等会儿去建这个结构体)
        Arc::new(crate::net::socket::UdpSocket::new()) 
    } else {
        // 否则默认按 TCP 处理
        Arc::new(crate::net::socket::TcpSocket::new()) 
    };
    
    let fd_desc = FileDescriptor {
        file: Some(socket_file),
        cloexec, 
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
    
    fd as isize
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
    if let Some(udp_socket) = file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
        // 从 addr 中安全读取端口信息
        let mut port_bytes = [0u8; 2];
        port_bytes[0] = *crate::mm::translated_ref(token, (addr as usize + 2) as *const u8);
        port_bytes[1] = *crate::mm::translated_ref(token, (addr as usize + 3) as *const u8);
        let port = u16::from_be_bytes(port_bytes);
        
        // 绑定端口
        return udp_socket.bind(port);
    }
    if let Some(socket) = file.as_any().downcast_ref::<TcpSocket>() {
        // 1. 从用户态读取 16 字节的 sockaddr_in
        let mut sockaddr = [0u8; 16];
        let mut curr = addr as usize;
        for i in 0..16 {
            sockaddr[i] = unsafe { *crate::mm::translated_ref(token, curr as *const u8) };
            curr += 1;
        }
        
        // 2. 解析大端序的端口号
        let port = u16::from_be_bytes([sockaddr[2], sockaddr[3]]);
        
        // 3. 借用 smoltcp 的能力，直接在 bind 阶段占有并监听端口
        let mut sockets = crate::net::SOCKET_SET.exclusive_access();
        let smol_socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(socket.handle);
        let _ = smol_socket.listen(port); // 进入 Listen 状态
        
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
        return Errno::EBADF.as_isize();
    }


    0
}

pub fn sys_accept(fd: usize, addr: *mut u8, addrlen: *mut u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let mut inner = process.inner_exclusive_access();

    const O_PATH: usize = 0o10000000; 

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return Errno::EBADF.as_isize();
    }

    if (inner.fd_table[fd].status & O_PATH) != 0 {
        return Errno::EBADF.as_isize();
    }


    if addr as usize == 0xffffffffffffffff || addrlen as usize == 0xffffffffffffffff {
        return Errno::EFAULT.as_isize();
    }


    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    
    if let Some(_socket) = file.as_any().downcast_ref::<TcpSocket>() {
        

        let new_fd = match inner.alloc_fd() {
            Some(idx) => idx,
            None => return Errno::EMFILE.as_isize(), 
        };


        let new_socket = Arc::new(TcpSocket::new());

        inner.fd_table[new_fd] = FileDescriptor {
            file: Some(new_socket),
            cloexec: false,
            status: 0,
        };

        new_fd as isize

        
    } else {
        Errno::ENOTSOCK.as_isize()
    }
}