//! Nanosleep 使用的按截止时间排序睡眠队列。

use core::cmp::Ordering;

use crate::arch::timer::get_time_us;
use crate::process::{TaskContext, TaskControlBlock, TaskStatus};
use crate::process::scheduler::processor::{current_task, schedule};
use crate::process::scheduler::runqueue::wake_up_task;
use crate::sync::MPSafeCell;
use alloc::{collections::BinaryHeap, sync::Arc, vec::Vec};
use lazy_static::*;

struct SleepEntry {
	deadline_ns: usize,
	order: usize,
	task: Arc<TaskControlBlock>,
}

impl PartialEq for SleepEntry {
	fn eq(&self, other: &Self) -> bool {
		self.deadline_ns == other.deadline_ns
			&& self.order == other.order
			&& Arc::ptr_eq(&self.task, &other.task)
	}
}

impl Eq for SleepEntry {}

impl Ord for SleepEntry {
	fn cmp(&self, other: &Self) -> Ordering {
		let self_ptr = Arc::as_ptr(&self.task) as usize;
		let other_ptr = Arc::as_ptr(&other.task) as usize;
		other.deadline_ns
			.cmp(&self.deadline_ns)
			.then_with(|| other.order.cmp(&self.order))
			.then_with(|| self_ptr.cmp(&other_ptr))
	}
}

impl PartialOrd for SleepEntry {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

struct SleepQueue {
	inner: BinaryHeap<SleepEntry>,
	enqueue_order: usize,
}

impl SleepQueue {
	fn new() -> Self {
		Self {
			inner: BinaryHeap::new(),
			enqueue_order: 0,
		}
	}

	fn push(&mut self, deadline_ns: usize, task: Arc<TaskControlBlock>) {
		self.inner.retain(|entry| entry.task.gettid() != task.gettid());
		let entry = SleepEntry {
			deadline_ns,
			order: self.enqueue_order,
			task,
		};
		self.enqueue_order = self.enqueue_order.wrapping_add(1);
		self.inner.push(entry);
	}

	fn remove_task(&mut self, tid: usize) {
		self.inner.retain(|entry| entry.task.gettid() != tid);
	}

	fn pop_expired(&mut self, now_ns: usize) -> Option<Arc<TaskControlBlock>> {
		if self.inner.peek().map_or(false, |entry| entry.deadline_ns <= now_ns) {
			self.inner.pop().map(|entry| entry.task)
		} else {
			None
		}
	}

	fn pop_all_expired(&mut self, now_ns: usize) -> Vec<Arc<TaskControlBlock>> {
		let mut tasks = Vec::new();
		while self
			.inner
			.peek()
			.map_or(false, |entry| entry.deadline_ns <= now_ns)
		{
			if let Some(entry) = self.inner.pop() {
				tasks.push(entry.task);
			}
		}
		tasks
	}
}

lazy_static! {
	static ref SLEEP_QUEUE: MPSafeCell<SleepQueue> = MPSafeCell::new(SleepQueue::new());
}

fn monotonic_now_ns() -> usize {
	get_time_us().saturating_mul(1_000)
}

pub(crate) fn register_sleep_task(deadline_ns: usize, task: Arc<TaskControlBlock>) {
	SLEEP_QUEUE.exclusive_access().push(deadline_ns, task);
}

pub(crate) fn cancel_sleep_task(tid: usize) {
	SLEEP_QUEUE.exclusive_access().remove_task(tid);
}

pub fn sleep_current_until(deadline_ns: usize) {
	let task = current_task().unwrap();
	let task_cx_ptr = {
		let mut inner = task.inner_exclusive_access();
		let ptr = &mut inner.thread.task_ctx as *mut TaskContext;
		inner.state = TaskStatus::BlockSaving;
		ptr
	};
	register_sleep_task(deadline_ns, task);
	schedule(task_cx_ptr);
}
// 处理到期任务：锁内一次性收集所有到期任务，锁外统一唤醒
pub fn wake_expired_sleep_tasks() {
	let tasks = {
		let now_ns = monotonic_now_ns();
		SLEEP_QUEUE.exclusive_access().pop_all_expired(now_ns)
	};
	for task in tasks {
		wake_up_task(task);
	}
}