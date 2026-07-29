// os/src/net/netlink.rs  Netlink 协议族实现)
use crate::fs::{File, Stat};
use crate::drivers::net::{EthernetDevice, NET_DEVICE};
use crate::mm::UserBuffer;
use crate::net::Vec;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use core::mem::size_of;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use spin::Mutex;

/// Netlink 消息头
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NlMsgHdr {
    pub nlmsg_len: u32,   // 包含头部在内的总长度
    pub nlmsg_type: u16,  // 消息类型 (例如: RTM_NEWADDR)
    pub nlmsg_flags: u16, // 标志位 (例如: NLM_F_ACK)
    pub nlmsg_seq: u32,   // 序列号
    pub nlmsg_pid: u32,   // 发送进程的 Port ID (通常是 PID)
}

/// Netlink 错误/ACK 响应体
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NlMsgErr {
    pub error: i32,    // 错误码 (0 代表成功/ACK)
    pub msg: NlMsgHdr, // 触发该错误的原消息头
}

/// 接口地址消息头
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IfAddressMsg {
    pub ifa_family: u8,    // 地址族 (AF_INET / AF_INET6)
    pub ifa_prefixlen: u8, // 子网掩码长度 (如 24)
    pub ifa_flags: u8,     // 接口标志
    pub ifa_scope: u8,     // 作用域
    pub ifa_index: u32,    // 网卡接口索引 (Interface Index)
}

/// 路由属性头，TLV 格式
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RtAttr {
    pub rta_len: u16,  // 属性总长度 (包含这 4 字节自身)
    pub rta_type: u16, // 属性类型 (例如: IFA_LOCAL, IFA_ADDRESS)
}

// Netlink 标准常量定义
const NLMSG_ERROR: u16 = 2;
const RTM_NEWADDR: u16 = 20;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;

// Netlink 4 字节强制对齐宏的标准 Rust 实现
fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

// Netlink 套接字结构

