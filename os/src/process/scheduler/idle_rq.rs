//! Linux idle scheduling class 的每 CPU 状态。

use crate::process::TaskControlBlock;
use crate::process::scheduler::runqueue::{steal_task_from_cpu, StealResult, RQ_ARRAY};
use alloc::sync::Arc;

/// Linux idle class 不维护普通队列，每 CPU 固定持有一个 `rq->idle`。
pub struct IdleRq {
	/// 绑定到本 CPU 的永久 idle 任务，对应 Linux `rq->idle`。
	idle: Option<Arc<TaskControlBlock>>,
}

impl IdleRq {
	/// 创建尚未绑定 idle 任务的 idle 队列状态。
	pub fn new() -> Self {
		Self { idle: None }
	}

	/// 绑定本 CPU 专用的 idle 任务。
	pub fn set_idle_task(&mut self, task: Arc<TaskControlBlock>) {
		self.idle = Some(task);
	}

	/// 返回 idle 任务；仅在其他所有调度类均无任务时调用。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		self.idle.as_ref().map(Arc::clone)
	}
}

/// 当前 CPU 无本地任务时执行一次空闲负载均衡。
///
/// 函数寻找可运行任务数最多的远端 CPU，并从其运行队列中窃取一个
/// 普通可迁移任务。它运行在内核调度循环中，不代表一个独立的用户任务。
pub fn idle_tasks(cpu_id: usize) -> Option<Arc<TaskControlBlock>> {
	if cpu_id >= RQ_ARRAY.len() {
		return None;
	}
	let mut busiest_cpu = None;
	let mut busiest_load = 0;

	for remote_cpu in 0..RQ_ARRAY.len() {
		if remote_cpu == cpu_id {
			continue;
		}
		// Avoid serialising every idle scan on a remote rq lock. The following
		// steal transaction validates the candidate under that source rq's lock.
		let load = RQ_ARRAY[remote_cpu].view.snapshot().runnable_count;
		if load > busiest_load {
			busiest_load = load;
			busiest_cpu = Some(remote_cpu);
		}
	}

	let remote_cpu = busiest_cpu?;
	// `runnable_count` includes a remote CPU's current task, while only tasks
	// already queued in the exact rq can be pulled. A value of two can therefore
	// mean one running task plus one valid steal candidate.
	if busiest_load == 0 {
		return None;
	}

	let result = steal_task_from_cpu(remote_cpu, cpu_id);
	match result {
		StealResult::Stolen(task, _) => Some(task),
		StealResult::Dropped(_) => None,
		StealResult::None => None,
	}
}

/// 兼容原有单数命名的空闲负载均衡入口。
pub fn idle_task(cpu_id: usize) -> Option<Arc<TaskControlBlock>> {
	idle_tasks(cpu_id)
}
