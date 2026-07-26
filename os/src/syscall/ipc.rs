//! IPC相关系统调用实现
//! 目前仅实现了System V消息队列和共享内存
//! 
use crate::{
    auth::{FileMode, PermSet, PermStat},
    ipc::{msg::*, shm::*, namespace::*, IpcPerm},
    mm::{UserBuffer, mmap, try_translated_byte_buffer, try_translated_byte_buffer_mut, try_translated_read, try_translated_write},
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
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
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
    let task = current_task().unwrap();
    let pid = task.getpid();
    let token = current_user_token();
    let (uid, gid) = {
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
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
    let task = current_task().unwrap();
    let pid = task.getpid();
    let token = current_user_token();
    let (uid, gid) = {
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
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
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
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
}

const SHM_SIZE_LIMIT: usize = 16 * 1024 * 1024; // 16MB


// shm syscalls参考https://www.cnblogs.com/52php/p/5861372.html

pub fn sys_shmget(key: i32, size: usize, flags: i32) -> isize {
    // 获取低9位权限
    let mode = (flags & 0o777) as u16;
    let ipc_flags = ShmFlags::from_bits_truncate(flags & !0o777);

    let (uid, gid) = {
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
    };

    // IPC_PRIVATE 始终创建新段
    if key == 0 {
        if !ipc_flags.contains(ShmFlags::IPC_CREAT) {
            return EINVAL.as_isize();
        }
        let cpid = current_task().unwrap().getpid();
        let ns = current_ipc_namespace();
        let mut ns_lckd = ns.lock();
        let shm = ns_lckd.shm_manager().create_shm(size, key, mode, cpid, uid, gid);
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
    let cpid = current_task().unwrap().getpid();
    let shm = ns_lckd.shm_manager().create_shm(size, key, mode, cpid, uid, gid);
    shm.get_id() as isize
}

pub fn sys_shmctl(shmid: u32, cmd: usize, buf: usize) -> isize {
    use crate::ipc::shm::ShmidDs;
    let token = current_user_token();
    let (uid, gid) = {
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
    };
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();

    let shm = match ns_lckd.shm_manager().get_shm(shmid) {
        Some(s) => s,
        None => return EINVAL.as_isize(),
    };

    match cmd {
        IPC_STAT => {
            if !ipc_read_check(&shm.get_perm(), uid, gid) {
                return EACCES.as_isize();
            }
            let ds = shm.get_stat();
            if !try_translated_write(token, buf as *mut ShmidDs, ds) {
                return EFAULT.as_isize();
            }
            0
        }
        IPC_SET => {
            let ds: ShmidDs = match try_translated_read(token, buf as *const ShmidDs) {
                Some(d) => d,
                None => return EFAULT.as_isize(),
            };
            if !ipc_owner_check(&shm.get_perm(), uid)
                && !ipc_write_check(&shm.get_perm(), uid, gid)
            {
                return EACCES.as_isize();
            }
            shm.set_perm_fields(ds.shm_perm.uid, ds.shm_perm.gid, ds.shm_perm.mode);
            shm.set_lpid(current_task().unwrap().getpid());
            0
        }
        IPC_RMID => {
            if !ipc_owner_check(&shm.get_perm(), uid) {
                return EPERM.as_isize();
            }
            ns_lckd.shm_manager().remove_shm(shmid);
            0
        }
        _ => EINVAL.as_isize(),
    }
}

/// 分离共享内存段
pub fn sys_shmdt(shmaddr: usize) -> isize {
    let task = current_task().unwrap();
    let pid = task.getpid();
    info!("kernel:pid[{}] sys_shmdt: addr={:#x}", pid, shmaddr);

    if shmaddr == 0 || shmaddr % crate::PAGE_SIZE != 0 {
        return EINVAL.as_isize();
    }

    // 查找该地址对应的 shmid
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    let shmid = match ns_lckd.shm_manager().take_attach(shmaddr) {
        Some(id) => id,
        None => return EINVAL.as_isize(),
    };

    // 减计数
    if let Some(shm) = ns_lckd.shm_manager().get_shm(shmid) {
        shm.dec_nattch();
    }

    // 释放锁后再 unmap（避免死锁）
    drop(ns_lckd);

    match mmap::do_munmap(shmaddr, crate::PAGE_SIZE) {
        Ok(()) => 0,
        Err(_) => EINVAL.as_isize(),
    }
}

/// 附加共享内存段
///
/// 将 shmid 标识的共享内存段附加到调用进程的地址空间
/// 参数:
///   shmid - 共享内存标识符
///   shmaddr - 建议的附加地址（NULL 表示内核决定）
///   shmflg - 标志位
/// 
/// 成功返回附加地址
pub fn sys_shmat(shmid: usize, shmaddr: usize, shmflg: i32) -> isize {
    const SHM_RDONLY: i32 = 0o10000;
    const SHM_RND: i32 = 0o20000;

    info!("kernel:pid[{}] sys_shmat: shmid={}, addr={:#x}, flg={:#o}",
        current_task().unwrap().getpid(), shmid, shmaddr, shmflg);

    // 获取共享内存段
    let ns = current_ipc_namespace();
    let mut ns_lckd = ns.lock();
    let shm = match ns_lckd.shm_manager().get_shm(shmid as u32) {
        Some(s) => s,
        None => return EINVAL.as_isize(),
    };

    let shm_size = shm.get_size();
    let shm_perm = shm.get_perm();

    // 权限检查
    let (uid, gid) = {
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        let cred = inner.cred.exclusive_access();
        (cred.euid(), cred.egid())
    };
    let is_readonly = (shmflg & SHM_RDONLY) != 0;
    let perm_stat = PermStat::new(
        FileMode::from_bits_truncate(shm_perm.mode),
        shm_perm.uid,
        shm_perm.gid,
    );
    perm_stat.current_perm_set();
    if !perm_stat.can_read(uid, gid) {
        return EACCES.as_isize();
    }
    if !is_readonly && !perm_stat.can_write(uid, gid) {
        return EACCES.as_isize();
    }

    // 确定附加地址
    let attach_addr = if shmaddr == 0 {
        // 由内核选择地址，通过 do_mmap 自动分配
        0
    } else if (shmflg & SHM_RND) != 0 {
        // SHM_RND: 向下对齐到 SHMLBA（通常等于 PAGE_SIZE）
        shmaddr & !(crate::PAGE_SIZE - 1)
    } else {
        if shmaddr % crate::PAGE_SIZE != 0 {
            return EINVAL.as_isize();
        }
        shmaddr
    };

    // 使用 mmap 分配虚拟地址空间并映射
    let prot = if is_readonly {
        mmap::MMapProt::PROT_READ
    } else {
        mmap::MMapProt::PROT_READ | mmap::MMapProt::PROT_WRITE
    };
    let mmap_flags = mmap::MMapFlags::MAP_SHARED;

    let tmpfs = shm.inner();

    let mapped_addr = match mmap::do_mmap(attach_addr, shm_size, prot, mmap_flags, Some(tmpfs), 0) {
        Ok(addr) => addr,
        Err(e) => return e,
    };

    // 记录映射关系，供 shmdt 查找
    ns_lckd.shm_manager().record_attach(shmid as u32, mapped_addr);
    // 更新附加计数
    shm.inc_nattch();
    shm.set_lpid(current_task().unwrap().getpid());

    mapped_addr as isize
}