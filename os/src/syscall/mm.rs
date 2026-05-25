//! 内存管理相关syscall，暂未完全迁移

use super::*;
use Errno::*;
use riscv::register::satp::Mode;
use crate::{auth::{PermSet, PermStat, FileMode}, mm::shm::*};
use bitflags::bitflags;

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