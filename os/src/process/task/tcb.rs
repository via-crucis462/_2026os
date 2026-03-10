//! Types related to task management & Functions for completely changing TCB
#![allow(unused)]
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, IdHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    arch::trap::{TrapContext, trap_handler},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, translated_refmut},
    sync::MPSafeCell,
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
#[allow(unused)]
use crate::arch::config::*;
use super::*;



/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// 线程所属进程
    /// 让线程拥有对进程的弱引用，便于调用进程的方法
    /// 不能用arc否则循环引用
    pub process: Weak<ProcessControlBlock>,

    /// 线程id
    pub tid: Arc<IdHandle>,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    pub inner: MPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> spin::MutexGuard<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    pub fn getpid(&self) -> usize {
        if let Some(process) = self.process.upgrade() {
            process.pid.0
        } else {
            0
        }
    }
    pub fn gettid(&self) -> usize {
        self.tid.0
    }
}

pub struct TaskControlBlockInner {

    /// 此处改为直接保存地址
    pub trap_cx_addr: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,
    pub signals: SignalFlags,
    pub signal_mask: SignalFlags,
    // the signal which is being handling
    pub handling_sig: isize,
    // Signal actions
    pub signal_actions: SignalActions,
    // if the task is killed
    pub killed: bool,
    // if the task is frozen by a signal
    pub frozen: bool,
    pub trap_ctx_backup: Option<TrapContext>,
}

impl TaskControlBlockInner {
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        PhysAddr(self.trap_cx_addr).get_mut()
    }
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }

}

impl TaskControlBlock {

}

#[derive(Copy, Clone, PartialEq)]
/// task status: UnInit, Ready, Running, Exited
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Zombie,
}
