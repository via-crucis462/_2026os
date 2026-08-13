//! Implementation of [`Processor`] and task context switching.

use crate::process::scheduler::idle_tasks;
use crate::process::scheduler::nanosleep::wake_expired_sleep_tasks;
use crate::process::scheduler::runqueue::{
    advance_cfs_min_vruntime, enqueue_resumed_task, fetch_task, scheduler_cpu_online,
    scheduler_cpu_running, scheduler_idle_arm, scheduler_idle_exit, scheduler_idle_prepare,
    scheduler_task_stopped, SCHED_BATCH, SCHED_IDLE, SCHED_OTHER, SCHED_RR,
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
	// The RISC-V kernel trap vector is diagnostic-only and cannot return to
	// the interrupted instruction. Keep SIE disabled while the scheduler is
	// in kernel context; WFI resumes for a pending SSIP and the idle loop
	// acknowledges that source below.
	#[cfg(target_arch = "riscv64")]
	unsafe {
		riscv::register::sstatus::clear_sie();
	}
    // let mut times = 0;
    info!("[kernel] Hello from hart {}!", hart_id);
	 scheduler_cpu_online(hart_id);
    loop {
        let hart_id = get_hart_id();
        // 先处理到期睡眠任务，再只从当前 CPU 的本地队列取任务。
        wake_expired_sleep_tasks();
        // 本地队列为空时，idle_task 才会从其他 CPU 窃取一个可迁移任务。
		let next_task = fetch_task().or_else(|| idle_tasks(hart_id));
		if let Some(task) = next_task {
			scheduler_cpu_running(hart_id);
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
            let (sched_policy, time_slice_ms) = current_task()
                .map(|task| {
                    let mut inner = task.inner_exclusive_access();
                    let time_slice_ms = if matches!(inner.sched_policy, SCHED_OTHER | SCHED_BATCH | SCHED_IDLE) {
                        let time_slice_ms = crate::process::scheduler::CfsRq::time_slice_ms(inner.se.queue_level);
                        inner.se.queue_level = (inner.se.queue_level + 1).min(2);
                        time_slice_ms
                    } else {
                        0
                    };
                    (inner.sched_policy, time_slice_ms)
                })
                .unwrap_or((SCHED_OTHER, 1));
            if time_slice_ms == 0 {
                crate::arch::timer::set_next_trigger(sched_policy);
            } else {
                crate::arch::timer::set_next_trigger_ms(time_slice_ms);
            }
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
				let (status, cpu_id, sched_policy, load_weight, wake_source_cpu) = {
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
							let wake_source_cpu = prev_inner.wake_source_cpu.take();
							(
								prev_inner.state,
								prev_inner.cpu,
								prev_inner.sched_policy,
								prev_inner.se.load_weight,
								wake_source_cpu,
							)
						} else {
							prev_inner.state = TaskStatus::Blocked;
							prev_inner.wake_source_cpu = None;
							(
								prev_inner.state,
								prev_inner.cpu,
								prev_inner.sched_policy,
								prev_inner.se.load_weight,
								None,
							)
						}
					} else {
						(
							prev_inner.state,
							prev_inner.cpu,
							prev_inner.sched_policy,
							prev_inner.se.load_weight,
							None,
						)
					}
				};
				if status == TaskStatus::Ready {
					if let Some(wake_source_cpu) = wake_source_cpu {
						let _ = enqueue_resumed_task(prev_task, wake_source_cpu);
					} else {
						crate::process::scheduler::runqueue::add_task_into_pool(prev_task);
					}
                } else {
					scheduler_task_stopped(cpu_id, load_weight);
                    if matches!(sched_policy, SCHED_OTHER | SCHED_BATCH | SCHED_IDLE) {
                        advance_cfs_min_vruntime(cpu_id);
                    }
                }
            }
        } else {
            crate::arch::timer::set_next_trigger(SCHED_IDLE);
			scheduler_idle_prepare(hart_id);
			if !scheduler_idle_arm(hart_id) {
				continue;
			}
			// RISC-V's kernel trap vector has no save/restore frame, so its
			// normal kernel execution keeps SIE disabled. WFI still resumes
			// for a pending enabled SSIP, which is acknowledged below.
			// LoongArch keeps CRMD.IE disabled in kernel context as well.
			// `idle 0` returns for a pending IPI, and the action is consumed
			// below without entering a kernel interrupt handler.
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
			// Close the idle notification gate before acknowledging the hardware
			// source.  This also waits for any sender that already observed
			// IdleWfi, so no scheduler IPI can survive this clear and cause a
			// redundant trap immediately after returning to userspace.
			scheduler_idle_exit(hart_id);
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
