//! 三级多级反馈队列调度器。

use crate::process::TaskControlBlock;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

const QUEUE_COUNT: usize = 3;
const BOOST_INTERVAL: usize = 20;
const TIME_SLICES_MS: [usize; QUEUE_COUNT] = [1, 2, 5];

/// 对应 Linux `struct load_weight`。
pub struct LoadWeight {
	/// 当前 CFS 队列中全部可运行实体的权重总和。
	pub weight: u64,
	/// 权重倒数缓存，用于将除法优化为乘法；当前仅预留 Linux 字段。
	pub inv_weight: u32,
}

/// 每 CPU 的 CFS 运行队列，对应 Linux `struct cfs_rq` 的核心字段。
pub struct CfsRq {
	/// 队列中可运行实体的总负载权重。
	pub load: LoadWeight,
	/// 直接挂在本 CFS 队列上的可运行实体数量。
	pub nr_queued: usize,
	/// 包含调度组层次后的可运行实体数量。
	pub h_nr_queued: usize,
	/// 保留的兼容统计字段；MLFQ 不再按虚拟运行时间排序。
	pub min_vruntime: u64,
	/// 三个优先级依次降低的 FIFO 队列。
	tasks: [VecDeque<Arc<TaskControlBlock>>; QUEUE_COUNT],
	/// 本 CFS 队列累计消耗的实际执行时间。
	pub exec_clock: u64,
	/// 距离上一次优先级提升已经取出的任务数。
	dispatches_since_boost: usize,
}

impl CfsRq {
	/// 创建一个没有可运行实体的 CFS 队列。
	pub fn new() -> Self {
		Self {
			load: LoadWeight { weight: 0, inv_weight: 0 },
			nr_queued: 0,
			h_nr_queued: 0,
			min_vruntime: 0,
			tasks: core::array::from_fn(|_| VecDeque::new()),
			exec_clock: 0,
			dispatches_since_boost: 0,
		}
	}

	/// 返回指定层级的时间片，越低优先级获得越长时间片。
	pub fn time_slice_ms(level: usize) -> usize {
		TIME_SLICES_MS[level.min(QUEUE_COUNT - 1)]
	}

	/// 按任务当前层级加入对应 FIFO 队尾。
	pub fn enqueue(&mut self, task: Arc<TaskControlBlock>, _vruntime: u64, weight: u64) {
		let level = {
			let mut inner = task.inner_exclusive_access();
			let level = inner.se.queue_level.min(QUEUE_COUNT - 1);
			inner.se.queue_level = level;
			level
		};
		self.tasks[level].push_back(task);
		self.nr_queued += 1;
		self.h_nr_queued += 1;
		self.load.weight = self.load.weight.saturating_add(weight);
	}

	/// 从任意层移除指定任务。
	pub fn dequeue(&mut self, tid: usize, _vruntime: u64, weight: u64) -> Option<Arc<TaskControlBlock>> {
		let task = self.remove_matching(|task| task.gettid() == tid)?;
		self.account_dequeue(weight);
		Some(task)
	}

	/// 返回最高优先级非空队列的队首任务，但不执行出队。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		self.tasks.iter().find_map(|queue| queue.front().map(Arc::clone))
	}

	/// 从最高优先级非空队列取出任务，并记录一次调度。
	pub fn pop_next(&mut self) -> Option<Arc<TaskControlBlock>> {
		if self.dispatches_since_boost >= BOOST_INTERVAL {
			self.boost_bottom_queue();
			self.dispatches_since_boost = 0;
		}
		let task = self.tasks.iter_mut().find_map(VecDeque::pop_front)?;
		let load_weight = task.inner_exclusive_access().se.load_weight;
		self.account_dequeue(load_weight);
		self.dispatches_since_boost += 1;
		Some(task)
	}

	/// 按线程 ID 从任意层移除任务。
	pub fn remove_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		let task = self.remove_matching(|task| task.gettid() == tid)?;
		let load_weight = task.inner_exclusive_access().se.load_weight;
		self.account_dequeue(load_weight);
		Some(task)
	}

	/// MLFQ 不依赖虚拟运行时间，保留为空操作以兼容阻塞路径。
	pub fn advance_min_vruntime(&mut self) {}

	fn boost_bottom_queue(&mut self) {
		while let Some(task) = self.tasks[QUEUE_COUNT - 1].pop_front() {
			task.inner_exclusive_access().se.queue_level = 0;
			self.tasks[0].push_back(task);
		}
	}

	fn remove_matching(
		&mut self,
		mut predicate: impl FnMut(&Arc<TaskControlBlock>) -> bool,
	) -> Option<Arc<TaskControlBlock>> {
		for queue in &mut self.tasks {
			if let Some(index) = queue.iter().position(&mut predicate) {
				return queue.remove(index);
			}
		}
		None
	}

	fn account_dequeue(&mut self, weight: u64) {
		self.nr_queued = self.nr_queued.saturating_sub(1);
		self.h_nr_queued = self.h_nr_queued.saturating_sub(1);
		self.load.weight = self.load.weight.saturating_sub(weight);
	}
}
