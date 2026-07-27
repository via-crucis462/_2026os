//! Linux stop scheduling class 的每 CPU 状态。

use crate::process::TaskControlBlock;
use alloc::sync::Arc;

/// Linux stop class 不维护普通队列，每 CPU 只有一个 `rq->stop` 任务。
pub struct StopRq {
	/// 绑定到本 CPU 的 stop 内核任务，对应 Linux `rq->stop`。
	stop: Option<Arc<TaskControlBlock>>,
	/// stop 任务当前是否需要运行；其优先级高于所有普通调度类。
	runnable: bool,
}

impl StopRq {
	/// 创建尚未绑定 stop 任务且不可运行的 stop 队列状态。
	pub fn new() -> Self {
		Self { stop: None, runnable: false }
	}

	/// 绑定本 CPU 专用的 stop 任务。
	pub fn set_stop_task(&mut self, task: Arc<TaskControlBlock>) {
		self.stop = Some(task);
	}

	/// 设置 stop 任务是否处于可运行状态。
	pub fn set_runnable(&mut self, runnable: bool) {
		self.runnable = runnable;
	}

	/// stop 任务可运行时返回其引用，否则返回 `None`。
	pub fn pick_next(&self) -> Option<Arc<TaskControlBlock>> {
		self.runnable
			.then(|| self.stop.as_ref().map(Arc::clone))
			.flatten()
	}
}
