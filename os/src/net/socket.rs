// os/src/net/socket.rs
use alloc::vec;
use core::any::Any;
use smoltcp::socket::tcp::{Socket as TcpSocketSmol, SocketBuffer};
use smoltcp::iface::SocketHandle;

use crate::net::SOCKET_SET;
use crate::fs::{File, Stat};    // 引入 File trait 和 Stat
use crate::mm::UserBuffer;      // 引入 UserBuffer

pub struct TcpSocket {
    pub handle: SocketHandle,
}

impl TcpSocket {
    pub fn new() -> Self {
        let rx_buffer = SocketBuffer::new(vec![0; 8192]);
        let tx_buffer = SocketBuffer::new(vec![0; 8192]);
        let socket = TcpSocketSmol::new(rx_buffer, tx_buffer);
        let handle = SOCKET_SET.exclusive_access().add(socket);
        Self { handle }
    }
}

impl File for TcpSocket {
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut sockets = SOCKET_SET.exclusive_access();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(self.handle);

        if !socket.may_recv() {
            return 0; 
        }

        let mut temp_buf = vec![0u8; buf.len()];
        let recv_len = socket.recv_slice(&mut temp_buf).unwrap_or(0);

        let mut current = 0;
        for buffer in buf.buffers.iter_mut() {
            let copy_len = buffer.len().min(recv_len - current);
            buffer[..copy_len].copy_from_slice(&temp_buf[current..current + copy_len]);
            current += copy_len;
            if current == recv_len { break; }
        }
        
        recv_len
    }

    fn write(&self, buf: UserBuffer) -> usize {
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

        socket.send_slice(&temp_buf).unwrap_or(0)
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
            __unused: [0; 1], // 如果这里报错说类型不匹配，可能需要改成 [0; 2] 或者其他数组形式
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}