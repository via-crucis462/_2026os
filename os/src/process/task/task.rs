//! Types related to task management & Functions for completely changing TCB

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


const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;

/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// 线程所在进程的PID
    pub pid: IdHandle,

    /// 线程id
    pub tid: IdHandle,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    inner: MPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> spin::MutexGuard<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// Get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        let inner = self.inner_exclusive_access();
        inner.memory_set.token()
    }
    pub fn get_asid(&self) -> usize {
        let inner = self.inner_exclusive_access();
        inner.memory_set.asid()
    }
}

pub struct TaskControlBlockInner {
    /// 进程名
    pub pname: String,

    /// 此处改为直接保存地址
    pub trap_cx_addr: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,

     pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,
    
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
