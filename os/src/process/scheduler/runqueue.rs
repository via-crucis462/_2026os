//! Linux 风格的每 CPU 总运行队列。
//!
//! 各调度类的内部数据结构分别位于 `stopRq.rs`、`deadlineRq`、
//! `rtRq`、`cfsRq` 和 `itRq`；本文件只负责聚合及调度类顺序。

use crate::process::scheduler::{CfsRq, DeadlineRq, IdleRq, RtRq, StopRq, CPU_NUM};
use crate::process::{TaskControlBlock, TaskStatus};
use crate::sync::{MPSafeCell, MPSafeGuard};
use crate::get_hart_id;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::lazy;
use lazy_static::*;

pub const SCHED_OTHER: isize = 0;
pub const SCHED_FIFO: isize = 1;
pub const SCHED_RR: isize = 2;
pub const SCHED_BATCH: isize = 3;
pub const SCHED_IDLE: isize = 5;
pub const SCHED_DEADLINE: isize = 6;

/// Published state of a CPU while it has no current user task.
///
/// The exact scheduler-class queues remain under `Rq::inner`.  This small
/// atomic view is deliberately separate so a remote waker can choose a CPU
/// without serialising every wakeup on a remote rq lock.
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdleState {
	Running = 0,
	IdlePolling = 1,
	IdleWfi = 2,
}

impl IdleState {
	fn from_raw(raw: usize) -> Self {
		match raw {
			1 => Self::IdlePolling,
			2 => Self::IdleWfi,
			_ => Self::Running,
		}
	}
}

/// Lock-free information used only for CPU selection and idle notification.
///
/// `runnable_*` is an advisory placement signal: it includes tasks in the
/// exact rq plus the current task only while that CPU is executing user work.
/// The rq lock remains the authority for dequeue/enqueue and task transitions.
pub struct RqView {
	runnable_count: AtomicUsize,
	runnable_weight: AtomicUsize,
	wake_reservations: AtomicUsize,
	idle_state: AtomicUsize,
	online: AtomicBool,
	ipi_pending: AtomicBool,
}

impl RqView {
	const fn new() -> Self {
		Self {
			runnable_count: AtomicUsize::new(0),
			runnable_weight: AtomicUsize::new(0),
			wake_reservations: AtomicUsize::new(0),
			idle_state: AtomicUsize::new(IdleState::Running as usize),
			online: AtomicBool::new(false),
			ipi_pending: AtomicBool::new(false),
		}
	}

	pub(crate) fn snapshot(&self) -> RqSnapshot {
		RqSnapshot {
			runnable_count: self.runnable_count.load(Ordering::Acquire),
			runnable_weight: self.runnable_weight.load(Ordering::Acquire),
			wake_reservations: self.wake_reservations.load(Ordering::Acquire),
			idle_state: IdleState::from_raw(self.idle_state.load(Ordering::Acquire)),
			online: self.online.load(Ordering::Acquire),
		}
	}

	fn add_runnable(&self, weight: usize) {
		self.runnable_count.fetch_add(1, Ordering::Release);
		self.runnable_weight.fetch_add(weight, Ordering::Release);
	}

	fn remove_runnable(&self, weight: usize) {
		self.runnable_count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
			Some(count.saturating_sub(1))
		}).ok();
		self.runnable_weight.fetch_update(Ordering::AcqRel, Ordering::Acquire, |load| {
			Some(load.saturating_sub(weight))
		}).ok();
	}

	fn consume_wakeup_reservation(&self) {
		self.wake_reservations.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
			Some(count.saturating_sub(1))
		}).ok();
	}

	fn try_reserve_wakeup(&self, expected: usize) -> bool {
		self.wake_reservations
			.compare_exchange(
				expected,
				expected.saturating_add(1),
				Ordering::AcqRel,
				Ordering::Acquire,
			)
			.is_ok()
	}
}

#[derive(Clone, Copy)]
pub(crate) struct RqSnapshot {
	pub(crate) runnable_count: usize,
	pub(crate) runnable_weight: usize,
	pub(crate) wake_reservations: usize,
	pub(crate) idle_state: IdleState,
	pub(crate) online: bool,
}

#[derive(Clone, Copy)]
struct EnqueuedTask {
	weight: usize,
}

