use crate::mm::VirtAddr;
use crate::process::registry::remove_from_tid2task;
use crate::process::registry;
use crate::process::scheduler::{current_task, schedule};
use crate::process::signal::Sigpending;
use crate::process::signal::SignalFlags;
use crate::process::task::{FileDescriptorTable, TaskContext, TaskStatus, TaskStruct};
use crate::process::INITTASK;
use crate::sync::MPSafeCell;
use alloc::sync::Arc;

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 1;

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next(exit_code: i32){
	//println!("[K] PID {} is exiting with code {} ...", current_task().unwrap().process().pid.0, exit_code);
	let task = match current_task() {
		Some(t) => t,
		None => {
			println!("No current task found in exit_current_and_run_next!");
			schedule(&mut TaskContext::zero_init() as *mut _);
			return;
		}
	};
	let pid = task.getpid();
	let (token, clear_child_tid) = {
		let inner = task.inner_exclusive_access();
		let token = inner
			.mm
			.as_ref()
			.map(|mm| mm.exclusive_access().token())
			.unwrap_or(0);
		(token, inner.clear_child_tid)
	};
	crate::syscall::process::clear_child_tid_and_wake(token, clear_child_tid);

	task.recycle_on_exit(exit_code);

	if pid == IDLE_PID {
		println!("[kernel] Idle process exit with exit_code {} ...", exit_code);
		panic!("All applications completed!");
	}

	let last_thread = !registry::TID2TCB
		.exclusive_access()
		.values()
		.any(|other| other.getpid() == pid && other.gettid() != task.gettid());

	if last_thread {
		crate::timer::TIMER_MANAGER.lock().cancel_alarm(pid);
		crate::process::remove_process_posix_timers(pid);

		let (parent, orphan_children) = {
			let mut inner = task.inner_exclusive_access();
			(inner.parent.upgrade(), core::mem::take(&mut inner.children))
		};
		if let Some(parent) = parent {
			let (parent_tgid, signal) = {
				let parent_inner = parent.inner_exclusive_access();
				(parent.gettgid(), parent_inner.signal.clone())
			};
			signal.exclusive_access().insert_pending(SignalFlags::SIGCHLD);

			// SIGCHLD is process-directed. Wake every eligible thread so one
			// unblocked member can consume the shared pending signal.
			let parent_threads: alloc::vec::Vec<_> = registry::TID2TCB
				.exclusive_access()
				.values()
				.filter(|thread| thread.gettgid() == parent_tgid)
				.cloned()
				.collect();
			for thread in parent_threads {
				let mut inner = thread.inner_exclusive_access();
				let deliverable = !inner.blocked.contains(SignalFlags::SIGCHLD);
				if deliverable && matches!(inner.state, TaskStatus::Blocked) {
					inner.signal_interrupted = true;
				}
				drop(inner);
				if deliverable {
					crate::process::wake_up_task(thread);
				}
			}
			// wait4/waitid 的过滤条件各不相同，全部唤醒后由等待者重新检查。
			crate::process::wake_child_exit_waiters(&parent);
		}
		if !orphan_children.is_empty() {
			for child in &orphan_children {
				let mut child_inner = child.inner_exclusive_access();
				child_inner.parent = Arc::downgrade(&INITTASK);
				child_inner.real_parent = Arc::downgrade(&INITTASK);
			}
			INITTASK
				.inner_exclusive_access()
				.children
				.extend(orphan_children);
		}
	}
	drop(task);
	schedule(&mut TaskContext::zero_init() as *mut _);
}

impl TaskStruct {
	/// 线程退出时的清理工作
	pub fn recycle_on_exit(&self, exit_code: i32) {
		remove_from_tid2task(self.gettid());

		let mut inner = self.inner_exclusive_access();
		#[cfg(target_arch = "riscv64")]
		if let Some(mm) = inner.mm.as_ref() {
			//带走自己的内核上下文
			mm.exclusive_access()
				.remove_trap_context_page(VirtAddr::from(inner.thread.trap_ctx));
		}
		// 线程退出时，设置退出码、错误码、状态，并清理资源
		inner.exit_code = exit_code;
		inner.errno = 0;
		inner.state = TaskStatus::Zombie;
		inner.pending = Sigpending::new();
		inner.mm.take();
		let vfork_completion = inner.vfork_completion.take();
		let files = core::mem::replace(
			&mut inner.files,
			Arc::new(MPSafeCell::new(FileDescriptorTable::empty())),
		);
		drop(inner);
		drop(files);
		if let Some(completion) = vfork_completion {
			completion.complete();
		}
	}
}
