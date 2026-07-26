//! Global and per-hart ready queue implementation.

use core::cmp::Ordering;

use crate::{CPU_CORE_NUM, arch::timer::get_time_us, sync::MPSafeCell};
use crate::get_hart_id;
use crate::process::{TaskContext, TaskControlBlock, TaskStatus};
use crate::process::registry::tid2task;
use lazy_static::*;
use alloc::{
	collections::{BinaryHeap, VecDeque},
	sync::Arc,
	vec::Vec,
};

pub const SCHED_OTHER: isize = 0;
pub const SCHED_FIFO: isize = 1;
pub const SCHED_RR: isize = 2;
pub const SCHED_BATCH: isize = 3;
pub const SCHED_IDLE: isize = 5;

const LOCAL_QUEUE_LOW_WATERMARK: usize = 2;
const LOCAL_QUEUE_REFILL_TARGET: usize = 4;

lazy_static! {
	pub static ref SCHEDULER: MPSafeCell<Scheduler> = MPSafeCell::new(Scheduler {
		task_pool: TaskPool::new(),
	});
	pub static ref SCHED_DISPATCH_LOCK: MPSafeCell<()> = MPSafeCell::new(());
	pub static ref TASK_MANAGERS: [MPSafeCell<TaskManager>; CPU_CORE_NUM] = {
		core::array::from_fn(|_| MPSafeCell::new(TaskManager::new()))
	};
}

pub fn lock_dispatch() -> crate::sync::MPSafeGuard<'static, ()> {
	SCHED_DISPATCH_LOCK.exclusive_access()
}

pub struct Scheduler {
	pub task_pool: TaskPool,
}

impl Scheduler {
	pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
		self.task_pool.add_task(task);
	}

	pub fn get_pool(&mut self) -> &mut TaskPool {
		&mut self.task_pool
	}

	pub fn auto_get_task(&mut self) -> VecDeque<Arc<TaskControlBlock>> {
		let mut total_num = self.task_pool.count().saturating_add(1);
		let mut list = VecDeque::new();
		while let Some(task) = self.task_pool.take_a_task() {
			list.push_back(task);
			total_num -= 1;
			if total_num == 0 {
				break;
			}
		}
		list
	}

	pub fn get_task_count(&self) -> usize {
		let mut sum = 0;
		for i in 0..CPU_CORE_NUM {
			sum += TASK_MANAGERS[i].exclusive_access().task_count();
		}
		sum
	}
}

pub struct TaskPool {
	deadline: BinaryHeap<PoolEntry>,
	realtime: BinaryHeap<PoolEntry>,
	fair: BinaryHeap<PoolEntry>,
	idle: BinaryHeap<PoolEntry>,
	enqueue_order: usize,
}

struct PoolEntry {
	class: u8,
	priority: i32,
	order: usize,
	task: Arc<TaskControlBlock>,
}

impl PartialEq for PoolEntry {
	fn eq(&self, other: &Self) -> bool {
		self.class == other.class
			&& self.priority == other.priority
			&& self.order == other.order
			&& Arc::ptr_eq(&self.task, &other.task)
	}
}

impl Eq for PoolEntry {}

impl Ord for PoolEntry {
	fn cmp(&self, other: &Self) -> Ordering {
		let self_ptr = Arc::as_ptr(&self.task) as usize;
		let other_ptr = Arc::as_ptr(&other.task) as usize;
		self.class
			.cmp(&other.class)
			.then_with(|| self.priority.cmp(&other.priority))
			.then_with(|| other.order.cmp(&self.order))
			.then_with(|| self_ptr.cmp(&other_ptr))
	}
}

impl PartialOrd for PoolEntry {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

pub(crate) fn task_sched_rank(task: &Arc<TaskControlBlock>) -> (u8, i32) {
	let inner = task.inner_exclusive_access();
	match inner.sched_policy {
		SCHED_FIFO | SCHED_RR if inner.sched_priority > 0 => (2, inner.sched_priority),
		SCHED_IDLE => (0, 0),
		SCHED_BATCH => (1, 0),
		_ => (1, 0),
	}
}

impl TaskPool {
	pub fn new() -> Self {
		Self {
			deadline: BinaryHeap::new(),
			realtime: BinaryHeap::new(),
			fair: BinaryHeap::new(),
			idle: BinaryHeap::new(),
			enqueue_order: 0,
		}
	}

