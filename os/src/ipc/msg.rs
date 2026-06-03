//! System V IPC 消息队列 

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

    /// msgget 标志
    pub struct MsgGetFlags: usize {
        const IPC_CREAT = 0o1000;
        const IPC_EXCL  = 0o2000;
        const MSG_RD    = 0o0400;
        const MSG_WR    = 0o0200;
    }

}

pub const IPC_RMID: usize = 0;
pub const IPC_SET: usize = 1;
pub const IPC_STAT: usize = 2;


pub struct MsgManager {
    id_allocator: RecycleAllocator,
    // id->队列
    queues: BTreeMap<u32, Arc<Mutex<MsgQueue>>>,
    // key->id
    key_to_id: BTreeMap<u32, u32>,
}

impl MsgManager {
    pub fn new() -> Self {
        Self {
            id_allocator: RecycleAllocator::new_with_start(1),
            queues: BTreeMap::new(),
            key_to_id: BTreeMap::new(),
        }
    }

    /// 根据 key 和 flags 获取或创建队列
    pub fn msgget(&mut self, key: u32, flags: MsgGetFlags, mode: u16, uid: u32, gid: u32) -> isize {
        // key = 0 直接创建
        if key == 0 {
            return self.create_queue(key, mode, uid, gid);
        }

        // 尝试查找
        if let Some(&id) = self.key_to_id.get(&key) {
            if flags.contains(MsgGetFlags::IPC_CREAT) && flags.contains(MsgGetFlags::IPC_EXCL) {
                return EEXIST.as_isize();
            }
            // 权限检查在上层 syscall 中处理
            return id as isize;
        }

        // 不存在且指定 IPC_CREAT，创建
        if flags.contains(MsgGetFlags::IPC_CREAT) {
            return self.create_queue(key, mode, uid, gid);
        }

        ENOENT.as_isize()
    }

