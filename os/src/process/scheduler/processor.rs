//! Implementation of [`Processor`] and task context switching.

use crate::process::scheduler::idle_tasks;
use crate::process::scheduler::nanosleep::wake_expired_sleep_tasks;
use crate::process::scheduler::runqueue::{
    advance_cfs_min_vruntime, enqueue_task_on_cpu, fetch_task, SCHED_BATCH, SCHED_IDLE,
    SCHED_OTHER, SCHED_RR
};
use crate::process::{TaskContext, TaskControlBlock, TaskStatus};
#[cfg(target_arch = "riscv64")]
use crate::mm::{kernel_token, switch_mm};
use crate::mm::MemorySet;
use crate::arch::timer::get_time_us;
use crate::get_hart_id;
use crate::sync::*;
use crate::arch::{
    trap::TrapContext,
    config::*,
};
use alloc::sync::Arc;
use lazy_static::*;
use core::arch::asm;
use core::arch::global_asm;

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

/// Clone the current task's address-space handle under a short task-inner
/// lock.  User-pointer helpers take this handle directly; they must not use a
/// hardware token to recover the address space through a global registry.
pub fn current_user_mm() -> Arc<MemorySet> {
    let task = current_task().expect("user pointer access without a current task");
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .expect("user task has no mm");
    mm
}

pub fn current_user_asid() -> usize {
    let task = current_task().unwrap();
    let asid = task.inner_exclusive_access().get_asid();
    asid
}

pub fn run_tasks() {
    let hart_id = get_hart_id();
    // let mut times = 0;
    info!("[kernel] Hello from hart {}!", hart_id);
    loop {
        let hart_id = get_hart_id();
        // 先处理到期睡眠任务，再只从当前 CPU 的本地队列取任务。
        wake_expired_sleep_tasks();
        // 本地队列为空时，idle_task 才会从其他 CPU 窃取一个可迁移任务。
        let next_task = fetch_task().or_else(|| idle_tasks(hart_id));
        if let Some(task) = next_task {
            {
                let mut inner = task.inner_exclusive_access();
                inner.cpu = hart_id;
                inner.on_rq = false;
                inner.on_cpu = true;
                inner.state = TaskStatus::Running;
                inner.need_resched = false;
                // 使用单调微秒时钟记录本时间片起点；切回调度器时统一结算。
                inner.se.exec_start = get_time_us() as u64;
            }
            let mut processor = current_processor();
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            let task_inner = task.inner_exclusive_access();
            #[cfg(target_arch = "riscv64")]
            let next_token = task_inner
                .mm
                .as_ref()
                .map(|mm| mm.token())
                .unwrap_or_else(kernel_token);
            let next_task_cx_ptr = &task_inner.thread.task_ctx as *const TaskContext;
            drop(task_inner);
            processor.current = Some(task);
            drop(processor);
            let sched_policy = current_task()
                .map(|task| task.inner_exclusive_access().sched_policy)
                .unwrap_or(SCHED_OTHER);
            crate::arch::timer::set_next_trigger(sched_policy);
            #[cfg(target_arch = "riscv64")]
            switch_mm(next_token);
            unsafe {
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            // println!("[kernel] hart {} back to scheduler", hart_id);
            let prev_task = {
                let mut processor = current_processor();
                processor.take_current()
            };
            if let Some(prev_task) = prev_task {
				let (status, cpu_id, sched_policy) = {
					let mut prev_inner = prev_task.inner_exclusive_access();
                    let now = get_time_us() as u64;
                    let delta_exec = now.saturating_sub(prev_inner.se.exec_start).max(1);
                    prev_inner.se.prev_sum_exec_runtime = prev_inner.se.sum_exec_runtime;
                    prev_inner.se.sum_exec_runtime = prev_inner.se.sum_exec_runtime
                        .saturating_add(delta_exec);
                    // nice=0 时 load_weight=1024，vruntime 与实际运行微秒数等速增长。
                    let weight = prev_inner.se.load_weight.max(1);
                    let delta_vruntime = delta_exec.saturating_mul(1024) / weight;
                    prev_inner.se.vruntime = prev_inner.se.vruntime
                        .saturating_add(delta_vruntime.max(1));
					prev_inner.se.exec_start = 0;
					prev_inner.on_cpu = false;
					// Commit BlockSaving while still holding the task lock.  A waker
					// can either set wake_pending before this point, in which case we
					// publish Ready, or observe the stable Blocked state afterwards
					// and enqueue it itself.  Do not leave a gap between observing
					// wake_pending and publishing Blocked.
					if prev_inner.state == TaskStatus::BlockSaving {
						if prev_inner.wake_pending {
							prev_inner.state = TaskStatus::Ready;
							prev_inner.wake_pending = false;
						} else {
							prev_inner.state = TaskStatus::Blocked;
						}
					}
					(
						prev_inner.state,
						prev_inner.cpu,
						prev_inner.sched_policy,
					)
				};
				if status == TaskStatus::Ready {
					enqueue_task_on_cpu(prev_task, cpu_id);
                } else {
                    if matches!(sched_policy, SCHED_OTHER | SCHED_BATCH | SCHED_IDLE) {
                        advance_cfs_min_vruntime(cpu_id);
                    }
                }
            }
        } else {
            crate::arch::timer::set_next_trigger(SCHED_IDLE);
            #[cfg(target_arch = "loongarch64")]
            #[cfg(board = "virt")]
            unsafe {
                asm!("idle 0");
            }
            #[cfg(board = "2k1000")]
            core::hint::spin_loop();

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
    // debug!("[kernel] task_ctx {:p} - hart {} scheduled", switched_task_cx_ptr, get_hart_id());
    drop(processor);
    unsafe {
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
    // debug!("[kernel] task_ctx {:p} - hart {} back to scheduler", idle_task_cx_ptr, get_hart_id());
}
