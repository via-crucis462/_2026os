//! IPC相关系统调用实现
//! 目前仅实现了System V消息队列和共享内存
//! 
use crate::{
    auth::{FileMode, PermSet, PermStat},
    ipc::{msg::*, shm::*},
    mm::{UserBuffer, try_translated_byte_buffer, try_translated_byte_buffer_mut, try_translated_read, try_translated_write},
    process::current_user_token
};
use super::*;
use Errno::*;
use bitflags::bitflags;
use alloc::vec;


//
// msg相关
//

/// msgget 标志解析
fn msgget_flags_from(msgflg: usize) -> (MsgGetFlags, u16) {
    let mode = (msgflg & 0o777) as u16;
    let flags = MsgGetFlags::from_bits_truncate(msgflg & !0o777);
    (flags, mode)
}

/// 查询消息队列id，根据flg决定是否新建
pub fn sys_msgget(key: u32, msgflg: usize) -> isize {
    let (flags, mode) = msgget_flags_from(msgflg);
    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.uid, inner.gid)
    };
    MSG_MANAGER.lock().msgget(key, flags, mode, uid, gid)
}

/// 向指定id的消息队列发送消息
/// msgp：用户空间地址，msgsz：大小，msgflg：标志
pub fn sys_msgsnd(msqid: usize, msgp: usize, msgsz: usize, msgflg: usize) -> isize {
    let pid = current_task().unwrap().process().pid.0;
    let token = current_user_token();
    let flags = MsgFlags::from_bits_truncate(msgflg);
    
    // 读取消息类型
    let mut mtype = if let Some(m) = try_translated_read(token, msgp as *const usize) {
        m
    } else {
        return EFAULT.as_isize();
    };
    if mtype == 0 {
        return EINVAL.as_isize();
    }

    // 读取消息内容
    let msgtptr = msgp + core::mem::size_of::<usize>();
    let msg_buf = if msgsz > 0 {
        if let Some(text) = try_translated_byte_buffer(token, msgtptr as *const u8, msgsz) {
            UserBuffer::new(text)
        } else {
            return EFAULT.as_isize();
        }
    } else {
        UserBuffer::new(Vec::new())
    };
    let mut text = vec![0u8; msg_buf.len()];
    msg_buf.read(&mut text);

    // 构造消息结构体
    let msg = Msg::new(mtype, text);

    // 存入队列
    let manager = MSG_MANAGER.lock();
    if let Some(queue) = manager.get_queue(msqid as u32) {
        let mut q = queue.lock();
        // 此处暂时省略 msg_qbytes 上限检查及 IPC_NOWAIT 阻塞逻辑
        q.add(msg, pid);
        0
    } else {
        EINVAL.as_isize()
    }
}

/// 从指定id的消息队列接收消息
/// msgp：消息指针，msgsz：消息大小，msgtyp：类型，msgflg：标志
pub fn sys_msgrcv(msqid: usize, msgp: usize, msgsz: usize, msgtyp: isize, msgflg: usize) -> isize {
    let pid = current_task().unwrap().process().pid.0;
    let token = current_user_token();
    let flags = MsgFlags::from_bits_truncate(msgflg);
    let manager = MSG_MANAGER.lock();
    if let Some(queue) = manager.get_queue(msqid as u32) {
        let mut q = queue.lock();
        match q.get(msgtyp, msgsz, flags, pid) {
            Ok(msg) => {
                let actual_len = msg.mtext.len();
                let copy_len = core::cmp::min(msgsz, actual_len);

                // 写入消息类型
                if !try_translated_write(token, msgp as *mut usize, msg.mtype) {
                    return EFAULT.as_isize();
                }

                // 写入消息内容
                if copy_len > 0 {
                    let mtext_buf = if let Some(buf) = try_translated_byte_buffer_mut(
                        token,
                        (msgp + core::mem::size_of::<usize>()) as *mut u8,
                        copy_len,
                    ) {
                        UserBuffer::new(buf)
                    } else {
                        return EFAULT.as_isize();
                    };
                    let mut mtext_buf = mtext_buf;
                    mtext_buf.write(&msg.mtext[..copy_len]);
                }

                copy_len as isize
            }
            Err(e) => e,
        }
    } else {
        EINVAL.as_isize()
    }
}