#[derive(Clone, Copy)]
struct CpuSelection {
	cpu_id: usize,
	reserved: bool,
}

pub(crate) enum StealResult {
	Stolen(Arc<TaskControlBlock>, usize),
	Dropped(usize),
	None,
}

/// Returns a stolen task and the weight that must move from the source CPU's
/// published view to the destination CPU's current-task contribution.
pub(crate) fn steal_task_from_cpu(source_cpu: usize, target_cpu: usize) -> StealResult {
	if target_cpu >= RQ_ARRAY.len() {
		return StealResult::None;
	}
	let Some(source_rq) = RQ_ARRAY.get(source_cpu) else {
		return StealResult::None;
	};
	let result = source_rq.inner_exclusive_access().steal_task(target_cpu);
	match result {
		StealResult::Stolen(task, weight) => {
			if source_cpu != target_cpu {
				source_rq.view.remove_runnable(weight);
				RQ_ARRAY[target_cpu].view.add_runnable(weight);
			}
			mark_resched_and_kick(target_cpu, local_cpu());
			StealResult::Stolen(task, weight)
		}
		StealResult::Dropped(weight) => {
			source_rq.view.remove_runnable(weight);
			StealResult::Dropped(weight)
		}
		StealResult::None => StealResult::None,
	}
}

#[inline]
fn task_weight_from_value(weight: u64) -> usize {
	weight.max(1).min(usize::MAX as u64) as usize
}

#[inline]
fn cpu_allowed(mask: usize, cpu_id: usize) -> bool {
	cpu_id < usize::BITS as usize && mask & (1usize << cpu_id) != 0
}

// 核的调度队列与核id
pub struct Rq {
	pub inner: MPSafeCell<Rqinner>,
	pub view: RqView,
	/// Serializes the final idle-state check with a scheduler IPI write.
	///
	/// A receiver closes this gate before clearing pending IPI actions.  Once
	/// it observes `Running` under the gate, no sender can still be between an
	/// `IdleWfi` observation and its architecture IPI write.
	idle_ipi_gate: MPSafeCell<()>,
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
		Self {
			inner: MPSafeCell::new(Rqinner::new()),
			view: RqView::new(),
			idle_ipi_gate: MPSafeCell::new(()),
			cpu_id,
		}
	}
}
/// 每 CPU 运行队列，对应 Linux `struct rq` 的调度类核心部分。
/// 调度类优先级固定为 stop > deadline > rt > cfs > idle。
pub struct Rqinner {
	/// 最高优先级的每 CPU stop 调度类状态。
	pub stop: StopRq,
	/// SCHED_DEADLINE 的每 CPU 运行队列。
	pub deadline: DeadlineRq,
	/// 普通、批处理及 SCHED_IDLE 用户任务使用的 CFS 运行队列。
	pub cfs: CfsRq,
	/// SCHED_FIFO 和 SCHED_RR 使用的实时运行队列。
	pub rt: RtRq,
	/// 最低优先级的每 CPU idle 任务状态。
	pub idle: IdleRq,
	/// 除 per-CPU idle 任务外的可运行任务总数。
	pub nr_running: usize,
}

impl Rqinner {
	/// 创建五个调度类均为空的每 CPU 总运行队列。
	pub fn new() -> Self {
		Self {
			stop: StopRq::new(),
			deadline: DeadlineRq::new(),
			cfs: CfsRq::new(),
			rt: RtRq::new(),
			idle: IdleRq::new(),
			nr_running: 0,
		}
	}

	/// 按 Linux 调度类优先级依次选择任务，且不执行出队。
	///
	/// 固定顺序为 stop → deadline → rt → cfs → idle。
	pub fn pick_next_task(&self) -> Option<Arc<TaskControlBlock>> {
		self.stop
			.pick_next()
			.or_else(|| self.deadline.pick_next())
			.or_else(|| self.rt.pick_next())
			.or_else(|| self.cfs.pick_next())
			.or_else(|| self.idle.pick_next())
	}

