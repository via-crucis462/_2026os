//! Implementation of [`Processor`] and Intersection of control flow
//!
//! Here, the continuous operation of user apps in CPU is maintained,
//! the current running state of CPU is recorded,
//! and the replacement and transfer of control flow of different applications are executed.

use super::__switch;
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::get_hart_id;
use crate::MAIN_HART_ID;
use crate::sync::*;
use crate::arch::{
    trap::TrapContext,
    config::*,
};
use crate::task::{add_task_into_pool, manager};
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
        // 数组宏，给每个元素调用一次函数取返回值
        let arr = core::array::from_fn(|_| MPSafeCell::new(Processor::new()));
        Arc::new(arr)
    };
}

// 获取并锁住当前处理器
pub fn current_processor() -> MPSafeGuard<'static, Processor> {
    let hart_id = get_hart_id();
    PROCESSORS[hart_id].exclusive_access()
}

use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
///The main part of process execution and scheduling
///Loop `fetch_task` to get the process that needs to run, and switch the process through `__switch`
pub fn run_tasks() {
    //let mut counter: usize = 0;
        let hart_id = get_hart_id();
        info!("[kernel] Hello from hart {}!", hart_id);
    loop {
        //counter += 1;
        //println!("run_tasks counter: {}", counter);
        let hart_id = get_hart_id();
        if let Some(task) = fetch_task() {
            let mut processor = current_processor();
            if (task.process().inner_exclusive_access().on_main_hart &&
                hart_id != MAIN_HART_ID.load(Ordering::Acquire)) {
                let _dispatch = crate::task::lock_dispatch();
                let mut task_inner = task.inner_exclusive_access();
                task_inner.task_status = TaskStatus::Ready;
                task_inner.owner_hart = None;
                drop(task_inner);
                info!("[kernel] run_tasks: task pid={} is on main hart, but current hart is {}, put it back into pool", task.getpid(), hart_id);
                crate::task::add_task_into_pool_unlocked(task);
                drop(processor);
                #[cfg(target_arch = "riscv64")]
                {
                    crate::arch::timer::set_next_trigger();
                    unsafe {
                        asm!("wfi");
                    }
                }
                continue;
            } 
            //warn!("[kernel] hart {}, run_tasks: fetched tid={} of pid={}", hart_id, task.tid.0, task.getpid());
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            let task_inner = task.inner_exclusive_access();
            let next_task_cx_ptr = &task_inner.task_cx as *const TaskContext;
            drop(task_inner);
            // release coming task TCB manually
            processor.current = Some(task);
            // release processor manually
            // 释放锁
            drop(processor);
            //debug!("[kernel] hart {}, run_tasks: switching to tid={} of pid={}, main_hart={}", hart_id, current_task().unwrap().tid.0, current_task().unwrap().getpid(), MAIN_HART_ID.load(Ordering::Acquire));
            unsafe {
                // 切换到下一个任务执行流
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            // suspend_current_and_run_next以及exit_current_and_run_next会跳到这里
            let prev_task = {
                let mut processor = current_processor();
                processor.take_current()
            };
            if let Some(prev_task) = prev_task {
                let _dispatch = crate::task::lock_dispatch();
                let mut prev_inner = prev_task.inner_exclusive_access();
                let status = prev_inner.task_status;
                prev_inner.owner_hart = None;
                drop(prev_inner);
                if status == TaskStatus::Ready {
                    // 之前已经保存好了
                    let on_main_hart = prev_task.process().inner_exclusive_access().on_main_hart;
                    if on_main_hart {
                        crate::task::manager::add_task_in_current_hart_unlocked(prev_task);
                    } else {
                        crate::task::add_task_into_pool_unlocked(prev_task);
                    }
                } /*else if status == TaskStatus::WaitSaving {
                    // 调用了wait函数
                    prev_task.inner_exclusive_access().task_status = TaskStatus::Blocked;
                }*/
                // 如果 status 是 Zombie 或 Blocked，什么都不做，自然销毁或等别人唤醒
            }
        } else {
            #[cfg(target_arch = "riscv64")]
            crate::arch::timer::set_next_trigger();
            #[cfg(target_arch = "loongarch64")]
            unsafe {
                asm!("idle 0");
            }
            #[cfg(target_arch = "riscv64")]
            unsafe {
                asm!("wfi");
            }
            trace!("no tasks available in hart {}", hart_id);
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
/// 将参数线程切换到就绪队列，并切换到idle线程的控制流
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    //info!("[kernel] schedule: returning to idle control flow");
    let mut processor = current_processor();
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    drop(processor);
    unsafe {
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
}