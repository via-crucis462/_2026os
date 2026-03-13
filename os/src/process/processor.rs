//! Implementation of [`Processor`] and Intersection of control flow
//!
//! Here, the continuous operation of user apps in CPU is maintained,
//! the current running state of CPU is recorded,
//! and the replacement and transfer of control flow of different applications are executed.

use super::__switch;
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
#[cfg(target_arch = "riscv64")]
use crate::get_hart_id;
use crate::sync::MPSafeCell;
use crate::arch::{
    trap::TrapContext,
    config::*,
};
use alloc::sync::Arc;
use lazy_static::*;

/// Processor management structure
/// 控制单个核心的运行
pub struct Processor {
    ///The task currently executing on the current processor
    current: Option<Arc<TaskControlBlock>>,

    ///The basic control flow of each core, helping to select and switch process
    idle_task_cx: TaskContext,
}

impl Processor {
    ///Create an empty Processor
    pub fn new() -> Self {
        Self {
            current: None,
            idle_task_cx: TaskContext::zero_init(),
        }
    }

    ///Get mutable reference to `idle_task_cx`
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }

    ///Get current task in moving semanteme
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }

    ///Get current task in cloning semanteme
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
}

// 对数组本身不套锁，因为初始化后不会修改数组内容
lazy_static! {
    pub static ref PROCESSORS: Arc<[MPSafeCell<Processor>; CPU_CORE_NUM]> = {
      let mut arr: [MPSafeCell<Processor>; CPU_CORE_NUM] = unsafe { core::mem::zeroed() };
        for i in 0..CPU_CORE_NUM {
            arr[i] = MPSafeCell::new(Processor::new());
        }
        Arc::new(arr)
    };
}

// 获取并锁住当前处理器
pub fn current_processor() -> spin::MutexGuard<'static, Processor> {
    #[cfg(target_arch = "riscv64")]
    let hart_id = get_hart_id();
    PROCESSORS[hart_id].exclusive_access()
}

///The main part of process execution and scheduling
///Loop `fetch_task` to get the process that needs to run, and switch the process through `__switch`
/// 待修改
pub fn run_tasks() {
    //let mut counter: usize = 0;
    loop {
        //counter += 1;
        //println!("run_tasks counter: {}", counter);
        let mut processor = current_processor();
        if let Some(task) = fetch_task() {
            info!("[kernel] run_tasks: fetched tid={}", task.tid.0);
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // access coming task TCB exclusively
            let mut task_inner = task.inner_exclusive_access();
            let next_task_cx_ptr = &task_inner.task_cx as *const TaskContext;
            task_inner.task_status = TaskStatus::Running;
            // release coming task_inner manually
            drop(task_inner);
            println!("[kernel] run_tasks: switching to pid={}", task.tid.0);
            // release coming task TCB manually
            processor.current = Some(task);
            println!("1");
            // release processor manually
            // 释放锁
            drop(processor);
            unsafe {
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
        } else {
            warn!("no tasks available in run_tasks");
        }
    }
}

/// Get current task through take, leaving a None in its place
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    current_processor().take_current()
}

/// Get a copy of the current task
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    current_processor().current()
}

pub fn current_tid() -> usize {
    current_task().unwrap().gettid()
}

/// Get the current user token(addr of page table)
pub fn current_user_token() -> usize {
    let task = current_task().unwrap();
    task.process().inner_exclusive_access().get_user_token()
}

pub fn current_user_asid() -> usize {
    let task = current_task().unwrap();
    task.process().inner_exclusive_access().get_asid()
}

/// Get the mutable reference to trap context of current task
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task()
        .unwrap()
        .inner_exclusive_access()
        .get_trap_cx()
}

/// Return to idle control flow for new scheduling
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    let mut processor = current_processor();
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    drop(processor);
    unsafe {
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
}
