//! Linux CFS runqueue：按 vruntime 排序的 cached rbtree。

use super::rbtree::RbRootCached;
use crate::process::TaskControlBlock;
use alloc::sync::Arc;
use core::cmp::Ordering;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CfsKey {
	/// 实体累计的归一化虚拟运行时间，数值越小越应优先运行。
	vruntime: u64,
	/// vruntime 相同时，线程 ID 越大越应优先运行。
	tid: usize,
}

impl Ord for CfsKey {
	fn cmp(&self, other: &Self) -> Ordering {
		self.vruntime
			.cmp(&other.vruntime)
			// 红黑树取最小键，因此反向比较 tid，使较大的 tid 排在更左侧。
			.then_with(|| other.tid.cmp(&self.tid))
	}
}

impl PartialOrd for CfsKey {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

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
	/// 队列当前最小虚拟运行时间，防止新实体获得不公平优势。
	pub min_vruntime: u64,
	/// 按 `vruntime` 升序、`tid` 降序排列并缓存最左节点的时间线红黑树。
	tasks_timeline: RbRootCached<CfsKey, Arc<TaskControlBlock>>,
	/// 本 CFS 队列累计消耗的实际执行时间。
	pub exec_clock: u64,
}

impl CfsRq {
	/// 创建一个没有可运行实体的 CFS 队列。
	pub fn new() -> Self {
		Self {
			load: LoadWeight { weight: 0, inv_weight: 0 },
			nr_queued: 0,
			h_nr_queued: 0,
			min_vruntime: 0,
			tasks_timeline: RbRootCached::new(),
			exec_clock: 0,
		}
	}

	/// 将任务按虚拟运行时间加入 CFS 时间线，并更新数量和负载统计。
	pub fn enqueue(&mut self, task: Arc<TaskControlBlock>, vruntime: u64, weight: u64) {
		self.tasks_timeline.insert(CfsKey { vruntime, tid: task.gettid() }, task);
		self.nr_queued += 1;
		self.h_nr_queued += 1;
		self.load.weight = self.load.weight.saturating_add(weight);
		self.min_vruntime = self.tasks_timeline
			.first_key()
			.map(|key| key.vruntime)
			.unwrap_or(vruntime);
	}

	/// 从 CFS 时间线移除指定任务，并回退对应的数量和负载统计。
	///
	/// 返回 `None` 表示给定 `(tid, vruntime)` 不在队列中。
	pub fn dequeue(&mut self, tid: usize, vruntime: u64, weight: u64) -> Option<Arc<TaskControlBlock>> {
		let task = self.tasks_timeline.remove(CfsKey { vruntime, tid });
		if task.is_some() {
			self.nr_queued -= 1;
			self.h_nr_queued -= 1;
			self.load.weight = self.load.weight.saturating_sub(weight);
		}
		task
	}

	/// 返回 vruntime 最小的任务，但不将其从红黑树中移除。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		self.tasks_timeline.first().map(Arc::clone)
	}

	/// 移除并返回 vruntime 最小的任务，供调度和空闲核负载均衡使用。
	pub fn pop_next(&mut self) -> Option<Arc<TaskControlBlock>> {
		let task = self.tasks_timeline.pop_first()?;
		let load_weight = task.inner_exclusive_access().se.load_weight;
		self.nr_queued = self.nr_queued.saturating_sub(1);
		self.h_nr_queued = self.h_nr_queued.saturating_sub(1);
		self.load.weight = self.load.weight.saturating_sub(load_weight);
		self.min_vruntime = self.tasks_timeline
			.first_key()
			.map(|key| key.vruntime)
			.unwrap_or(self.min_vruntime);
		Some(task)
	}

	/// 按线程 ID 从 CFS 队列中移除任务。
	pub fn remove_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		let task = self.tasks_timeline.remove_where(|task| task.gettid() == tid)?;
		self.nr_queued = self.nr_queued.saturating_sub(1);
		self.h_nr_queued = self.h_nr_queued.saturating_sub(1);
		self.load.weight = self.load.weight.saturating_sub(task.inner_exclusive_access().se.load_weight);
		Some(task)
	}
}