	/// 按任务策略入队并更新本 CPU 的可运行任务计数。
	fn enqueue_task(&mut self, task: Arc<TaskControlBlock>) -> Option<EnqueuedTask> {
		let (sched_policy, prio, vruntime, load_weight, absolute_deadline) = {
			let mut inner = task.inner_exclusive_access();
			if inner.on_rq {
				return None;
			}
			if inner.state == TaskStatus::Zombie {
				inner.on_rq = false;
				return None;
			}
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
		self.nr_running += 1;
		match sched_policy {
			SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => {
				self.cfs.enqueue(task, vruntime, load_weight)
			}
			SCHED_FIFO | SCHED_RR => {
				let priority = prio.max(0) as usize;
				self.rt.enqueue(task, priority.min(99), sched_policy == SCHED_RR);
			}
			SCHED_DEADLINE => self.deadline.enqueue(task, absolute_deadline),
			_ => panic!("Unsupported scheduling policy"),
		}
		Some(EnqueuedTask { weight: task_weight_from_value(load_weight) })
	}

	/// 从最高可用普通调度类中移除一个任务。
	pub(crate) fn pop_next_task(&mut self) -> Option<Arc<TaskControlBlock>> {
		let task = self.deadline.pop_next()
			.or_else(|| self.rt.pop_next())
			.or_else(|| self.cfs.pop_next());
		if task.is_some() {
			self.nr_running = self.nr_running.saturating_sub(1);
		}
		task
	}

	/// 从本运行队列窃取任务，并把任务的归属 CPU 更新为目标 CPU。
	pub(crate) fn steal_task(&mut self, target_cpu: usize) -> StealResult {
		let Some(task) = self.pop_next_task() else {
			return StealResult::None;
		};
		let (can_move, weight) = {
			let mut inner = task.inner_exclusive_access();
			inner.on_rq = false;
			if inner.state == TaskStatus::Zombie {
				return StealResult::Dropped(task_weight_from_value(inner.se.load_weight));
			}
			let can_move = !inner.on_main_hart
				&& cpu_allowed(inner.cpus_allowed, target_cpu)
				&& inner.rt.migratable;
			if can_move {
				inner.cpu = target_cpu;
			}
			(can_move, task_weight_from_value(inner.se.load_weight))
		};
		if can_move {
			StealResult::Stolen(task, weight)
		} else {
			let _ = self.enqueue_task(task);
			StealResult::None
		}
	}

	fn remove_task(&mut self, tid: usize) -> Option<usize> {
		let task = self.deadline.remove_task(tid)
			.or_else(|| self.rt.remove_task(tid))
			.or_else(|| self.cfs.remove_task(tid));
		if let Some(task) = task {
			self.nr_running = self.nr_running.saturating_sub(1);
			let mut inner = task.inner_exclusive_access();
			inner.on_rq = false;
			Some(task_weight_from_value(inner.se.load_weight))
		} else {
			None
		}
	}
}

/// 将任务加入指定 CPU 的本地运行队列。
///
/// This is the exact enqueue transaction.  Callers that make a task newly
/// runnable must use `enqueue_new_runnable_task`; current tasks returning to
/// Ready already contribute to their CPU's published runnable load.
fn enqueue_task_on_cpu(task: Arc<TaskControlBlock>, cpu_id: usize, is_new_runnable: bool) -> bool {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return false;
	};
	let enqueue = {
		let mut inner = task.inner_exclusive_access();
		if inner.state == TaskStatus::Zombie {
			inner.on_rq = false;
			inner.on_cpu = false;
			return false;
		}
		inner.cpu = cpu_id;
		inner.state = TaskStatus::Ready;
		inner.on_cpu = false;
		is_new_runnable.then_some(task_weight_from_value(inner.se.load_weight))
	};
	let enqueued = RQ_ARRAY[cpu_id].inner_exclusive_access().enqueue_task(task);
	if let Some(enqueued) = enqueued {
		if let Some(weight) = enqueue {
			debug_assert_eq!(weight, enqueued.weight);
			RQ_ARRAY[cpu_id].view.add_runnable(enqueued.weight);
		}
		return true;
	}
	false
}

fn normalize_cpu(cpu_id: usize) -> Option<usize> {
	(cpu_id < RQ_ARRAY.len()).then_some(cpu_id)
}

fn local_cpu() -> usize {
	get_hart_id().min(RQ_ARRAY.len().saturating_sub(1))
}

