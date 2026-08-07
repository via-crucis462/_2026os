//! Futex 等待队列管理结构。

use crate::process::{
	current_task, schedule, TaskContext, TaskControlBlock, TaskStatus,
};
use alloc::{collections::{BTreeMap, VecDeque}, sync::Arc, vec::Vec};
use lazy_static::*;
use spin::Mutex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FutexKey {
	address: usize,
	private_token: Option<usize>,
}

impl FutexKey {
	pub(crate) fn shared(physical_address: usize) -> Self {
		Self { address: physical_address, private_token: None }
	}

	pub(crate) fn private(user_token: usize, virtual_address: usize) -> Self {
		Self { address: virtual_address, private_token: Some(user_token) }
	}
}

pub(crate) struct FutexWaiter {
	task: Arc<TaskControlBlock>,
	bitset: u32,
}

pub(crate) struct FutexWaitQueue {
	queue: VecDeque<FutexWaiter>,
}

impl FutexWaitQueue {
	fn new() -> Self {
		Self { queue: VecDeque::new() }
	}

	fn push_back(&mut self, waiter: FutexWaiter) {
		self.queue.push_back(waiter);
	}

	fn pop_matching(&mut self, bitset: u32) -> Option<FutexWaiter> {
		let index = self.queue.iter().position(|waiter| waiter.bitset & bitset != 0)?;
		self.queue.remove(index)
	}

	fn pop_front(&mut self) -> Option<FutexWaiter> {
		self.queue.pop_front()
	}

	fn remove_task(&mut self, tid: usize) {
		self.queue.retain(|waiter| waiter.task.gettid() != tid);
	}

	fn len(&self) -> usize {
		self.queue.len()
	}

	fn tids(&self) -> Vec<usize> {
		self.queue.iter().map(|waiter| waiter.task.gettid()).collect()
	}
}

lazy_static! {
	/// Futex 物理地址到对应等待队列的映射。
	pub(crate) static ref FUTEX_WAIT_QUEUES: Mutex<BTreeMap<FutexKey, Arc<Mutex<FutexWaitQueue>>>> =
		Mutex::new(BTreeMap::new());
}

/// 获取 Futex 地址对应的等待队列；队列不存在时按原逻辑创建。
pub(crate) fn get_futex_wait_queue(key: FutexKey) -> Arc<Mutex<FutexWaitQueue>> {
	let mut queues = FUTEX_WAIT_QUEUES.lock();
	queues
		.entry(key)
		.or_insert_with(|| Arc::new(Mutex::new(FutexWaitQueue::new())))
		.clone()
}

/// 在同一 futex 队列锁下完成用户值复查、入队和阻塞，避免漏掉并发 WAKE。
pub(crate) fn block_current_on_futex_if<F>(
	queue: &Arc<Mutex<FutexWaitQueue>>,
	bitset: u32,
	deadline_ns: Option<usize>,
	should_block: F,
) -> bool
where
	F: FnOnce() -> bool,
{
	let mut guard = queue.lock();
	if !should_block() {
		return false;
	}

	let task = current_task().unwrap();
	let task_cx_ptr = {
		let mut inner = task.inner_exclusive_access();
		let ptr = &mut inner.thread.task_ctx as *mut TaskContext;
		inner.state = TaskStatus::BlockSaving;
		ptr
	};
	if let Some(deadline_ns) = deadline_ns {
		crate::process::scheduler::nanosleep::register_sleep_task(deadline_ns, task.clone());
	}
	guard.push_back(FutexWaiter { task, bitset });
	drop(guard);
	schedule(task_cx_ptr);
	true
}

pub(crate) fn wake_futex_waiters(
	queue: &Arc<Mutex<FutexWaitQueue>>,
	count: usize,
	bitset: u32,
) -> usize {
	let mut waiters = Vec::new();
	{
		let mut guard = queue.lock();
		while waiters.len() < count {
			let Some(waiter) = guard.pop_matching(bitset) else {
				break;
			};
			waiters.push(waiter);
		}
	}

	let mut woken = 0;
	for waiter in waiters {
		let was_blocked = matches!(waiter.task.inner_exclusive_access().state, TaskStatus::Blocked | TaskStatus::BlockSaving);
		crate::process::scheduler::runqueue::wake_up_task(waiter.task);
		if was_blocked {
			woken += 1;
		}
	}
	woken
}

pub(crate) fn requeue_futex_waiters(
	src_queue: &Arc<Mutex<FutexWaitQueue>>,
	dst_queue: Option<&Arc<Mutex<FutexWaitQueue>>>,
	wake_count: usize,
	requeue_count: usize,
) -> usize {
	let mut wake_waiters = Vec::new();
	let mut moved_waiters = Vec::new();

	if let Some(dst_queue) = dst_queue {
		if Arc::ptr_eq(src_queue, dst_queue) {
			let mut src = src_queue.lock();
			for _ in 0..wake_count {
				let Some(waiter) = src.pop_front() else { break; };
				wake_waiters.push(waiter);
			}
			for _ in 0..requeue_count {
				let Some(waiter) = src.pop_front() else { break; };
				src.push_back(waiter);
				moved_waiters.push(());
			}
		} else if Arc::as_ptr(src_queue) as usize <= Arc::as_ptr(dst_queue) as usize {
			let mut src = src_queue.lock();
			let mut dst = dst_queue.lock();
			for _ in 0..wake_count {
				let Some(waiter) = src.pop_front() else { break; };
				wake_waiters.push(waiter);
			}
			for _ in 0..requeue_count {
				let Some(waiter) = src.pop_front() else { break; };
				dst.push_back(waiter);
				moved_waiters.push(());
			}
		} else {
			let mut dst = dst_queue.lock();
			let mut src = src_queue.lock();
			for _ in 0..wake_count {
				let Some(waiter) = src.pop_front() else { break; };
				wake_waiters.push(waiter);
			}
			for _ in 0..requeue_count {
				let Some(waiter) = src.pop_front() else { break; };
				dst.push_back(waiter);
				moved_waiters.push(());
			}
		}
	} else {
		let mut src = src_queue.lock();
		for _ in 0..wake_count {
			let Some(waiter) = src.pop_front() else { break; };
			wake_waiters.push(waiter);
		}
	}

	let woken = wake_waiters.len();
	for waiter in wake_waiters {
		crate::process::scheduler::runqueue::wake_up_task(waiter.task);
	}
	woken + moved_waiters.len()
}

pub(crate) fn remove_futex_waiter(tid: usize) {
	let queues: Vec<_> = FUTEX_WAIT_QUEUES.lock().values().cloned().collect();
	for queue in queues {
		queue.lock().remove_task(tid);
	}
}

/// 调试用：打印所有 futex 等待队列的长度。
pub fn debug_print_futex_queues() {
	let queues = FUTEX_WAIT_QUEUES.lock();
	if queues.is_empty() {
		println!("[FUTEX-DBG] no futex queues");
		return;
	}
	for (key, queue) in queues.iter() {
		let guard = queue.lock();
		let len = guard.len();
		if len > 0 {
			let tids = guard.tids();
			println!(
				"[FUTEX-DBG] key={:?} waiters={} tids={:?}",
				key, len, tids
			);
		}
	}
}
