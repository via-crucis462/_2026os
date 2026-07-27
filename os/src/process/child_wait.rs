//! 子进程状态等待。
//!
//! 将 wait4/waitid 的条件检查、阻塞入队和子进程退出唤醒放在进程层，
//! 避免把进程生命周期机制散落在系统调用实现中。

use crate::process::scheduler::wait::{
    block_current_and_run_next_if_mp, wake_up_all_mp,
};
use crate::process::{TaskControlBlock, TaskStatus};
use alloc::sync::Arc;

const P_ALL: i32 = 0;
const P_PID: i32 = 1;
const P_PGID: i32 = 2;

/// 检查 wait4 是否仍有匹配且尚未退出的子进程，因而需要阻塞。
fn wait4_should_block(proc: &Arc<TaskControlBlock>, pid: i32) -> bool {
    let proc_inner = proc.inner_exclusive_access();
    let mut has_match = false;
    for child in proc_inner.children.iter() {
        let child_inner = child.inner_exclusive_access();
        let matches = match pid {
            -1 => true,
            0 => child_inner.pgid == proc_inner.pgid,
            value if value > 0 => child.getpid() == value as usize,
            value if value < -1 => {
                value != i32::MIN && child_inner.pgid == (-value) as usize
            }
            _ => false,
        };
        if matches {
            has_match = true;
            if child_inner.state == TaskStatus::Zombie {
                return false;
            }
        }
    }
    has_match
}

/// 检查 waitid 是否仍有匹配且尚未退出的子进程，因而需要阻塞。
fn waitid_should_block(proc: &Arc<TaskControlBlock>, idtype: i32, id: i32) -> bool {
    let proc_inner = proc.inner_exclusive_access();
    let mut has_match = false;
    for child in proc_inner.children.iter() {
        let child_inner = child.inner_exclusive_access();
        let matches = match idtype {
            P_ALL => true,
            P_PID => id >= 0 && child.getpid() == id as usize,
            P_PGID => id >= 0 && child_inner.pgid == id as usize,
            _ => false,
        };
        if matches {
            has_match = true;
            if child_inner.state == TaskStatus::Zombie {
                return false;
            }
        }
    }
    has_match
}

/// 将当前任务挂到自身 `signal_struct.wait_chldexit`，等待 wait4 条件变化。
pub fn wait4_block_current(proc: &Arc<TaskControlBlock>, pid: i32) -> bool {
    let wait_chldexit = proc
        .inner_exclusive_access()
        .signal
        .exclusive_access()
        .wait_chldexit
        .clone();
    block_current_and_run_next_if_mp(&wait_chldexit, || wait4_should_block(proc, pid))
}

/// 将当前任务挂到自身 `signal_struct.wait_chldexit`，等待 waitid 条件变化。
pub fn waitid_block_current(proc: &Arc<TaskControlBlock>, idtype: i32, id: i32) -> bool {
    let wait_chldexit = proc
        .inner_exclusive_access()
        .signal
        .exclusive_access()
        .wait_chldexit
        .clone();
    block_current_and_run_next_if_mp(&wait_chldexit, || {
        waitid_should_block(proc, idtype, id)
    })
}

/// 子进程退出时唤醒父进程 signal_struct 上的全部子进程等待者。
pub fn wake_child_exit_waiters(parent: &Arc<TaskControlBlock>) -> usize {
    let wait_chldexit = parent
        .inner_exclusive_access()
        .signal
        .exclusive_access()
        .wait_chldexit
        .clone();
    wake_up_all_mp(&wait_chldexit)
}