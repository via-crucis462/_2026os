#![allow(unused)]
use super::process::*;

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
    trace!("kernel:pid[{}] sys_prctl option={}", current_task().unwrap().pid.0, option);
    match option {
        PR_SETNAME => {
            // 将buff的内容写进pname字段
            let buff = translated_byte_buffer(current_user_token(), _arg2 as *const u8, 16);
            let mut name_bytes = String::new();
            let mut flag = false;
            for buf in buff.iter() {
                for &b in buf.iter() {
                    if b == 0 {
                        flag = true;
                        break;
                    }
                    name_bytes.push(b as char);
                }
                if flag {
                    break;
                }
            }
            let task = current_task().unwrap();
            let mut inner = task.inner_exclusive_access();
            inner.pname = name_bytes;
            0
        },
        PR_GETNAME => {
            // 与set相反
            let mut buff = translated_byte_buffer(current_user_token(), _arg2 as *const u8, 16);
            let task = current_task().unwrap();
            let inner = task.inner_exclusive_access();
            let name = inner.pname.as_bytes();
            let len = inner.pname.len().min(15);
            let mut i = 0;
            for buf in buff.iter_mut() {
                for b in buf.iter_mut() {
                    if i >= len {
                        *b = 0;
                    } else {
                        *b = name[i];
                        i += 1;
                    }
                }
            }
            0
        },
        PR_GET_SECCOMP => {
            // 尚未实现secomp，允许所有系统调用
            0
        }
        PR_SET_SECCOMP => {
            // 不支持设置secomp
            -EINVAL
        }
        PR_CAPBSET_READ =>{
            // 目前不支持cap，返回1表示所有能力都可用
            1
        }
        PR_GET_TSC =>{
            // 允许读取TSC
            1
        }
        PR_SET_TSC =>{
            // 默认允许所以支持启用
            if _arg2 == PR_TSC_ENABLE {
                0
            } else {
                -EINVAL
            }
        }
        PR_GET_TIMERSLACK => {
            // 伪实现，返回默认值50ms
            50000
        }
        PR_SET_TIMERSLACK => {
            // 伪实现，假装设置成功
            0
        }
        PR_SET_CHILD_SUBREAPER => {
            // 子进程收割者，待后续实现
            -EINVAL
        }
        PR_GET_CHILD_SUBREAPER => {
            -EINVAL
        }
        PR_SET_NO_NEW_PRIVS => {
            // 伪实现，假装设置成功但不实际执行任何操作
            if _arg2 == 1 {
                0
            } else {
                -EINVAL
            }
        }
        PR_GET_NO_NEW_PRIVS => {
            // 默认允许提权
            0
        }
        PR_SET_THP_DISABLE => {
            // 目前的内核始终使用4K标准页大小，即默认禁用大页
            if _arg2 == 1 {
                0
            } else {
                -EINVAL
            }
        }
        PR_GET_THP_DISABLE => {
            // 1:已启用
            1
        }
        PR_CAP_AMBIENT => {
            // 目前不支持cap
            -EINVAL
        }
        PR_GET_SPECULATION_CTRL => {
            // 分支预测控制依赖具体cpu
            // 伪实现，返回默认值0表示不受限制
            0
        }
        PR_SET_SPECULATION_CTRL => {
            -EINVAL
        }
        _ => -EINVAL,
    }
}