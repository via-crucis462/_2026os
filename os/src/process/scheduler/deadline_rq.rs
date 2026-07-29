//! Linux deadline runqueue：按绝对截止时间排序的 cached rbtree。

use super::rbtree::RbRootCached;
use crate::process::TaskControlBlock;
use alloc::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeadlineKey {
	/// CBS 补充后的任务绝对截止时间，数值越小优先级越高。
	deadline: u64,
	/// 截止时间相同时用于稳定排序的线程 ID。
	tid: usize,
}

/// 每 CPU 的 SCHED_DEADLINE 运行队列，对应 Linux `struct dl_rq`。
pub struct DeadlineRq {
	/// 所有可运行 DL 任务，按绝对截止时间排序并缓存最早任务。
	root: RbRootCached<DeadlineKey, Arc<TaskControlBlock>>,
	/// CONFIG_SMP：可迁移 DL 任务按 deadline 排序。
	pushable_dl_tasks_root: RbRootCached<DeadlineKey, Arc<TaskControlBlock>>,
	/// 当前可运行的 SCHED_DEADLINE 任务数量。
	pub dl_nr_running: usize,
	/// 当前正在消耗的 DL 带宽。
	pub running_bw: u64,
	/// 已准入到本 CPU 的 DL 总带宽。
	pub this_bw: u64,
	/// 当前运行任务的最早截止时间，用于 SMP 推拉决策。
	pub earliest_dl_curr: u64,
	/// 本队列下一可运行任务的最早截止时间。
	pub earliest_dl_next: u64,
	/// 是否存在可推送到其他 CPU 的 DL 任务。
	pub overloaded: bool,
}

impl DeadlineRq {
	/// 创建空的 deadline 运行队列，最早截止时间初始化为无穷大。
	pub fn new() -> Self {
		Self {
			root: RbRootCached::new(),
			pushable_dl_tasks_root: RbRootCached::new(),
			dl_nr_running: 0,
			running_bw: 0,
			this_bw: 0,
			earliest_dl_curr: u64::MAX,
			earliest_dl_next: u64::MAX,
			overloaded: false,
		}
	}

	/// 按绝对截止时间将任务加入本 CPU 的 deadline 红黑树。
	pub fn enqueue(&mut self, task: Arc<TaskControlBlock>, absolute_deadline: u64) {
		self.root.insert(
			DeadlineKey { deadline: absolute_deadline, tid: task.gettid() },
			task,
		);
		self.dl_nr_running += 1;
	}

	/// 从本 CPU 的 deadline 红黑树移除指定任务。
	pub fn dequeue(&mut self, tid: usize, absolute_deadline: u64) -> Option<Arc<TaskControlBlock>> {
		let task = self.root.remove(DeadlineKey { deadline: absolute_deadline, tid });
		if task.is_some() {
			self.dl_nr_running -= 1;
		}
		task
	}

	/// 返回具有最早绝对截止时间的任务，但不执行出队。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		self.root.first().map(Arc::clone)
	}

	/// 移除并返回截止时间最早的任务。
	pub fn pop_next(&mut self) -> Option<Arc<TaskControlBlock>> {
		let task = self.root.pop_first()?;
		self.dl_nr_running = self.dl_nr_running.saturating_sub(1);
		self.earliest_dl_next = self.root
			.first_key()
			.map(|key| key.deadline)
			.unwrap_or(u64::MAX);
		Some(task)
	}

	/// 按线程 ID 从 deadline 队列中移除任务。
	pub fn remove_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		let task = self.root.remove_where(|task| task.gettid() == tid)?;
		self.dl_nr_running = self.dl_nr_running.saturating_sub(1);
		Some(task)
	}

	/// 将允许迁移的任务加入 SMP pushable deadline 红黑树。
	pub fn enqueue_pushable(&mut self, task: Arc<TaskControlBlock>, absolute_deadline: u64) {
		self.pushable_dl_tasks_root.insert(
			DeadlineKey { deadline: absolute_deadline, tid: task.gettid() },
			task,
		);
		self.overloaded = !self.pushable_dl_tasks_root.is_empty();
	}

	/// 从 SMP pushable deadline 红黑树移除任务并更新过载状态。
	pub fn dequeue_pushable(&mut self, tid: usize, absolute_deadline: u64) -> Option<Arc<TaskControlBlock>> {
		let task = self.pushable_dl_tasks_root.remove(DeadlineKey {
			deadline: absolute_deadline,
			tid,
		});
		self.overloaded = !self.pushable_dl_tasks_root.is_empty();
		task
	}
}