	pub fn count(&self) -> usize {
		self.deadline.len() + self.realtime.len() + self.fair.len() + self.idle.len()
	}

	pub fn remove_task(&mut self, tid: usize) {
		self.deadline.retain(|entry| entry.task.gettid() != tid);
		self.realtime.retain(|entry| entry.task.gettid() != tid);
		self.fair.retain(|entry| entry.task.gettid() != tid);
		self.idle.retain(|entry| entry.task.gettid() != tid);
	}

	pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
		let tid = task.gettid();
		if self.contains_task(tid) {
			return;
		}
		let (class, priority) = task_sched_rank(&task);
		let entry = PoolEntry {
			class,
			priority,
			order: self.enqueue_order,
			task,
		};
		self.enqueue_order = self.enqueue_order.wrapping_add(1);
		match class {
			2 => self.realtime.push(entry),
			0 => self.idle.push(entry),
			_ => self.fair.push(entry),
		}
	}

	pub fn get_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		tid2task(tid)
	}

	pub fn take_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
		if let Some(task) = tid2task(tid) {
			self.remove_task(tid);
			Some(task)
		} else {
			None
		}
	}

	pub fn take_a_task(&mut self) -> Option<Arc<TaskControlBlock>> {
		self.deadline
			.pop()
			.or_else(|| self.realtime.pop())
			.or_else(|| self.fair.pop())
			.or_else(|| self.idle.pop())
			.map(|entry| entry.task)
	}

	pub fn get_task_list(&self) -> VecDeque<Arc<TaskControlBlock>> {
		self.deadline
			.iter()
			.chain(self.realtime.iter())
			.chain(self.fair.iter())
			.chain(self.idle.iter())
			.map(|entry| Arc::clone(&entry.task))
			.collect()
	}

	fn contains_task(&self, tid: usize) -> bool {
		self.deadline
			.iter()
			.chain(self.realtime.iter())
			.chain(self.fair.iter())
			.chain(self.idle.iter())
			.any(|entry| entry.task.gettid() == tid)
	}
}

struct HeapInode {
	priority: usize,
	order: usize,
	tcb: Arc<TaskControlBlock>,
}

impl PartialEq for HeapInode {
	fn eq(&self, other: &Self) -> bool {
		self.priority == other.priority
			&& self.order == other.order
			&& Arc::ptr_eq(&self.tcb, &other.tcb)
	}
}

impl Eq for HeapInode {}

impl Ord for HeapInode {
	fn cmp(&self, other: &Self) -> Ordering {
		let self_ptr = Arc::as_ptr(&self.tcb) as usize;
		let other_ptr = Arc::as_ptr(&other.tcb) as usize;
		self.priority
			.cmp(&other.priority)
			.then_with(|| other.order.cmp(&self.order))
			.then_with(|| self_ptr.cmp(&other_ptr))
	}
}

impl PartialOrd for HeapInode {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

pub struct TaskManager {
	ready_queue: BinaryHeap<HeapInode>,
	enqueue_order: usize,
}

impl TaskManager {
	pub fn new() -> Self {
		Self {
			ready_queue: BinaryHeap::new(),
			enqueue_order: 0,
		}
	}

	pub fn add(&mut self, task: Arc<TaskControlBlock>) {
		let tid = task.gettid();
		if self.ready_queue.iter().any(|inode| inode.tcb.gettid() == tid) {
			return;
		}
		let (class, priority) = task_sched_rank(&task);
		let priority = (class as usize) * 100 + priority.max(0) as usize;
		let order = self.enqueue_order;
		self.enqueue_order = self.enqueue_order.wrapping_add(1);
		self.ready_queue.push(HeapInode { priority, order, tcb: task });
	}

	pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
		self.ready_queue.pop().map(|inode| inode.tcb)
	}

	pub fn task_count(&self) -> usize {
		self.ready_queue.len()
	}

	pub fn remove(&mut self, tid: usize) {
		self.ready_queue.retain(|inode| inode.tcb.gettid() != tid);
	}
}