fn allowed_fallback(allowed: usize, fallback_cpu: usize) -> usize {
	if cpu_allowed(allowed, fallback_cpu) && fallback_cpu < RQ_ARRAY.len() {
		return fallback_cpu;
	}
	(0..RQ_ARRAY.len())
		.find(|&cpu_id| cpu_allowed(allowed, cpu_id))
		.unwrap_or_else(|| fallback_cpu.min(RQ_ARRAY.len().saturating_sub(1)))
}

/// Select an allowed CPU from lock-free published rq state.
///
/// A waker reserves the selected CPU before it takes the rq lock.  That
/// reservation is charged in the next selector's score and consumed by the
/// actual enqueue path, preventing a futex wake batch from collapsing onto
/// the same observed-idle CPU.
fn select_task_rq(task: &Arc<TaskControlBlock>, source_cpu: usize, prefer_cpu: usize) -> CpuSelection {
	let (allowed, on_main_hart, task_weight) = {
		let inner = task.inner_exclusive_access();
		(
			inner.cpus_allowed,
			inner.on_main_hart,
			task_weight_from_value(inner.se.load_weight),
		)
	};
	let preferred_cpu = if cpu_allowed(allowed, prefer_cpu) && prefer_cpu < RQ_ARRAY.len() {
		prefer_cpu
	} else {
		allowed_fallback(allowed, source_cpu)
	};
	let fallback = allowed_fallback(allowed, preferred_cpu);
	if on_main_hart || RQ_ARRAY.len() <= 1 {
		return CpuSelection { cpu_id: fallback, reserved: false };
	}

	let mut reservation_retries = 0usize;
	loop {
		let mut best_idle = None;
		let mut best_loaded = None;
		for cpu_id in 0..RQ_ARRAY.len() {
			if !cpu_allowed(allowed, cpu_id) {
				continue;
			}
			let snapshot = RQ_ARRAY[cpu_id].view.snapshot();
			if !snapshot.online {
				continue;
			}
			let score = snapshot
				.runnable_weight
				.saturating_add(snapshot.wake_reservations.saturating_mul(task_weight));
			let idle = matches!(snapshot.idle_state, IdleState::IdlePolling | IdleState::IdleWfi)
				&& snapshot.runnable_count == 0;
			// Preserve the prior CPU on exact ties.  It keeps cache affinity
			// without overriding an actual load or idle-state advantage.
			let affinity_penalty = usize::from(cpu_id != preferred_cpu);
			let candidate = (score, affinity_penalty, cpu_id, snapshot.wake_reservations);
			if idle {
				if best_idle.map_or(true, |current: (usize, usize, usize, usize)| candidate < current) {
					best_idle = Some(candidate);
				}
			} else if best_loaded.map_or(true, |current: (usize, usize, usize, usize)| candidate < current) {
				best_loaded = Some(candidate);
			}
		}

		let Some((_, _, selected_cpu, observed_reservations)) = best_idle.or(best_loaded) else {
			return CpuSelection { cpu_id: fallback, reserved: false };
		};
		if RQ_ARRAY[selected_cpu]
			.view
			.try_reserve_wakeup(observed_reservations)
		{
			return CpuSelection { cpu_id: selected_cpu, reserved: true };
		}
		reservation_retries = reservation_retries.saturating_add(1);
		if reservation_retries >= RQ_ARRAY.len().saturating_mul(2).max(1) {
			return CpuSelection { cpu_id: fallback, reserved: false };
		}
	}
}

fn mark_resched_and_kick(cpu_id: usize, source_cpu: usize) {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return;
	};
	let view = &RQ_ARRAY[cpu_id].view;
	if cpu_id == source_cpu {
		return;
	}

	// `idle_state` is advisory during CPU selection, but authoritative for
	// sending an IPI only while this gate is held.  The idle CPU takes the same
	// gate, publishes Running, and then clears the hardware action.  Therefore
	// no IPI write that observed IdleWfi can finish after that clear.
	let _gate = RQ_ARRAY[cpu_id].idle_ipi_gate.exclusive_access();
	if view.idle_state.load(Ordering::Acquire) == IdleState::IdleWfi as usize
		&& !view.ipi_pending.swap(true, Ordering::SeqCst)
	{
		crate::arch::ipi::send_scheduler_ipi(cpu_id);
	}
}