    fn create_queue(&mut self, key: u32, mode: u16, uid: u32, gid: u32) -> isize {
        let id = self.id_allocator.alloc();
        // 溢出检查
        if id > MSG_Q_MAX {
            return ENOMEM.as_isize();
        }
        let mut queue = MsgQueue::new();
        // 设置权限信息
        queue.msqds.msg_perm = IpcPerm {
            key: key as i32,
            uid,
            gid,
            cuid: uid,
            cgid: gid,
            mode,
            seq: 0,
            ..Default::default()
        };
        // 初始化基本信息
        queue.msqds.msg_ctime = (crate::get_real_time_ns() / 1_000_000_000) as usize;
        let qid = id as u32;
        self.key_to_id.insert(key, qid);
        self.queues.insert(qid, Arc::new(Mutex::new(queue)));
        id as isize
    }
    /// 根据id查询队列，获得其arc克隆
    pub fn get_queue(&self, id: u32) -> Option<Arc<Mutex<MsgQueue>>> {
        self.queues.get(&id).cloned()
    }
    /// 按id移除队列
    pub fn remove_queue(&mut self, id: u32) {
        let queues = &mut self.queues;
        if let Some(queue) = queues.get(&id) {
            let q = queue.lock();
            let key = q.msqds.msg_perm.key as u32;
            self.key_to_id.remove(&key);
            self.id_allocator.dealloc(id as usize);
        }
        queues.remove(&id);
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
    pub msg_lspid: i32,        // 最后一个调用 msgsnd() 的进程 PID
    pub msg_lrpid: i32,        // 最后一个调用 msgrcv() 的进程 PID
    pub __unused4: usize,
    pub __unused5: usize,
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
            __unused4: 0,
            __unused5: 0,
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

    /// 更新信息（调用前 msg 必须已完成入队或出队操作）
    fn update_msqds(&mut self, is_send: bool, pid: usize) {
        let time_now = crate::get_real_time_ns() / 1_000_000_000;
        // msg_qnum 和 msg_cbytes 直接由队列内容计算
        self.msqds.msg_qnum = self.msgs.len();
        self.msqds.msg_cbytes = self.msgs.iter().map(|msg| msg.mtext.len()).sum();
        if is_send {
            self.msqds.msg_stime = time_now as usize;
            self.msqds.msg_lspid = pid as i32;
        } else {
            self.msqds.msg_rtime = time_now as usize;
            self.msqds.msg_lrpid = pid as i32;
        }
    }

    /// 将消息追加到队列，超出 msg_qbytes 限制时返回 EAGAIN
    pub fn add(&mut self, msg: Msg, pid: usize) -> Result<(), isize> {
        let size = msg.get_size();
        // 检查队列字节数上限
        if self.msqds.msg_cbytes + size > self.msqds.msg_qbytes {
            return Err(EAGAIN.as_isize());
        }
        self.msgs.push_back(msg);
        self.update_msqds(true, pid);
        Ok(())
    }

    // 从队列头部取出消息
    pub fn pop(&mut self, pid: usize) -> Option<Msg> {
        let msg = self.msgs.pop_front();
        if msg.is_some() {
            self.update_msqds(false, pid);
        }
        msg
    }

    pub fn len(&self) -> usize {
        self.msgs.len()
    }

    /// 获取 msqid_ds 快照
    pub fn get_msqid_ds(&self) -> MsqidDs {
        self.msqds.clone()
    }

    /// IPC_SET: 应用用户设置的 uid, gid, mode, msg_qbytes
    pub fn apply_ipc_set(&mut self, new_ds: &MsqidDs) {
        self.msqds.msg_perm.uid = new_ds.msg_perm.uid;
        self.msqds.msg_perm.gid = new_ds.msg_perm.gid;
        self.msqds.msg_perm.mode = new_ds.msg_perm.mode;
        self.msqds.msg_qbytes = new_ds.msg_qbytes;
        self.msqds.msg_ctime = (crate::get_real_time_ns() / 1_000_000_000) as usize;
    }

    /// 从队列中取出一个类型匹配的消息
    ///
    /// msgtyp 符号含义与 wait4 的 pid 类似:
    /// 0   表示不限制(取队头);
    /// >0  表示取第一条类型等于 msgtyp 的消息;
    /// <0  表示取类型值最小，且类型值小于等于 |msgtyp| 的消息.
    ///
    /// 若消息过长且未设置 MSG_NOERROR 则返回 E2BIG。
    pub fn get(&mut self, msgtyp: isize, msgsz: usize, msgflg: MsgFlags, pid: usize) -> Result<Msg, isize> {
        // MSG_COPY 暂不支持
        if msgflg.contains(MsgFlags::MSG_COPY) {
            return Err(EINVAL.as_isize());
        }

        // 查询
        let idx = self.find_msg(msgtyp, msgflg).ok_or(ENOMSG.as_isize())?;

        // 消息过长且未设置 MSG_NOERROR
        if self.msgs[idx].mtext.len() > msgsz && !msgflg.contains(MsgFlags::MSG_NOERROR) {
            return Err(E2BIG.as_isize());
        }

        // 取出消息
        let msg = if idx == 0 {
            self.msgs.pop_front().unwrap()
        } else {
            self.msgs.remove(idx).unwrap()
        };

        self.update_msqds(false, pid);
        Ok(msg)
    }

    // 查找符合条件的消息的下标
    fn find_msg(&self, msgtyp: isize, msgflg: MsgFlags) -> Option<usize> {
        if self.msgs.is_empty() {
            return None;
        }
        if msgtyp == 0 {
            // 取第一条消息
            Some(0)
        } else if msgtyp > 0 {
            let target = msgtyp as usize;
            if msgflg.contains(MsgFlags::MSG_EXCEPT) {
                // 第一条类型 != target 的消息
                self.msgs.iter().position(|m| m.mtype != target)
            } else {
                // 第一条类型 == target 的消息
                self.msgs.iter().position(|m| m.mtype == target)
            }
        } else {
            // msgtyp < 0
            let limit = (-msgtyp) as usize;
            let mut best: Option<(usize, usize)> = None;
            for (i, msg) in self.msgs.iter().enumerate() {
                if msg.mtype <= limit {
                    match best {
                        None => best = Some((i, msg.mtype)),
                        Some((_, best_type)) if msg.mtype < best_type => best = Some((i, msg.mtype)),
                        _ => {}
                    }
                }
            }
            best.map(|(i, _)| i)
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