/// 对消息队列执行控制操作
/// cmd: IPC_STAT / IPC_SET / IPC_RMID
pub fn sys_msgctl(msqid: u32, cmd: usize, buf: usize) -> isize {
    let token = current_user_token();

    match cmd {
        // 获取状态信息
        IPC_STAT => {
            let manager = MSG_MANAGER.lock();
            if let Some(queue) = manager.get_queue(msqid) {
                let q = queue.lock();
                let ds = q.get_msqid_ds();
                if !try_translated_write(token, buf as *mut MsqidDs, ds) {
                    return EFAULT.as_isize();
                }
                0
            } else {
                EINVAL.as_isize()
            }
        }
        // 设置状态信息
        IPC_SET => {
            let ds = if let Some(d) = try_translated_read(token, buf as *const MsqidDs) {
                d
            } else {
                return EFAULT.as_isize();
            };
            let manager = MSG_MANAGER.lock();
            if let Some(queue) = manager.get_queue(msqid) {
                let mut q = queue.lock();
                q.apply_ipc_set(&ds);
                0
            } else {
                EINVAL.as_isize()
            }
        }
        // 删除消息队列
        IPC_RMID => {
            MSG_MANAGER.lock().remove_queue(msqid);
            0
        }
        _ => EINVAL.as_isize(),
    }
}


//
// shm相关
//

bitflags! {
    struct ShmFlags: i32 {
        const IPC_CREAT = 0o1000;
        const IPC_EXCL = 0o2000;
        const IPC_NOWAIT = 0o4000;

        const SHM_RD = 0o400;
        const SHM_WR = 0o200;
    
    }
    struct ShmCtlCmd: i32 {
        const IPC_RMID = 0;
        const IPC_SET = 1;
        const IPC_STAT = 2;
        const IPC_INFO = 3;
    }
}

const SHM_SIZE_LIMIT: usize = 16 * 1024 * 1024; // 16MB


// shm syscalls参考https://www.cnblogs.com/52php/p/5861372.html

pub fn sys_shmget(key: i32, size: usize, flags: i32) -> isize {
    // 获取低9位权限
    let mode = (flags & 0o777) as u16;
    let ipc_flags = ShmFlags::from_bits_truncate(flags & !0o777);

    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.uid, inner.gid)
    };

    // IPC_PRIVATE 始终创建新段
    if key == 0 {
        if !ipc_flags.contains(ShmFlags::IPC_CREAT) {
            return EINVAL.as_isize();
        }
        let cpid = current_task().unwrap().process().pid.0;
        let shm = get_new_shm(size, key, mode, cpid);
        return shm.get_id() as isize
    }

    // 尝试找到段
    if let Some(shm) = get_shm_by_key(key) {
        if ipc_flags.contains(ShmFlags::IPC_EXCL) && ipc_flags.contains(ShmFlags::IPC_CREAT) {
            return EEXIST.as_isize();
        }
        // 权限检查
        let perm = shm.stat.lock().shm_perm;
        let perm_stat = PermStat::new(
            FileMode::from_bits_truncate(perm.mode),
            perm.uid,
            perm.gid,
        );
        perm_stat.current_perm_set();
        if ipc_flags.contains(ShmFlags::SHM_RD) && !perm_stat.can_read(uid, gid) {
            return EACCES.as_isize();
        }
        if ipc_flags.contains(ShmFlags::SHM_WR) && !perm_stat.can_write(uid, gid) {
            return EACCES.as_isize();
        }
        // 成功找到段，返回id
        return shm.get_id() as isize;
    }

    // 不存在，如果指定则创建新段
    if !ipc_flags.contains(ShmFlags::IPC_CREAT) {
        return ENOENT.as_isize();
    }
    if size == 0 || size > SHM_SIZE_LIMIT {
        return EINVAL.as_isize();
    }
    let cpid = current_task().unwrap().process().pid.0;
    let shm = get_new_shm(size, key, mode, cpid);
    shm.get_id() as isize
}

pub fn sys_shmctl(shmid: u32, cmd: i32, flags: i32) -> isize {
    let cmd = ShmCtlCmd::from_bits_truncate(cmd);
    ENOSYS.as_isize()
}