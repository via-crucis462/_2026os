// os/src/net/netlink.rs  Netlink 协议族实现)
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;
use core::mem::size_of;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use crate::fs::{File, Stat};
use crate::mm::UserBuffer;
use alloc::vec;
use crate::net::Vec;





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
    pub error: i32,       // 错误码 (0 代表成功/ACK)
    pub msg: NlMsgHdr,    // 触发该错误的原消息头
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
            if (rta.rta_type == IFA_ADDRESS || rta.rta_type == IFA_LOCAL) && value_payload.len() == 4 {
                let ip = Ipv4Address::new(value_payload[0], value_payload[1], value_payload[2], value_payload[3]);
                let cidr = IpCidr::new(IpAddress::Ipv4(ip), prefix_len as u8);

                // 调用内核唯一的网络总管更新 IP 池
                crate::net::NET_IFACE.exclusive_access().update_ip_addrs(|addrs| {
                    if !addrs.iter().any(|a| *a == cidr) {
                        let _ = addrs.push(cidr);
                    }
                });
                break;
            }

            // 遵照标准：属性的步进必须强制 4 字节对齐
            offset += nlmsg_align(rta_len);
        }
    }
}

// 实现抽象的 File 特征接口

impl File for StandardNetlinkSocket {
    fn readable(&self) -> bool { !self.rx_buffer.lock().is_empty() }
    fn writable(&self) -> bool { true }

