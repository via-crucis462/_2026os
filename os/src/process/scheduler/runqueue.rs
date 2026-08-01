//! Linux 风格的每 CPU 总运行队列。
//!
//! 各调度类的内部数据结构分别位于 `stopRq.rs`、`deadlineRq`、
//! `rtRq`、`cfsRq` 和 `itRq`；本文件只负责聚合及调度类顺序。

use crate::process::scheduler::{CfsRq, DeadlineRq, IdleRq, RtRq, StopRq, CPU_NUM};
use crate::process::{TaskControlBlock, TaskStatus};
use crate::sync::{MPSafeCell, MPSafeGuard};
use crate::get_hart_id;
use alloc::sync::Arc;
use spin::lazy;
use lazy_static::*;

pub const SCHED_OTHER: isize = 0;
pub const SCHED_FIFO: isize = 1;
pub const SCHED_RR: isize = 2;
pub const SCHED_BATCH: isize = 3;
pub const SCHED_IDLE: isize = 5;
pub const SCHED_DEADLINE: isize = 6;

// 核的调度队列与核id
pub struct Rq {
	pub inner: MPSafeCell<Rqinner>,
	pub cpu_id: usize,
}

lazy_static! {
	/// 每 CPU 的运行队列数组，索引为 CPU ID。
	pub static ref RQ_ARRAY: [Rq; CPU_NUM] = core::array::from_fn(|cpu_id| Rq::new(cpu_id));
	/// 兼容退出和 exec 清理路径的全局队列操作串行锁。
	static ref SCHED_DISPATCH_LOCK: MPSafeCell<()> = MPSafeCell::new(());
}

pub fn lock_dispatch() -> MPSafeGuard<'static, ()> {
	SCHED_DISPATCH_LOCK.exclusive_access()
}

impl Rq {
	pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, Rqinner> {
        self.inner.exclusive_access()
    }
	pub fn new(cpu_id: usize) -> Self {
		Self { inner: MPSafeCell::new(Rqinner::new()), cpu_id }
	}
}
/// 每 CPU 运行队列，对应 Linux `struct rq` 的调度类核心部分。
/// 调度类优先级固定为 stop > deadline > rt > cfs > idle。
pub struct Rqinner {
	/// 最高优先级的每 CPU stop 调度类状态。
	pub stop: MPSafeCell<StopRq>,
	/// SCHED_DEADLINE 的每 CPU 运行队列。
	pub deadline: MPSafeCell<DeadlineRq>,
	/// 普通、批处理及 SCHED_IDLE 用户任务使用的 CFS 运行队列。
	pub cfs: MPSafeCell<CfsRq>,
	/// SCHED_FIFO 和 SCHED_RR 使用的实时运行队列。
	pub rt: MPSafeCell<RtRq>,
	/// 最低优先级的每 CPU idle 任务状态。
	pub idle: MPSafeCell<IdleRq>,
	/// 除 per-CPU idle 任务外的可运行任务总数。
	pub nr_running: usize,
}

impl Rqinner {
	/// 创建五个调度类均为空的每 CPU 总运行队列。
	pub fn new() -> Self {
		Self {
			stop: MPSafeCell::new(StopRq::new()),
			deadline: MPSafeCell::new(DeadlineRq::new()),
			cfs: MPSafeCell::new(CfsRq::new()),
			rt: MPSafeCell::new(RtRq::new()),
			idle: MPSafeCell::new(IdleRq::new()),
			nr_running: 0,
		}
	}

	/// 按 Linux 调度类优先级依次选择任务，且不执行出队。
	///
	/// 固定顺序为 stop → deadline → rt → cfs → idle。
	pub fn pick_next_task(&self) -> Option<Arc<TaskControlBlock>> {
		self.stop
			.exclusive_access()
			.pick_next()
			.or_else(|| self.deadline.exclusive_access().pick_next())
			.or_else(|| self.rt.exclusive_access().pick_next())
			.or_else(|| self.cfs.exclusive_access().pick_next())
			.or_else(|| self.idle.exclusive_access().pick_next())
	}

	pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
		self.enqueue_task(task);
	}

	/// 按任务策略入队并更新本 CPU 的可运行任务计数。
	pub fn enqueue_task(&mut self, task: Arc<TaskControlBlock>) {
		if task.inner_exclusive_access().on_rq {
			return;
		}
		self.nr_running += 1;
		let (sched_policy, prio, vruntime, load_weight, absolute_deadline) = {
			let mut inner = task.inner_exclusive_access();
			inner.on_rq = true;
			inner.on_cpu = false;
			(
				inner.sched_policy,
				inner.prio,
				inner.se.vruntime,
				inner.se.load_weight,
				inner.dl.absolute_deadline,
			)
		};
		// 根据调度策略将任务加入相应的调度类队列
		match sched_policy {
			SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => {
				self.cfs.exclusive_access().enqueue(task, vruntime, load_weight)
			}
			SCHED_FIFO | SCHED_RR => {
				let priority = prio.max(0) as usize;
				self.rt.exclusive_access().enqueue(task, priority.min(99), sched_policy == SCHED_RR);
			}
			SCHED_DEADLINE => self.deadline.exclusive_access().enqueue(task, absolute_deadline),
			_ => panic!("Unsupported scheduling policy"),
		}
	}

	/// 从最高可用普通调度类中移除一个任务。
	pub(crate) fn pop_next_task(&mut self) -> Option<Arc<TaskControlBlock>> {
		let task = self.deadline.exclusive_access().pop_next()
			.or_else(|| self.rt.exclusive_access().pop_next())
			.or_else(|| self.cfs.exclusive_access().pop_next());
		if task.is_some() {
			self.nr_running = self.nr_running.saturating_sub(1);
		}
		task
	}

	/// 从本运行队列窃取任务，并把任务的归属 CPU 更新为目标 CPU。
	pub fn steal_task(&mut self, target_cpu: usize) -> Option<Arc<TaskControlBlock>> {
		let task = self.pop_next_task()?;
		{
			let mut inner = task.inner_exclusive_access();
			let target_allowed = target_cpu < usize::BITS as usize
				&& inner.cpus_allowed & (1usize << target_cpu) != 0;
			if inner.on_main_hart || !target_allowed || !inner.rt.migratable {
				drop(inner);
				self.enqueue_task(Arc::clone(&task));
				return None;
			}
			inner.cpu = target_cpu;
			inner.on_rq = false;
		}
		Some(task)
	}

	/// 从本 CPU 的任一普通调度类队列中移除指定线程。
	fn remove_task(&mut self, tid: usize) -> bool {
		let task = self.deadline.exclusive_access().remove_task(tid)
			.or_else(|| self.rt.exclusive_access().remove_task(tid))
			.or_else(|| self.cfs.exclusive_access().remove_task(tid));
		if let Some(task) = task {
			self.nr_running = self.nr_running.saturating_sub(1);
			task.inner_exclusive_access().on_rq = false;
			true
		} else {
			false
		}
	}
}

/// 将任务加入指定 CPU 的本地运行队列。
pub fn enqueue_task_on_cpu(task: Arc<TaskControlBlock>, cpu_id: usize) {
	let cpu_id = cpu_id.min(RQ_ARRAY.len().saturating_sub(1));
	{
		let mut inner = task.inner_exclusive_access();
		inner.cpu = cpu_id;
		inner.state = TaskStatus::Ready;
		inner.on_cpu = false;
	}
	RQ_ARRAY[cpu_id].inner_exclusive_access().enqueue_task(task);
}

/// 将新创建的任务加入父任务所在 CPU 的本地运行队列。
pub fn enqueue_new_task(task: Arc<TaskControlBlock>, parent_cpu: usize) {
	enqueue_task_on_cpu(task, parent_cpu);
}

/// 将任务加入其 TCB 当前记录的本地运行队列。
pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
	let cpu_id = task.inner_exclusive_access().cpu;
	enqueue_task_on_cpu(task, cpu_id);
}

/// 兼容旧调用者；新框架不再需要全局 dispatch lock。
pub(crate) fn add_task_into_pool_unlocked(task: Arc<TaskControlBlock>) {
	add_task_into_pool(task);
}

pub(crate) fn remove_task_from_all_local_queues_unlocked(tid: usize) {
	for rq in RQ_ARRAY.iter() {
		if rq.inner_exclusive_access().remove_task(tid) {
			break;
		}
	}
}

/// 新框架没有全局任务池，保留为空操作以兼容迁移中的清理路径。
pub(crate) fn remove_task_from_global_pool_unlocked(_tid: usize) {}

/// 当前 CFS 实体不再可运行时，推进对应 CPU 的最小虚拟运行时间。
pub(crate) fn advance_cfs_min_vruntime(cpu_id: usize) {
	let cpu_id = cpu_id.min(RQ_ARRAY.len().saturating_sub(1));
	RQ_ARRAY[cpu_id]
		.inner_exclusive_access()
		.cfs
		.exclusive_access()
		.advance_min_vruntime();
}

/// 唤醒阻塞任务，并重新加入它原先所属 CPU 的运行队列。
pub fn wake_up_task(task: Arc<TaskControlBlock>) {
	let should_enqueue = loop {
		let mut inner = task.inner_exclusive_access();
		if matches!(inner.state, TaskStatus::Blocked) {
			inner.state = TaskStatus::Ready;
			break true;
		} else if inner.state == TaskStatus::BlockSaving {
			// 原本实现没有 loop，直接返回 false ，似乎会把 BlockSaving 的任务给直接丢弃掉
			// 添加一个 loop 以及调试信息
			println!(
				"[kernel] wake_up_task: task {} is saving context, current state: {:?}",
				task.getpid(), inner.state
			);
		} else {
			warn!(
				"[kernel] wake_up_task: task {} is not blocked, current state: {:?}",
				task.getpid(), inner.state
			);
			break false;
		}
	};
	if should_enqueue {
		add_task_into_pool(task);
	}
}

/// 从当前 CPU 自己的运行队列获取任务，不执行跨核窃取。
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
	let cpu_id = get_hart_id();
	let task = RQ_ARRAY[cpu_id]
		.inner_exclusive_access()
		.pop_next_task()?;
	{
		let mut inner = task.inner_exclusive_access();
		inner.cpu = cpu_id;
		inner.on_rq = false;
		inner.on_cpu = true;
		inner.state = TaskStatus::Running;
		inner.need_resched = false;
	}
	Some(task)
}
