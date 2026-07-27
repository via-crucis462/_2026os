//! Linux RT runqueue：优先级位图、每优先级 FIFO 以及 SMP pushable 队列。

use core::cmp::Ordering;
use crate::process::TaskControlBlock;
use alloc::{collections::{BinaryHeap, VecDeque}, sync::Arc};

/// Linux 支持的实时优先级数量，对应优先级范围 `[0, 99]`。
pub const MAX_RT_PRIO: usize = 100;
/// 覆盖全部实时优先级所需的 64 位位图字数。
const RT_BITMAP_WORDS: usize = (MAX_RT_PRIO + 63) / 64;

/// 对应 Linux `struct rt_prio_array`：优先级位图加每优先级 FIFO。
pub struct RtPrioArray {
	/// 标记哪些实时优先级存在可运行任务；最低置位代表最高优先级。
	bitmap: [u64; RT_BITMAP_WORDS],
	/// 每个实时优先级对应的 FIFO/RR 任务队列。
	queue: [VecDeque<Arc<TaskControlBlock>>; MAX_RT_PRIO],
}

impl RtPrioArray {
	/// 创建位图清空、所有优先级队列为空的实时优先级数组。
	pub fn new() -> Self {
		Self {
			bitmap: [0; RT_BITMAP_WORDS],
			queue: core::array::from_fn(|_| VecDeque::new()),
		}
	}

	/// 标记指定实时优先级为非空。
	fn set_bit(&mut self, priority: usize) {
		self.bitmap[priority / 64] |= 1u64 << (priority % 64);
	}

	/// 清除指定实时优先级的非空标记。
	fn clear_bit(&mut self, priority: usize) {
		self.bitmap[priority / 64] &= !(1u64 << (priority % 64));
	}

	/// 通过位图查找数值最小、即优先级最高的非空队列。
	fn highest_priority(&self) -> Option<usize> {
		self.bitmap.iter().enumerate().find_map(|(word_index, word)| {
			(*word != 0).then_some(word_index * 64 + word.trailing_zeros() as usize)
		})
	}
}

/// 每 CPU 的实时运行队列，对应 Linux `struct rt_rq` 的核心字段。
pub struct RtRq {
	/// 活跃实时任务的优先级数组。
	active: RtPrioArray,
	/// CONFIG_SMP 下可迁移任务的优先级堆，用于 push/pull 负载均衡。
	pushable_tasks: BinaryHeap<RtPushableTask>,
	/// 所有可运行实时任务数量。
	pub rt_nr_running: usize,
	/// 其中采用 SCHED_RR 策略的任务数量。
	pub rr_nr_running: usize,
	/// 当前实时带宽周期内已经消耗的运行时间。
	pub rt_time: u64,
	/// 每个实时带宽周期允许消耗的最大运行时间。
	pub rt_runtime: u64,
	/// 实时带宽耗尽后置位，置位期间不再选择 RT 任务。
	pub rt_throttled: bool,
}

impl RtRq {
	/// 创建一个未限流且没有可运行任务的实时队列。
	pub fn new() -> Self {
		Self {
			active: RtPrioArray::new(),
			pushable_tasks: BinaryHeap::new(),
			rt_nr_running: 0,
			rr_nr_running: 0,
			rt_time: 0,
			rt_runtime: u64::MAX,
			rt_throttled: false,
		}
	}

	/// 将实时任务加入指定优先级的队尾，并更新 FIFO/RR 统计。
	pub fn enqueue(&mut self, task: Arc<TaskControlBlock>, priority: usize, round_robin: bool) {
		assert!(priority < MAX_RT_PRIO);
		self.active.queue[priority].push_back(task);
		self.active.set_bit(priority);
		self.rt_nr_running += 1;
		if round_robin {
			self.rr_nr_running += 1;
		}
	}

	/// 从指定优先级队列移除线程 ID 匹配的任务。
	pub fn dequeue(&mut self, tid: usize, priority: usize, round_robin: bool) -> Option<Arc<TaskControlBlock>> {
		if priority >= MAX_RT_PRIO {
			return None;
		}
		let position = self.active.queue[priority].iter().position(|task| task.gettid() == tid)?;
		let task = self.active.queue[priority].remove(position);
		if self.active.queue[priority].is_empty() {
			self.active.clear_bit(priority);
		}
		self.rt_nr_running -= 1;
		if round_robin {
			self.rr_nr_running -= 1;
		}
		task
	}

	/// SCHED_RR 时间片耗尽时将当前优先级的队首轮转到队尾。
	pub fn rotate_rr(&mut self, priority: usize) {
		if priority < MAX_RT_PRIO && self.active.queue[priority].len() > 1 {
			self.active.queue[priority].rotate_left(1);
		}
	}

	/// 返回最高实时优先级队列的队首；队列被限流时返回 `None`。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		if self.rt_throttled {
			return None;
		}
		let priority = self.active.highest_priority()?;
		self.active.queue[priority].front().map(Arc::clone)
	}

	/// 移除并返回最高优先级实时任务。
	pub fn pop_next(&mut self) -> Option<Arc<TaskControlBlock>> {
		if self.rt_throttled {
			return None;
		}
		let priority = self.active.highest_priority()?;
		let task = self.active.queue[priority].pop_front()?;
		if self.active.queue[priority].is_empty() {
			self.active.clear_bit(priority);
		}
		self.rt_nr_running = self.rt_nr_running.saturating_sub(1);
		if task.inner_exclusive_access().sched_policy == crate::process::scheduler::SCHED_RR {
			self.rr_nr_running = self.rr_nr_running.saturating_sub(1);
		}
		Some(task)
	}

	/// 将可迁移实时任务加入 SMP pushable 优先级堆。
	pub fn enqueue_pushable(&mut self, task: Arc<TaskControlBlock>, priority: usize) {
		self.pushable_tasks.push(RtPushableTask { priority, task });
	}

	/// 按线程 ID 从 SMP pushable 优先级堆移除任务。
	pub fn dequeue_pushable(&mut self, tid: usize) {
		self.pushable_tasks.retain(|entry| entry.task.gettid() != tid);
	}

	/// 在全部实时优先级队列中按线程 ID 移除任务。
	pub fn remove_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		for priority in 0..MAX_RT_PRIO {
			if let Some(position) = self.active.queue[priority]
				.iter()
				.position(|task| task.gettid() == tid)
			{
				let task = self.active.queue[priority].remove(position)?;
				if self.active.queue[priority].is_empty() {
					self.active.clear_bit(priority);
				}
				self.rt_nr_running = self.rt_nr_running.saturating_sub(1);
				if task.inner_exclusive_access().sched_policy == crate::process::scheduler::SCHED_RR {
					self.rr_nr_running = self.rr_nr_running.saturating_sub(1);
				}
				return Some(task);
			}
		}
		None
	}
}

/// SMP 实时迁移候选，`BinaryHeap` 小端堆。
struct RtPushableTask {
	/// Linux 实时静态优先级，数值越小优先级越高。
	priority: usize,
	/// 对应的可迁移任务。
	task: Arc<TaskControlBlock>,
}

impl PartialEq for RtPushableTask {
	fn eq(&self, other: &Self) -> bool {
		self.priority == other.priority && Arc::ptr_eq(&self.task, &other.task)
	}
}

impl Eq for RtPushableTask {}

impl Ord for RtPushableTask {
	fn cmp(&self, other: &Self) -> Ordering {
		other.priority.cmp(&self.priority)
			.then_with(|| self.task.gettid().cmp(&other.task.gettid()))
	}
}

impl PartialOrd for RtPushableTask {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}
