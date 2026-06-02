use super::IpcPerm;
use crate::{
    process::RecycleAllocator,
    syscall::errno::Errno::*,
};

use alloc::collections::vec_deque::VecDeque;
use alloc::sync::{Arc};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::mutex::Mutex;
use lazy_static::lazy_static;
use bitflags::bitflags;

// 最大消息队列数量
pub const MSG_Q_MAX: usize = 4096;

bitflags! {
    pub struct MsgFlags: usize {
        /// 非阻塞（msgsnd / msgrcv）
        const IPC_NOWAIT  = 0o04000;
        /// 消息过长时截断而不报错（仅 msgrcv）
        const MSG_NOERROR = 0o10000;
        /// 接收类型不等于 msgtyp 的首条消息（仅 msgrcv，Linux）
        const MSG_EXCEPT  = 0o20000;
        /// 拷贝消息而不消费（仅 msgrcv，Linux 专用）
        const MSG_COPY    = 0o40000;
    }
}

lazy_static! {
    /// 全局消息队列管理器
    pub static ref MSG_MANAGER: Mutex<MsgManager> = Mutex::new(MsgManager::new());
}

pub struct MsgManager {
    id_allocator: RecycleAllocator,
    // id->MsgQueue
    queues: BTreeMap<u32, Arc<Mutex<MsgQueue>>>,
}

impl MsgManager {
    pub fn new() -> Self {
        Self {
            id_allocator: RecycleAllocator::new(),
            queues: BTreeMap::new(),
        }
    }
    pub fn create_queue(&mut self) -> isize {
        let id = self.id_allocator.alloc();
        // 溢出检查
        if id > MSG_Q_MAX {
            return ENOMEM.as_isize();
        }
        let queue = Arc::new(Mutex::new(MsgQueue::new()));
        self.queues.insert(id as u32, queue.clone());
        id as isize
    }
    /// 根据id查询队列，获得其arc克隆
    pub fn get_queue(&self, id: u32) -> Option<Arc<Mutex<MsgQueue>>> {
        self.queues.get(&id).cloned()
    }
    /// 按id移除队列
    pub fn remove_queue(&mut self, id: u32) {
        self.queues.remove(&id);
        self.id_allocator.dealloc(id as usize);
    }
}

/// System V 消息队列描述符，对应 Linux 的 struct msqid_ds
#[derive(Debug, Clone)]
#[repr(C)]
pub struct MsqidDs {
    pub msg_perm: IpcPerm,     // 所有权和权限信息
    pub msg_stime: usize,      // 上次执行 msgsnd() 的时间
    pub msg_rtime: usize,      // 上次执行 msgrcv() 的时间
    pub msg_ctime: usize,      // 最后一次改变的时间（msgctl修改或创建）
    pub msg_cbytes: usize,     // 当前队列中所有消息的字节数总和
    pub msg_qnum: usize,       // 当前队列中的消息总数
    pub msg_qbytes: usize,     // 队列允许的最大字节数
    pub msg_lspid: usize,      // 最后一个调用 msgsnd() 的进程 PID
    pub msg_lrpid: usize,      // 最后一个调用 msgrcv() 的进程 PID
}

impl Default for MsqidDs {
    fn default() -> Self {
        Self {
            msg_perm: IpcPerm::default(),
            msg_stime: 0,
            msg_rtime: 0,
            msg_ctime: 0,
            msg_cbytes: 0,
            msg_qnum: 0,
            msg_qbytes: 16384, // 默认最大 16KB，Linux 常见默认值 MSGMNB
            msg_lspid: 0,
            msg_lrpid: 0,
        }
    }
}

pub struct MsgQueue {
    msqds: MsqidDs,
    msgs: VecDeque<Msg>,
}

impl MsgQueue {
    pub fn new() -> Self {
        Self {
            msqds: MsqidDs::default(),
            msgs: VecDeque::new(),
        }
    }

    /// 更新信息
    fn update_msqds(&mut self, is_send: bool, pid: usize, size: usize) {
        let time_now = crate::get_real_time_ns();
        self.msqds.msg_qnum = self.msgs.len();
        self.msqds.msg_cbytes = self.msgs.iter().map(|msg| msg.mtext.len()).sum();
        if is_send {
            self.msqds.msg_stime = time_now as usize;
            self.msqds.msg_lspid = pid;
            self.msqds.msg_cbytes += size as usize;
         } else {
            self.msqds.msg_rtime = time_now as usize;
            self.msqds.msg_lrpid = pid;
            self.msqds.msg_cbytes -= size as usize;
        }
    }

    /// 将消息追加到队列
    pub fn add(&mut self, msg: Msg, pid: usize) -> Result<(), isize> {
        let size = msg.get_size();
        self.msgs.push_back(msg);
        self.update_msqds(true, pid, size);
        Ok(())
    }

    pub fn pop(&mut self, pid: usize) -> Option<Msg> {
        let msg = self.msgs.pop_front();
        if msg.is_some() {
            self.update_msqds(false, pid, 0);
        }
        msg
    }

    pub fn len(&self) -> usize {
        self.msgs.len()
    }

    /// 从队列中取出一个类型匹配的消息，若消息过长且未设置 MSG_NOERROR 则返回 E2BIG
    pub fn get(&mut self, msgtyp: isize, msgsz: usize, msgflg: MsgFlags, pid: usize) -> Result<Msg, isize> {
        let pos = self.msgs.iter().position(|msg| {
            msg.mtype == msgtyp as usize
        });
        if let Some(idx) = pos {
            let msg = &self.msgs[idx];
            // 消息过长且未设置 MSG_NOERROR，保留消息并返回 E2BIG
            if msg.mtext.len() > msgsz && !msgflg.contains(MsgFlags::MSG_NOERROR) {
                return Err(E2BIG.as_isize());
            }
            let msg = self.msgs.remove(idx).unwrap();
            self.update_msqds(false, pid, msg.mtext.len());
            Ok(msg)
        } else {
            Err(ENOMSG.as_isize())
        }
    }
}

#[derive(Debug, Clone)]
pub struct Msg {
    // TODO
    pub mtype: usize,
    pub mtext: Vec<u8>,
}

impl Msg {
    pub fn new(mtype: usize, mtext: Vec<u8>) -> Self {
        Self {
            mtype,
            mtext
        }
    }
    pub fn get_size(&self) -> usize {
        self.mtext.len()
    }
}