pub struct StandardNetlinkSocket {
    pub protocol: i32, // 记录创建时指定的协议，如 NETLINK_ROUTE
    pub rx_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl StandardNetlinkSocket {
    pub fn new(protocol: i32) -> Self {
        Self {
            protocol,
            rx_buffer: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    ///  Netlink 属性树（rtattr）解析器
    fn parse_rt_attributes(&self, payload: &[u8], prefix_len: u8) {
        let mut offset = 0;
        // 循环迭代解析标准的 TLV (Type-Length-Value) 链表
        while offset + size_of::<RtAttr>() <= payload.len() {
            // 安全读取属性头
            let rta = unsafe { &*(payload.as_ptr().add(offset) as *const RtAttr) };
            let rta_len = rta.rta_len as usize;
            // 安全边界防守：防止恶意构造的长度导致死循环或内存越界
            if rta_len < size_of::<RtAttr>() || offset + rta_len > payload.len() {
                break;
            }
            // 提取 Value 的数据切片
            let value_start = offset + size_of::<RtAttr>();
            let value_end = offset + rta_len;
            let value_payload = &payload[value_start..value_end];
            // 匹配标准类型：IFA_ADDRESS 或 IFA_LOCAL 且长度为 4 字节 (IPv4)
            if (rta.rta_type == IFA_ADDRESS || rta.rta_type == IFA_LOCAL)
                && value_payload.len() == 4
            {
                let ip = Ipv4Address::new(
                    value_payload[0],
                    value_payload[1],
                    value_payload[2],
                    value_payload[3],
                );
                let cidr = IpCidr::new(IpAddress::Ipv4(ip), prefix_len as u8);
                // 调用内核静态网卡驱动更新 IP 池
                crate::net::NET_IFACE
                    .exclusive_access()
                    .update_ip_addrs(|addrs| {
                        if !addrs.iter().any(|a| *a == cidr) {
                            let _ = addrs.push(cidr);
                        }
                    });
                break;
            }
            offset += nlmsg_align(rta_len);
        }
    }
}

// 实现抽象的 File 特征接口

impl File for StandardNetlinkSocket {
    fn readable(&self) -> bool {
        !self.rx_buffer.lock().is_empty()
    }
    fn writable(&self) -> bool {
        true
    }

    fn write(&self, buf: UserBuffer) -> usize {
        const RTM_NEWLINK: u16 = 16;
        const RTM_NEWADDR: u16 = 20;
        const RTM_GETLINK: u16 = 18;
        const RTM_GETADDR: u16 = 22;
        const NLMSG_DONE: u16 = 3;
        const NLMSG_ERROR: u16 = 2;
        const NLM_F_DUMP: u16 = 0x300;
        const NLM_F_MULTI: u16 = 2;
        const IFA_LOCAL: u16 = 2;
        const IFLA_IFNAME: u16 = 3;
        let total_len = buf.len();
        let mut data = vec![0u8; total_len];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            data[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }
        let mut offset = 0;
        while offset + size_of::<NlMsgHdr>() <= data.len() {
            let hdr = unsafe { &*(data.as_ptr().add(offset) as *const NlMsgHdr) };
            let msg_len = hdr.nlmsg_len as usize;
            if msg_len < size_of::<NlMsgHdr>() || offset + msg_len > data.len() {
                break;
            }
            let aligned_msg_len = (msg_len + 3) & !3;
            let msg_data = &data[offset..offset + msg_len];
            let mut rx_lock = self.rx_buffer.lock();
            if hdr.nlmsg_type == RTM_GETLINK {
                #[repr(C)]
                struct IfInfoMsg {
                    ifi_family: u8,
                    __pad: u8,
                    ifi_type: u16,
                    ifi_index: i32,
                    ifi_flags: u32,
                    ifi_change: u32,
                }
                const IFLA_ADDRESS: u16 = 1;

                #[repr(C)]
                struct LinkReplyPacket {
                    nl_hdr: NlMsgHdr,
                    if_msg: IfInfoMsg,
                    attr_name_hdr: RtAttr,
                    ifname: [u8; 8],
                    attr_mac_hdr: RtAttr,
                    mac_addr: [u8; 8],
                }

                let mut real_flags = 0x0002; // IFF_BROADCAST
                real_flags |= 0x0001; // IFF_UP
                real_flags |= 0x0040; // IFF_RUNNING
                real_flags |= 0x1000; // IFF_LOWER_UP

                let real_index = 2;
                let real_mac = NET_DEVICE.mac_address();

                let mut packet = LinkReplyPacket {
                    nl_hdr: NlMsgHdr {
                        nlmsg_len: 56,
                        nlmsg_type: RTM_NEWLINK,
                        nlmsg_flags: NLM_F_MULTI,
                        nlmsg_seq: hdr.nlmsg_seq,
                        nlmsg_pid: hdr.nlmsg_pid,
                    },
                    if_msg: IfInfoMsg {
                        ifi_family: 0,
                        __pad: 0,
                        ifi_type: 1,
                        ifi_index: real_index,
                        ifi_flags: real_flags,
                        ifi_change: 0,
                    },
                    attr_name_hdr: RtAttr {
                        rta_len: 9,
                        rta_type: IFLA_IFNAME,
                    },
                    ifname: [0; 8],
                    attr_mac_hdr: RtAttr {
                        rta_len: 10,
                        rta_type: IFLA_ADDRESS,
                    },
                    mac_addr: [0; 8],
                };
                packet.ifname[0..5].copy_from_slice(b"eth0\0");
                packet.mac_addr[0..6].copy_from_slice(&real_mac);

                let mut link_reply = vec![0u8; 56];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &packet as *const _ as *const u8,
                        link_reply.as_mut_ptr(),
                        56,
                    );
                }
                rx_lock.push_back(link_reply);

                let mut done_reply = vec![0u8; 16];
                let done_hdr = NlMsgHdr {
                    nlmsg_len: 16,
                    nlmsg_type: NLMSG_DONE,
                    nlmsg_flags: NLM_F_MULTI,
                    nlmsg_seq: hdr.nlmsg_seq,
                    nlmsg_pid: hdr.nlmsg_pid,
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &done_hdr as *const _ as *const u8,
                        done_reply.as_mut_ptr(),
                        16,
                    );
                }
                rx_lock.push_back(done_reply);
            } else if hdr.nlmsg_type == RTM_GETADDR {
                let iface = crate::net::NET_IFACE.exclusive_access();
                for cidr in iface.ip_addrs().iter() {
                    let smoltcp::wire::IpCidr::Ipv4(v4_cidr) = cidr;
                    let addr = v4_cidr.address();
                    let ip_bytes = addr.as_bytes();
                    let prefix_len = v4_cidr.prefix_len();

                    let mut addr_reply = vec![0u8; 32];
                    let reply_hdr = NlMsgHdr {
                        nlmsg_len: 32,
                        nlmsg_type: RTM_NEWADDR,
                        nlmsg_flags: NLM_F_MULTI,
                        nlmsg_seq: hdr.nlmsg_seq,
                        nlmsg_pid: hdr.nlmsg_pid,
                    };
                    let ifa = IfAddressMsg {
                        ifa_family: 2,
                        ifa_prefixlen: prefix_len as u8,
                        ifa_flags: 0,
                        ifa_scope: 0,
                        ifa_index: 2,
                    };
                    let rta = RtAttr {
                        rta_len: 8,
                        rta_type: IFA_LOCAL,
                    };

                    unsafe {
                        let ptr = addr_reply.as_mut_ptr();
                        core::ptr::copy_nonoverlapping(
                            &reply_hdr as *const _ as *const u8,
                            ptr,
                            size_of::<NlMsgHdr>(),
                        );
                        core::ptr::copy_nonoverlapping(
                            &ifa as *const _ as *const u8,
                            ptr.add(16),
                            size_of::<IfAddressMsg>(),
                        );
                        core::ptr::copy_nonoverlapping(
                            &rta as *const _ as *const u8,
                            ptr.add(24),
                            size_of::<RtAttr>(),
                        );
                    }
                    addr_reply[28..32].copy_from_slice(ip_bytes);
                    rx_lock.push_back(addr_reply);
                }

                let mut done_reply = vec![0u8; 16];
                let done_hdr = NlMsgHdr {
                    nlmsg_len: 16,
                    nlmsg_type: NLMSG_DONE,
                    nlmsg_flags: NLM_F_MULTI,
                    nlmsg_seq: hdr.nlmsg_seq,
                    nlmsg_pid: hdr.nlmsg_pid,
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &done_hdr as *const _ as *const u8,
                        done_reply.as_mut_ptr(),
                        16,
                    );
                }
                rx_lock.push_back(done_reply);
            } else {
                // 普通的请求 (如 RTM_NEWADDR 添加 IP)
                if hdr.nlmsg_type == RTM_NEWADDR {
                    let ifa_offset = size_of::<NlMsgHdr>();
                    if ifa_offset + size_of::<IfAddressMsg>() <= msg_len {
                        let ifa =
                            unsafe { &*(msg_data.as_ptr().add(ifa_offset) as *const IfAddressMsg) };
                        let attr_offset = ifa_offset + size_of::<IfAddressMsg>();
                        if attr_offset < msg_len {
                            let attr_payload = &msg_data[attr_offset..msg_len];
                            self.parse_rt_attributes(attr_payload, ifa.ifa_prefixlen);
                        }
                    }
                }
                let ack_hdr = NlMsgHdr {
                    nlmsg_len: 36,
                    nlmsg_type: NLMSG_ERROR,
                    nlmsg_flags: 0,
                    nlmsg_seq: hdr.nlmsg_seq,
                    nlmsg_pid: hdr.nlmsg_pid,
                };
                let ack_err = NlMsgErr {
                    error: 0,
                    msg: *hdr,
                };
                let mut ack_packet = vec![0u8; 36];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &ack_hdr as *const _ as *const u8,
                        ack_packet.as_mut_ptr(),
                        size_of::<NlMsgHdr>(),
                    );
                    core::ptr::copy_nonoverlapping(
                        &ack_err as *const _ as *const u8,
                        ack_packet.as_mut_ptr().add(size_of::<NlMsgHdr>()),
                        size_of::<NlMsgErr>(),
                    );
                }
                rx_lock.push_back(ack_packet);
            }
            offset += aligned_msg_len;
        }

        total_len
    }
    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut rx_lock = self.rx_buffer.lock();

        let mut flat_data = alloc::vec::Vec::new();
        while let Some(packet) = rx_lock.front() {
            if flat_data.len() + packet.len() > buf.len() {
                break;
            }
            let packet = rx_lock.pop_front().unwrap();
            flat_data.extend_from_slice(&packet);
        }

        if flat_data.is_empty() {
            return 0;
        }

        let mut current = 0;
        for buffer in buf.buffers.iter_mut() {
            let copy_len = buffer.len().min(flat_data.len() - current);
            if copy_len == 0 {
                break;
            }
            buffer[..copy_len].copy_from_slice(&flat_data[current..current + copy_len]);
            current += copy_len;
            if current == flat_data.len() {
                break;
            }
        }

        current
    }

    fn get_stat(&self) -> Stat {
        // 保持原样，标记为普通 Socket 文件类型
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

    fn get_perm(&self) -> crate::auth::PermStat {
        let stat = self.get_stat();
        //从 get_stat 的 mode 中安全截取并转换为 FileMode 权限格式
        let mode = crate::auth::FileMode::from_bits_truncate(stat.mode as u16);
        crate::auth::PermStat {
            mode,
            uid: stat.uid,
            gid: stat.gid,
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        // Socket 不是目录，返回 -1 代表无法进行目录项遍历
        -1
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn info_type(&self) {
        info!("StandardNetlinkSocket: protocol = {}", self.protocol);
    }
}