fn enqueue_selected_task(
	task: Arc<TaskControlBlock>,
	selection: CpuSelection,
	source_cpu: usize,
	is_new_runnable: bool,
) -> bool {
	let target_cpu = selection.cpu_id;
	let enqueued = enqueue_task_on_cpu(task, target_cpu, is_new_runnable);
	if selection.reserved {
		RQ_ARRAY[target_cpu].view.consume_wakeup_reservation();
	}
	if enqueued {
		mark_resched_and_kick(target_cpu, source_cpu);
	}
	enqueued
}

/// A task starts as runnable for the first time, so it receives a new load
/// contribution and follows the same placement policy as a stable wakeup.
pub fn enqueue_new_task(task: Arc<TaskControlBlock>, parent_cpu: usize) {
	let source_cpu = local_cpu();
	let selection = select_task_rq(&task, source_cpu, parent_cpu);
	let _ = enqueue_selected_task(task, selection, source_cpu, true);
}

/// Place a task that has just become runnable after a stable wakeup.  The
/// recorded source is a placement hint; the physical IPI is always sent from
/// the CPU actually performing this enqueue.
pub fn enqueue_woken_task(task: Arc<TaskControlBlock>, wake_source_cpu: usize) -> bool {
	let local_cpu = local_cpu();
	let prefer_cpu = task.inner_exclusive_access().cpu;
	let selection = select_task_rq(&task, wake_source_cpu, prefer_cpu);
	enqueue_selected_task(task, selection, local_cpu, true)
}

/// Complete a wake that raced with `BlockSaving`.
///
/// The task never reached stable `Blocked`, so its old CPU still owns one
/// published runnable contribution.  Requeue it without adding another one;
/// only transfer that contribution when placement moves it to a new CPU.
pub fn enqueue_resumed_task(task: Arc<TaskControlBlock>, wake_source_cpu: usize) -> bool {
	let (old_cpu, weight) = {
		let inner = task.inner_exclusive_access();
		(inner.cpu, task_weight_from_value(inner.se.load_weight))
	};
	let local_cpu = local_cpu();
	let selection = select_task_rq(&task, wake_source_cpu, old_cpu);
	let target_cpu = selection.cpu_id;
	let enqueued = enqueue_task_on_cpu(task, target_cpu, false);
	if selection.reserved {
		RQ_ARRAY[target_cpu].view.consume_wakeup_reservation();
	}
	if !enqueued {
		return false;
	}
	if old_cpu != target_cpu {
		if let Some(old_cpu) = normalize_cpu(old_cpu) {
			RQ_ARRAY[old_cpu].view.remove_runnable(weight);
		}
		RQ_ARRAY[target_cpu].view.add_runnable(weight);
	}
	mark_resched_and_kick(target_cpu, local_cpu);
	true
}

/// Requeue the currently running task on its own CPU.  Its runnable load was
/// retained while it was executing, so this only changes exact queue state.
pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
	let cpu_id = task.inner_exclusive_access().cpu;
	let cpu_id = normalize_cpu(cpu_id).unwrap_or_else(local_cpu);
	let _ = enqueue_task_on_cpu(task, cpu_id, false);
}

/// 兼容旧调用者；新框架不再需要全局 dispatch lock。
pub(crate) fn add_task_into_pool_unlocked(task: Arc<TaskControlBlock>) {
	add_task_into_pool(task);
}

pub(crate) fn remove_task_from_all_local_queues_unlocked(tid: usize) {
	for rq in RQ_ARRAY.iter() {
		let removed_weight = rq.inner_exclusive_access().remove_task(tid);
		if let Some(weight) = removed_weight {
			rq.view.remove_runnable(weight);
			break;
		}
	}
}

/// 新框架没有全局任务池，保留为空操作以兼容迁移中的清理路径。
pub(crate) fn remove_task_from_global_pool_unlocked(_tid: usize) {}

/// 当前 CFS 实体不再可运行时，推进对应 CPU 的最小虚拟运行时间。
pub(crate) fn advance_cfs_min_vruntime(cpu_id: usize) {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return;
	};
	RQ_ARRAY[cpu_id].inner_exclusive_access().cfs.advance_min_vruntime();
}

