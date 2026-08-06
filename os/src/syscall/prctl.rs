#![allow(unused)]
use crate::{mm::try_translated_str, process};
use crate::mm::try_translated_write;
use super::process::*;
use super::errno::Errno::*;

// 进程名
const PR_SETNAME: usize = 15;
const PR_GETNAME: usize = 16;
// seccomp
const PR_GET_SECCOMP: usize = 21;
const PR_SET_SECCOMP: usize = 22;
// capability
const PR_CAPBSET_READ: usize = 23;
// 时间戳计数器
const PR_GET_TSC: usize = 25;
const PR_SET_TSC: usize = 26;
// 定时器松弛度
const PR_SET_TIMERSLACK: usize = 29;
const PR_GET_TIMERSLACK: usize = 30;
// 子进程收割者
const PR_SET_CHILD_SUBREAPER: usize = 36;
const PR_GET_CHILD_SUBREAPER: usize = 37;
// 禁止提权
const PR_SET_NO_NEW_PRIVS: usize =38;
const PR_GET_NO_NEW_PRIVS: usize = 39;
// 大页禁用标志
const PR_SET_THP_DISABLE: usize = 41;
const PR_GET_THP_DISABLE: usize = 42;
// capability ambient
const PR_CAP_AMBIENT: usize = 47;
// 分支预测控制
const PR_GET_SPECULATION_CTRL: usize = 52;
const PR_SET_SPECULATION_CTRL: usize = 53;

const PR_CAP_AMBIENT_LOWER: usize = 1;
const PR_CAP_AMBIENT_IS_SET: usize = 2;
const PR_CAP_AMBIENT_CLEAR_ALL: usize = 3;

const PR_TSC_ENABLE: usize = 1;
const PR_TSC_SIGSEGV: usize = 2;

const EINVAL: isize = 22;

pub fn sys_prctl(option: usize, _arg2: usize, _arg3: usize, _arg4: usize, _arg5: usize) -> isize {
    // todo：实现真正的多用户，权限机制和多线程
    trace!("kernel:pid[{}] sys_prctl option={}", current_task().unwrap().getpid(), option);
    match option {
        PR_SETNAME => {
            // 将名称写入当前线程的 comm 字段。
            let name = if let Some(name) = try_translated_str(current_user_token(), _arg2 as *const u8) {
                name
            } else {
                return -EINVAL;
            };
            let task = current_task().unwrap();
            let mut inner = task.inner_exclusive_access();
            inner.comm = [0; 10];
            let name_bytes = name.as_bytes();
            let copy_len = name_bytes.len().min(inner.comm.len() - 1);
            inner.comm[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            0
        },
        PR_GETNAME => {
            // 与set相反
            let task = current_task().unwrap();
            let (name, token) = {
                let inner = task.inner_exclusive_access();
                (inner.comm, inner.get_user_token())
            };
            let mut out = [0u8; 16];
            let copy_len = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
            out[..copy_len].copy_from_slice(&name[..copy_len]);
            if !try_translated_write(token, _arg2 as *mut [u8; 16], out){
                return EFAULT.as_isize();
            };
            0
        },
        PR_GET_SECCOMP => {
            // seccomp state is not implemented.
            Errno::ENOSYS.as_isize()
        }
        PR_SET_SECCOMP => {
            Errno::ENOSYS.as_isize()
        }
        PR_CAPBSET_READ =>{
            Errno::ENOSYS.as_isize()
        }
        PR_GET_TSC =>{
            Errno::ENOSYS.as_isize()
        }
        PR_SET_TSC =>{
            Errno::ENOSYS.as_isize()
        }
        PR_GET_TIMERSLACK => {
            Errno::ENOSYS.as_isize()
        }
        PR_SET_TIMERSLACK => {
            Errno::ENOSYS.as_isize()
        }
        PR_SET_CHILD_SUBREAPER => {
            // 子进程收割者，待后续实现
            -EINVAL
        }
        PR_GET_CHILD_SUBREAPER => {
            -EINVAL
        }
        PR_SET_NO_NEW_PRIVS => {
            Errno::ENOSYS.as_isize()
        }
        PR_GET_NO_NEW_PRIVS => {
            Errno::ENOSYS.as_isize()
        }
        PR_SET_THP_DISABLE => {
            Errno::ENOSYS.as_isize()
        }
        PR_GET_THP_DISABLE => {
            Errno::ENOSYS.as_isize()
        }
        PR_CAP_AMBIENT => {
            // 目前不支持cap
            -EINVAL
        }
        PR_GET_SPECULATION_CTRL => {
            Errno::ENOSYS.as_isize()
        }
        PR_SET_SPECULATION_CTRL => {
            -EINVAL
        }
        _ => -EINVAL,
    }
}

#[allow(unused)]
pub fn sys_arch_prctl(option: usize, addr: usize) -> isize {
    // 摘自linux手册：仅支持 Linux/x86-64 的 64 位程序
    // 不实现
    Errno::ENOSYS.as_isize()
}
