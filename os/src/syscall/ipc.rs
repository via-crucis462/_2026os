//! IPC相关系统调用实现
//! 目前仅实现了System V消息队列和共享内存
//! 
use crate::{
    auth::{FileMode, PermSet, PermStat},
    ipc::{msg::*, shm::*, namespace::*, IpcPerm},
    mm::{UserBuffer, try_translated_byte_buffer, try_translated_byte_buffer_mut, try_translated_read, try_translated_write},
    process::current_user_token,
};
use super::*;
use Errno::*;
use bitflags::bitflags;
use alloc::vec;


/// 调用者是否为队列所有者或root
fn ipc_owner_check(perm: &IpcPerm, uid: u32) -> bool {
    uid == 0 || uid == perm.uid || uid == perm.cuid
}

/// 写入权检查
fn ipc_write_check(perm: &IpcPerm, uid: u32, gid: u32) -> bool {
    PermStat::new(FileMode::from_bits_truncate(perm.mode), perm.uid, perm.gid)
        .can_write(uid, gid)
}

/// 读取权检查
fn ipc_read_check(perm: &IpcPerm, uid: u32, gid: u32) -> bool {
    PermStat::new(FileMode::from_bits_truncate(perm.mode), perm.uid, perm.gid)
        .can_read(uid, gid)
}


//
// msg相关
//

/// msgget 标志解析
fn msgget_flags_from(msgflg: usize) -> (MsgGetFlags, u16) {
    let mode = (msgflg & 0o777) as u16;
    let flags = MsgGetFlags::from_bits_truncate(msgflg);
    (flags, mode)
}

/// 查询消息队列id，根据flg决定是否新建
pub fn sys_msgget(key: u32, msgflg: usize) -> isize {
    let (flags, mode) = msgget_flags_from(msgflg);
    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.euid, inner.egid)
    };
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    let msg_man = ns_lckd.msg_manager();
    let result = msg_man.msgget(key, flags, mode, uid, gid);
    // 找到队列，权限检查
    if result > 0 {
        let need_rd = flags.contains(MsgGetFlags::MSG_RD);
        let need_wr = flags.contains(MsgGetFlags::MSG_WR);
        if need_rd || need_wr {
            if let Some(queue) = msg_man.get_queue(result as u32) {
                let q = queue.lock();
                let perm = &q.get_msqid_ds().msg_perm;
                if need_rd && !ipc_read_check(perm, uid, gid) {
                    return EACCES.as_isize();
                }
                if need_wr && !ipc_write_check(perm, uid, gid) {
                    return EACCES.as_isize();
                }
            }
        }
    }
    result
}

/// 向指定id的消息队列发送消息
/// msgp：用户空间地址，msgsz：大小，msgflg：标志
pub fn sys_msgsnd(msqid: usize, msgp: usize, msgsz: usize, msgflg: usize) -> isize {
    let pid = current_task().unwrap().process().pid.0;
    let token = current_user_token();
    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.euid, inner.egid)
    };
    let _flags = MsgFlags::from_bits_truncate(msgflg);

    // 参数合法性检查
    if msgsz as isize > 0x7FFFFFFF || (msgsz as isize) < 0 {
        return EINVAL.as_isize();
    }

    // 读取消息类型
    let mtype = if let Some(m) = try_translated_read(token, msgp as *const isize) {
        m
    } else {
        return EFAULT.as_isize();
    };
    // mtype 必须 > 0
    if mtype <= 0 {
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
    let msg = Msg::new(mtype as usize, text);

    // 存入队列
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    let msg_man = ns_lckd.msg_manager();
    if let Some(queue) = msg_man.get_queue(msqid as u32) {
        let mut q = queue.lock();
        // 权限检查
        if !ipc_write_check(&q.get_msqid_ds().msg_perm, uid, gid) {
            return EACCES.as_isize();
        }
        q.add(msg, pid).map(|_| 0).unwrap_or_else(|e| e)
    } else {
        EINVAL.as_isize()
    }
}

/// 从指定id的消息队列接收消息
/// msgp：消息指针，msgsz：消息大小，msgtyp：类型，msgflg：标志
pub fn sys_msgrcv(msqid: usize, msgp: usize, msgsz: usize, msgtyp: isize, msgflg: usize) -> isize {
    let pid = current_task().unwrap().process().pid.0;
    let token = current_user_token();
    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.euid, inner.egid)
    };
    let flags = MsgFlags::from_bits_truncate(msgflg);
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    let msg_man = ns_lckd.msg_manager();
    if let Some(queue) = msg_man.get_queue(msqid as u32) {
        let mut q = queue.lock();
        // 权限检查：需要读权限
        if !ipc_read_check(&q.get_msqid_ds().msg_perm, uid, gid) {
            return EACCES.as_isize();
        }
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
    let (uid, gid) = {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        (inner.euid, inner.egid)
    };

    match cmd {
        // 获取状态信息
        IPC_STAT => {
            let ns = current_ipc_namespace();
            let mut ns_lckd = ns.lock();
            let msg_man = ns_lckd.msg_manager();
            if let Some(queue) = msg_man.get_queue(msqid) {
                let q = queue.lock();
                // 权限检查
                if !ipc_read_check(&q.get_msqid_ds().msg_perm, uid, gid) {
                    return EACCES.as_isize();
                }
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
            let ns = current_ipc_namespace();
            let mut ns_lckd = ns.lock();
            let msg_man = ns_lckd.msg_manager();
            if let Some(queue) = msg_man.get_queue(msqid) {
                let mut q = queue.lock();
                // 权限检查
                // 写或所有者
                if !(ipc_write_check(&q.get_msqid_ds().msg_perm, uid, gid)
                    || ipc_owner_check(&q.get_msqid_ds().msg_perm, uid))
                {
                    return EACCES.as_isize();
                }
                q.apply_ipc_set(&ds);
                0
            } else {
                EINVAL.as_isize()
            }
        }
        // 删除消息队列
        IPC_RMID => {
            let ns = current_ipc_namespace();
            let mut ns_lckd = ns.lock();
            let msg_man = ns_lckd.msg_manager();
            if let Some(queue) = msg_man.get_queue(msqid) {
                let q = queue.lock();
                let has_perm = ipc_owner_check(&q.get_msqid_ds().msg_perm, uid);
                drop(q);
                if !has_perm {
                    return EPERM.as_isize();
                }
                msg_man.remove_queue(msqid);
                0
            } else {
                EINVAL.as_isize()
            }
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
        (inner.euid, inner.egid)
    };

    // IPC_PRIVATE 始终创建新段
    if key == 0 {
        if !ipc_flags.contains(ShmFlags::IPC_CREAT) {
            return EINVAL.as_isize();
        }
        let cpid = current_task().unwrap().process().pid.0;
        let ns = current_ipc_namespace();
        let mut ns_lckd = ns.lock();
        let shm = ns_lckd.shm_manager().create_shm(size, key, mode, cpid);
        return shm.get_id() as isize
    }

    // 尝试找到段
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    if let Some(shm) = ns_lckd.shm_manager().get_shm_by_key(key) {
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
    let shm = ns_lckd.shm_manager().create_shm(size, key, mode, cpid);
    shm.get_id() as isize
}

pub fn sys_shmctl(shmid: u32, cmd: i32, flags: i32) -> isize {
    let cmd = ShmCtlCmd::from_bits_truncate(cmd);
    ENOSYS.as_isize()
}