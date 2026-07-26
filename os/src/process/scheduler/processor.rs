//! Implementation of [`Processor`] and task context switching.

use crate::process::scheduler::runqueue::{fetch_task, SCHED_OTHER};
use crate::process::{TaskContext, TaskControlBlock, TaskStatus};
use crate::get_hart_id;
use crate::MAIN_HART_ID;
use crate::sync::*;
use crate::arch::{
    trap::TrapContext,
    config::*,
};
use alloc::sync::Arc;
use lazy_static::*;
use core::arch::asm;
use core::arch::global_asm;
use core::sync::atomic::Ordering;

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("../../arch/riscv/task/switch.S"));
#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("../../arch/la/task/switch.S"));

extern "C" {
    pub fn __switch(current_task_cx_ptr: *mut TaskContext, next_task_cx_ptr: *const TaskContext);
}

/// Processor management structure.
pub struct Processor {
    pub(crate) current: Option<Arc<TaskControlBlock>>,
    idle_task_cx: TaskContext,
}

impl Processor {
    pub fn new() -> Self {
        Self {
            current: None,
            idle_task_cx: TaskContext::zero_init(),
        }
    }

    pub(crate) fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }

    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }

    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
}

lazy_static! {
    pub static ref PROCESSORS: Arc<[MPSafeCell<Processor>; CPU_CORE_NUM]> = {
        let arr = core::array::from_fn(|_| MPSafeCell::new(Processor::new()));
        Arc::new(arr)
    };
}

pub fn current_processor() -> MPSafeGuard<'static, Processor> {
    let hart_id = get_hart_id();
    PROCESSORS[hart_id].exclusive_access()
}

pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task()
        .unwrap()
        .inner_exclusive_access()
        .get_trap_cx()
}

pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    current_processor().take_current()
}

pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    current_processor().current()
}

pub fn current_tid() -> usize {
    current_task().unwrap().gettid()
}

pub fn current_user_token() -> usize {
    let task = current_task().unwrap();
    let token = task.inner_exclusive_access().get_user_token();
    token
}

pub fn current_user_asid() -> usize {
    let task = current_task().unwrap();
    let asid = task.inner_exclusive_access().get_asid();
    asid
}

pub fn run_tasks() {
    let hart_id = get_hart_id();
    info!("[kernel] Hello from hart {}!", hart_id);
    loop {
        let hart_id = get_hart_id();
        if let Some(task) = fetch_task() {
            let mut processor = current_processor();
            #[cfg(target_arch = "loongarch64")]
            warn!(
                "[la-sched] hart={} selected pid={} tid={} state={:?}",
                hart_id,
                task.getpid(),
                task.gettid(),
                task.inner_exclusive_access().state,
            );
            if task.inner_exclusive_access().on_main_hart
                && hart_id != MAIN_HART_ID.load(Ordering::Acquire)
            {
                let _dispatch = crate::task::lock_dispatch();
                let mut task_inner = task.inner_exclusive_access();
                task_inner.state = TaskStatus::Ready;
                drop(task_inner);
                info!("[kernel] run_tasks: task pid={} is on main hart, but current hart is {}, put it back into pool", task.getpid(), hart_id);
                crate::task::add_task_into_pool_unlocked(task);
                drop(processor);
                crate::arch::timer::set_next_trigger(SCHED_OTHER);
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    asm!("wfi");
                }
                #[cfg(target_arch = "loongarch64")]
                unsafe {
                    asm!("idle 0");
                }
                continue;
            }
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            let task_inner = task.inner_exclusive_access();
            let next_task_cx_ptr = &task_inner.thread.task_ctx as *const TaskContext;
            #[cfg(target_arch = "loongarch64")]
            warn!(
                "[la-sched] switch pid={} tid={} task_ra={:#x} task_sp={:#x} trap_ctx={:#x} user_era={:#x} user_sp={:#x}",
                task.getpid(),
                task.gettid(),
                task_inner.thread.task_ctx.ra,
                task_inner.thread.task_ctx.sp,
                task_inner.thread.trap_ctx,
                task_inner.get_trap_cx().get_rt(),
                task_inner.get_trap_cx().get_sp(),
            );
            drop(task_inner);
            processor.current = Some(task);
            drop(processor);
            let sched_policy = current_task()
                .map(|task| task.inner_exclusive_access().sched_policy)
                .unwrap_or(SCHED_OTHER);
            crate::arch::timer::set_next_trigger(sched_policy);
            unsafe {
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            let prev_task = {
                let mut processor = current_processor();
                processor.take_current()
            };
            if let Some(prev_task) = prev_task {
                let _dispatch = crate::task::lock_dispatch();
                let prev_inner = prev_task.inner_exclusive_access();
                let status = prev_inner.state;
                drop(prev_inner);
                if status == TaskStatus::Ready {
                    crate::task::add_task_into_pool_unlocked(prev_task);
                } else if status == TaskStatus::BlockSaving {
                    prev_task.inner_exclusive_access().state = TaskStatus::Blocked;
                }
            }
        } else {
            crate::arch::timer::set_next_trigger(SCHED_OTHER);
            #[cfg(target_arch = "loongarch64")]
            unsafe {
                asm!("idle 0");
            }
            #[cfg(target_arch = "riscv64")]
            unsafe {
                asm!("wfi");
            }
        }
    }
}

pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    let mut processor = current_processor();
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    drop(processor);
    unsafe {
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
}