pub fn get_current_task_manager() -> &'static MPSafeCell<TaskManager> {
	&TASK_MANAGERS[get_hart_id()]
}

pub fn current_add_tasks() {
	let mut manager = get_current_task_manager().exclusive_access();
	if manager.task_count() >= LOCAL_QUEUE_LOW_WATERMARK {
		return;
	}
	while manager.task_count() < LOCAL_QUEUE_REFILL_TARGET {
		if let Some(task) = ask_for_task() {
			manager.add(task);
		} else {
			break;
		}
	}
}

pub fn add_task_in_current_hart(task: Arc<TaskControlBlock>) {
	let _dispatch = lock_dispatch();
	add_task_in_current_hart_unlocked(task);
}

pub(crate) fn add_task_in_current_hart_unlocked(task: Arc<TaskControlBlock>) {
	remove_task_from_all_local_queues_unlocked(task.gettid());
	remove_task_from_global_pool_unlocked(task.gettid());
	get_current_task_manager().exclusive_access().add(task);
}

pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
	let _dispatch = lock_dispatch();
	current_add_tasks();
	loop {
		let task = get_current_task_manager().exclusive_access().fetch();
		let Some(task) = task else {
			return None;
		};
		let mut task_inner = task.inner_exclusive_access();
		if task_inner.state != TaskStatus::Ready {
			continue;
		}
		task_inner.state = TaskStatus::Running;
		drop(task_inner);
		remove_task_from_all_local_queues_unlocked(task.gettid());
		remove_task_from_global_pool_unlocked(task.gettid());
		return Some(task);
	}
}

pub fn cores_fetch_task() {
	for i in 0..CPU_CORE_NUM {
		let mut manager = TASK_MANAGERS[i].exclusive_access();
		let list = ask_for_tasks();
		for task in list {
			manager.add(task);
		}
	}
}

pub(crate) fn add_task_into_pool_unlocked(task: Arc<TaskControlBlock>) {
	remove_task_from_all_local_queues_unlocked(task.gettid());
	let mut scheduler = SCHEDULER.exclusive_access();
	scheduler.get_pool().remove_task(task.gettid());
	scheduler.get_pool().add_task(task);
}

pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
	let _dispatch = lock_dispatch();
	add_task_into_pool_unlocked(task);
}

pub(crate) fn remove_task_from_global_pool_unlocked(tid: usize) {
	SCHEDULER.exclusive_access().get_pool().remove_task(tid);
}

pub fn remove_task_from_global_pool(tid: usize) {
	let _dispatch = lock_dispatch();
	remove_task_from_global_pool_unlocked(tid);
}

pub fn ask_for_task() -> Option<Arc<TaskControlBlock>> {
	crate::process::scheduler::sleep::wake_expired_sleep_tasks();
	SCHEDULER.exclusive_access().get_pool().take_a_task()
}

pub fn ask_for_tasks() -> VecDeque<Arc<TaskControlBlock>> {
	let mut list = VecDeque::new();
	if let Some(task) = ask_for_task() {
		list.push_back(task);
	}
	list
}

pub fn get_task_count() -> usize {
	let mut sum = 0usize;
	for i in 0..CPU_CORE_NUM {
		sum += TASK_MANAGERS[i].exclusive_access().task_count();
	}
	sum
}

pub fn wake_up_task(task: Arc<TaskControlBlock>) {
	trace!("[kernel] wake_up_task: pid={}", task.getpid());
	let _dispatch = lock_dispatch();
	let mut inner = task.inner_exclusive_access();
	if matches!(inner.state, TaskStatus::Blocked) {
		inner.state = TaskStatus::Ready;
		drop(inner);
		add_task_into_pool_unlocked(task);
	}
}

pub(crate) fn remove_task_from_all_local_queues_unlocked(tid: usize) {
	for hart_id in 0..CPU_CORE_NUM {
		TASK_MANAGERS[hart_id].exclusive_access().remove(tid);
	}
}

pub fn remove_task_from_all_local_queues(tid: usize) {
	let _dispatch = lock_dispatch();
	remove_task_from_all_local_queues_unlocked(tid);
}