    fn write(&self, buf: UserBuffer) -> usize {
        let total_len = buf.len();
        let mut data = vec![0u8; total_len];
        let mut current = 0;
        for buffer in buf.buffers.iter() {
            let copy_len = buffer.len();
            data[current..current + copy_len].copy_from_slice(buffer);
            current += copy_len;
        }

        if data.len() < size_of::<NlMsgHdr>() {
            return total_len;
        }

        let hdr = unsafe { &*(data.as_ptr() as *const NlMsgHdr) };

        // 核心常量定义
        const RTM_NEWLINK: u16 = 16;
        const RTM_NEWADDR: u16 = 20;
        const RTM_GETLINK: u16 = 18;
        const RTM_GETADDR: u16 = 22;
        const NLMSG_DONE: u16 = 3;
        const NLMSG_ERROR: u16 = 2;
        const NLM_F_DUMP: u16 = 0x300;
        const NLM_F_MULTI: u16 = 2;
        const IFA_ADDRESS: u16 = 1;
        const IFA_LOCAL: u16 = 2;
        const IFLA_IFNAME: u16 = 3;


        // 用户态发起 DUMP 请求 (例如：ip addr show 或添加后触发的查询)

        if (hdr.nlmsg_flags & NLM_F_DUMP) != 0 || hdr.nlmsg_type == RTM_GETLINK || hdr.nlmsg_type == RTM_GETADDR {
            let mut rx_lock = self.rx_buffer.lock();

            // 1. 如果请求的是链路层状态 (GETLINK)，伪造一个标准的 eth0 链路层通知
            if hdr.nlmsg_type == RTM_GETLINK {
                let mut link_reply = vec![0u8; 44];
                let reply_hdr = NlMsgHdr {
                    nlmsg_len: 32,
                    nlmsg_type: RTM_NEWLINK,
                    nlmsg_flags: NLM_F_MULTI,
                    nlmsg_seq: hdr.nlmsg_seq,
                    nlmsg_pid: hdr.nlmsg_pid,
                };
                unsafe { core::ptr::copy_nonoverlapping(&reply_hdr as *const NlMsgHdr as *const u8, link_reply.as_mut_ptr(), size_of::<NlMsgHdr>()); }
                link_reply[16] = 0; // AF_UNSPEC
                link_reply[18] = 1; // ARPHRD_ETHER
                link_reply[20..24].copy_from_slice(&1u32.to_ne_bytes()); // ifindex = 1 (网卡序号)
                link_reply[24..28].copy_from_slice(&0x1003u32.to_ne_bytes()); // flags: IFF_UP | IFF_RUNNING
                let rta_name = RtAttr {
                    rta_len: 9, // RtAttr头(4) + "eth0\0"(5) = 9 字节
                    rta_type: IFLA_IFNAME, // 3: 网卡名属性
                };
                unsafe { core::ptr::copy_nonoverlapping(&rta_name as *const RtAttr as *const u8, link_reply.as_mut_ptr().add(32), size_of::<RtAttr>()); }
                link_reply[36..41].copy_from_slice(b"eth0\0");
                rx_lock.push_back(link_reply);
            }

            // 2.：如果请求的是地址列表 (GETADDR)，实时去底层网卡得到IP！
            if hdr.nlmsg_type == RTM_GETADDR || (hdr.nlmsg_flags & NLM_F_DUMP) != 0 {
                // 独占锁定网卡总管，捞出 smoltcp 内部维护的真实 IP 列表
                let iface = crate::net::NET_IFACE.exclusive_access();
                let ip_addrs = iface.ip_addrs();

                for cidr in ip_addrs.iter() {
                    let smoltcp::wire::IpCidr::Ipv4(v4_cidr) = cidr;
                    let addr = v4_cidr.address(); 
                    let ip_bytes = addr.as_bytes(); 
                    let prefix_len = v4_cidr.prefix_len();       // 掩码，如 24

                    // 动态计算报文长度：NlMsgHdr(16) + IfAddressMsg(8) + RtAttr(4) + IP数据(4) = 32 字节
                    let mut addr_reply = vec![0u8; 32];
                    
                    let reply_hdr = NlMsgHdr {
                        nlmsg_len: 32,
                        nlmsg_type: RTM_NEWADDR, // 回应 RTM_NEWADDR 报文
                        nlmsg_flags: NLM_F_MULTI,
                        nlmsg_seq: hdr.nlmsg_seq,
                        nlmsg_pid: hdr.nlmsg_pid,
                    };

                    let ifa = IfAddressMsg {
                        ifa_family: 2, // AF_INET (IPv4)
                        ifa_prefixlen: prefix_len as u8,
                        ifa_flags: 0,
                        ifa_scope: 0,   // RT_SCOPE_UNIVERSE
                        ifa_index: 1,   // 网卡 index 绑定为 1
                    };

                    let rta = RtAttr {
                        rta_len: 8,       // RtAttr头(4字节) + Value(4字节) = 8
                        rta_type: IFA_LOCAL, // 2: 本地操作地址
                    };

                    // 严格按字节无缝拷贝拼接成标准的 Linux 报文流
                    unsafe {
                        let ptr = addr_reply.as_mut_ptr();
                        core::ptr::copy_nonoverlapping(&reply_hdr as *const NlMsgHdr as *const u8, ptr, size_of::<NlMsgHdr>());
                        core::ptr::copy_nonoverlapping(&ifa as *const IfAddressMsg as *const u8, ptr.add(16), size_of::<IfAddressMsg>());
                        core::ptr::copy_nonoverlapping(&rta as *const RtAttr as *const u8, ptr.add(24), size_of::<RtAttr>());
                    }
                    addr_reply[28..32].copy_from_slice(ip_bytes); // 塞入底层的真实 IP 字节

                    rx_lock.push_back(addr_reply);
                    
                }
            }

        
            let mut done_reply = vec![0u8; 20];
            let done_hdr = NlMsgHdr {
                nlmsg_len: 20,
                nlmsg_type: NLMSG_DONE, // 3: NLMSG_DONE 代表批量导出结束
                nlmsg_flags: NLM_F_MULTI,
                nlmsg_seq: hdr.nlmsg_seq,
                nlmsg_pid: hdr.nlmsg_pid,
            };
            unsafe { core::ptr::copy_nonoverlapping(&done_hdr as *const NlMsgHdr as *const u8, done_reply.as_mut_ptr(), size_of::<NlMsgHdr>()); }
            rx_lock.push_back(done_reply);

            return total_len;
        }


        // 用户态发起普通的 RTM_NEWADDR (添加新 IP 命令)

        if hdr.nlmsg_type == RTM_NEWADDR {
            let ifa_offset = size_of::<NlMsgHdr>();
            if ifa_offset + size_of::<IfAddressMsg>() <= data.len() {
                let ifa = unsafe { &*(data.as_ptr().add(ifa_offset) as *const IfAddressMsg) };
                let attr_offset = ifa_offset + size_of::<IfAddressMsg>();
                let payload_end = hdr.nlmsg_len as usize;
                
                if attr_offset < payload_end && payload_end <= data.len() {
                    let attr_payload = &data[attr_offset..payload_end];
                    self.parse_rt_attributes(attr_payload, ifa.ifa_prefixlen);
                }
            }
        }

        // 正常的操作返回标准的成功 ACK 回执
        let ack_hdr = NlMsgHdr {
            nlmsg_len: (size_of::<NlMsgHdr>() + size_of::<NlMsgErr>()) as u32,
            nlmsg_type: NLMSG_ERROR,
            nlmsg_flags: 0,
            nlmsg_seq: hdr.nlmsg_seq,
            nlmsg_pid: hdr.nlmsg_pid,
        };
        let ack_err = NlMsgErr { error: 0, msg: *hdr };

        let mut ack_packet = vec![0u8; size_of::<NlMsgHdr>() + size_of::<NlMsgErr>()];
        unsafe {
            core::ptr::copy_nonoverlapping(&ack_hdr as *const NlMsgHdr as *const u8, ack_packet.as_mut_ptr(), size_of::<NlMsgHdr>());
            core::ptr::copy_nonoverlapping(&ack_err as *const NlMsgErr as *const u8, ack_packet.as_mut_ptr().add(size_of::<NlMsgHdr>()), size_of::<NlMsgErr>());
        }

        self.rx_buffer.lock().push_back(ack_packet);
        total_len
    }

    fn read(&self, mut buf: UserBuffer) -> usize {
        if let Some(packet) = self.rx_buffer.lock().pop_front() {
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
        0
    }

    fn get_stat(&self) -> Stat {
        // 保持原样，标记为普通 Socket 文件类型
        Stat {
            dev: 0, ino: 0, mode: 0o140000 | 0o666, nlink: 1, uid: 0, gid: 0, rdev: 0,
            __pad: 0, size: 0, blksize: 0, __pad2: 0, blocks: 0,
            atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    
    fn get_perm(&self) -> crate::auth::PermStat {
        let stat = self.get_stat();
        //从 get_stat 的 mode 中安全截取并转换为 FileMode 权限格式
        let mode = crate::auth::FileMode::from_bits_truncate(stat.mode as u16);
        crate::auth::PermStat { 
            mode, 
            uid: stat.uid, 
            gid: stat.gid 
        }
    }
    
    fn getdents(&self, _buf: &mut [u8]) -> isize {
        // Socket 不是目录，返回 -1 代表无法进行目录项遍历
        -1
    }
    fn as_any(&self) -> &dyn core::any::Any { self }
}