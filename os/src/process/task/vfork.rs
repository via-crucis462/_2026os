//! 为 CLONE_VFORK 标志实现的同步原语

use crate::process::current_tid;
use crate::process::scheduler::wait::{
	block_current_and_run_next_if_mp, wake_up_all_mp,
};
use crate::sync::{MPSafeCell, WaitQueue};
use core::sync::atomic::{AtomicBool, Ordering};

/// 用于父进程在 vfork 后阻塞等待子进程完成 exec/exit 的同步结构
/// 
/// 如果 A 进程调用 vfork 创建了 B 进程，则该结构体位于 B 的 TSInner 中，
/// A 进程在 vfork 后调用 wait() 阻塞等待，
/// B 进程在 exec/exit 后调用 complete() 唤醒 A 进程继续运行。
/// 
/// 仅允许单次使用，即父进程在 vfork 后只能等待一个子进程完成 exec/exit。
/// 子进程再次调用 clone 时不继承，而是根据：
/// - vfork: 新建一个，另起一个等待关系
/// - 不带vfork: 不继承，相应字段置为 None
pub struct VforkCompletion {
	// 子进程已经完成了 exec 或 exit，父进程可以继续运行
	done: AtomicBool,
	// 父进程在等待子进程完成 exec/exit 时的等待队列，实际上只有一个父进程在等待
	waiters: MPSafeCell<WaitQueue>,
}

impl VforkCompletion {
	pub fn new() -> Self {
		Self {
			done: AtomicBool::new(false),
			waiters: MPSafeCell::new(WaitQueue::new()),
		}
	}
	/// 开始等待
	/// 
	/// 父进程在执行好 vfork 后调用子进程的此方法
	/// 阻塞等待子进程完成 exec/exit 时由子进程调用 complete() 唤醒
	pub fn wait(&self) {
		while !self.done.load(Ordering::Acquire) {
			// 每次循环先检查 SIGKILL
			let task = crate::process::current_task().unwrap();
			if crate::process::signal::has_pending_sigkill(&task) {
				return;
			}
			block_current_and_run_next_if_mp(&self.waiters, || {
				!self.done.load(Ordering::Acquire)
			});

			// 如果是被别的原因唤醒，重新入队
			if !self.done.load(Ordering::Acquire) {
				self.waiters.exclusive_access().remove_by_tid(current_tid());
			}
		}
	}
	/// 标记 exec 或 exit 完成
	/// 
	/// 子进程在 exit 或 exec 后调用此方法，唤醒父进程继续运行
	pub fn complete(&self) {
		if !self.done.swap(true, Ordering::AcqRel) {
			wake_up_all_mp(&self.waiters);
		}
	}
}
