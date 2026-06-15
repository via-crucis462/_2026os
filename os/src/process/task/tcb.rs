//! Types related to task management & Functions for completely changing TCB
#![allow(unused)]
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle, TIdHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    arch::trap::{TrapContext, trap_handler},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap},
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
    pub tid: Arc<TIdHandle>,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    pub inner: MPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    pub fn process(&self) -> Arc<ProcessControlBlock> {
        self.process.upgrade().unwrap()
    }
    pub fn getpid(&self) -> usize {
        self.process().pid.0
    }
    pub fn gettid(&self) -> usize {
        self.tid.0
    }

    pub fn recycle_on_exit(&self, exit_code: i32) {
        remove_from_tid2task(self.gettid());

        let mut inner = self.inner_exclusive_access();
        inner.exit_code = exit_code;
        inner.errno = 0;
        inner.task_status = TaskStatus::Zombie;
        inner.signals = SignalFlags::empty();
        inner.signal_interrupted = false;
        inner.signal_mask_backup.clear();
        inner.trap_ctx_backup.clear();
        inner.signal_user_context_backup.clear();
        inner.killed = false;
        inner.term_signal = None;
        inner.frozen = false;
    }
}

pub struct TaskControlBlockInner {

    /// 此处改为直接保存地址
    pub trap_cx_addr: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// 当前由哪个 hart 持有运行所有权；None 表示可被调度领取。
    pub owner_hart: Option<usize>,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,
    pub errno: i32,
    pub signals: SignalFlags,
    pub signal_interrupted: bool,
    pub signal_mask: SignalFlags,
    /// 信号嵌套处理时的掩码栈（当前未完全验证行为是否正确，初步测试没问题）
    pub signal_mask_backup: Vec<SignalFlags>,
    // if the task is killed
    pub killed: bool,
    pub term_signal: Option<i32>,
    // if the task is frozen by a signal
    pub frozen: bool,
    /// 信号嵌套处理时的上下文栈（当前未完全验证行为是否正确，初步测试没问题）
    pub trap_ctx_backup: Vec<TrapContext>,

    /// 用户态 signal frame 中 ucontext 的地址，用于 sigreturn 读取用户修改后的上下文。
    pub signal_user_context_backup: Vec<usize>,

    pub clear_child_tid: usize,// 线程清理指针
}

impl TaskControlBlockInner {
    #[cfg(target_arch = "riscv64")]
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        PhysAddr(self.trap_cx_addr).get_mut()
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        unsafe { (self.trap_cx_addr as *mut TrapContext).as_mut().unwrap() }
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
    /// 被阻塞（目前是被锁阻塞）
    Blocked,
    /// exited
    Zombie,
    /// wait函数保存上下文前
    WaitSaving,
}