/// 唤醒阻塞任务，并重新加入它原先所属 CPU 的运行队列。
/// 如果任务不是阻塞状态，则打印警告信息并忽略。
/// 返回是否真的已入队；对正在切换（BlockSaving）的任务只挂起
/// 唤醒请求，由调度器在切换完成后入队，绝不自旋等待。
pub fn wake_up_task(task: Arc<TaskControlBlock>) -> bool {
	let source_cpu = get_hart_id();
	let prefer_cpu = {
		let mut inner = task.inner_exclusive_access();
		if matches!(inner.state, TaskStatus::Blocked) {
			inner.state = TaskStatus::Ready;
			Some(inner.cpu)
		} else if inner.state == TaskStatus::BlockSaving {
			inner.wake_pending = true;
			inner.wake_source_cpu = Some(source_cpu);
			None
		} else {
			None
		}
	};
	if prefer_cpu.is_some() {
		return enqueue_woken_task(task, source_cpu);
	}
	false
}

/// 从当前 CPU 自己的运行队列获取任务，不执行跨核窃取。
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
	let cpu_id = get_hart_id();
	let rq = RQ_ARRAY.get(cpu_id)?;
	loop {
		let task = rq.inner_exclusive_access().pop_next_task()?;
		{
			let mut inner = task.inner_exclusive_access();
			inner.on_rq = false;
			if inner.state == TaskStatus::Zombie {
				inner.on_cpu = false;
				rq.view.remove_runnable(task_weight_from_value(inner.se.load_weight));
				continue;
			}
			inner.cpu = cpu_id;
			inner.on_cpu = true;
			inner.state = TaskStatus::Running;
			inner.need_resched = false;
		}
		return Some(task);
	}
}

/// Mark a hart as available for placement after its scheduler loop starts.
pub fn scheduler_cpu_online(cpu_id: usize) {
	if let Some(cpu_id) = normalize_cpu(cpu_id) {
		RQ_ARRAY[cpu_id].view.online.store(true, Ordering::Release);
	}
}

/// Publish the transition out of idle before running or stealing work.
pub fn scheduler_cpu_running(cpu_id: usize) {
	if let Some(cpu_id) = normalize_cpu(cpu_id) {
		RQ_ARRAY[cpu_id]
			.view
			.idle_state
			.store(IdleState::Running as usize, Ordering::Release);
	}
}

/// Publish that the CPU is considering entering its architecture idle state.
pub fn scheduler_idle_prepare(cpu_id: usize) {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return;
	};
	let view = &RQ_ARRAY[cpu_id].view;
	view.idle_state
		.store(IdleState::IdlePolling as usize, Ordering::Release);
}

/// Arm the idle IPI gate only after checking the exact local runqueue.
///
/// The receiver holds the gate while it checks `nr_running` under the rq lock.
/// A producer publishes its rq entry before taking this same gate, so either
/// the receiver sees the task and does not sleep, or the producer observes
/// `IdleWfi` and sends the wakeup IPI.
pub fn scheduler_idle_arm(cpu_id: usize) -> bool {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return false;
	};
	let rq = &RQ_ARRAY[cpu_id];
	let _gate = rq.idle_ipi_gate.exclusive_access();
	let empty = rq.inner_exclusive_access().nr_running == 0;
	if empty {
		rq.view.idle_state
			.store(IdleState::IdleWfi as usize, Ordering::Release);
	} else {
		rq.view.idle_state
			.store(IdleState::Running as usize, Ordering::Release);
	}
	empty
}

/// Close the idle IPI gate and acknowledge all hardware notification state.
///
/// The gate makes the ordering stronger than an atomic state store alone:
/// after `Running` is published, every sender that previously observed
/// `IdleWfi` has completed its IPI write before the acknowledgement below.
pub fn scheduler_idle_exit(cpu_id: usize) {
	let Some(cpu_id) = normalize_cpu(cpu_id) else {
		return;
	};
	let rq = &RQ_ARRAY[cpu_id];
	let _gate = rq.idle_ipi_gate.exclusive_access();
	rq.view
		.idle_state
		.store(IdleState::Running as usize, Ordering::Release);
	crate::arch::ipi::acknowledge_scheduler_ipi();
	rq.view.ipi_pending.store(false, Ordering::Release);
}

/// Remove the current task's contribution when it becomes blocked or exits.
pub fn scheduler_task_stopped(cpu_id: usize, weight: u64) {
	if let Some(cpu_id) = normalize_cpu(cpu_id) {
		RQ_ARRAY[cpu_id]
			.view
			.remove_runnable(task_weight_from_value(weight));
	}
}
