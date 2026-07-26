pub mod context;
pub mod tcb;
pub mod status;
pub mod cred;
pub mod fs;
pub mod limits;
pub mod files;
pub mod clone;
pub mod exec;
pub mod exit;

pub use tcb::*;
pub use context::*;
pub use status::*;
pub use cred::*;
pub use fs::*;
pub use limits::*;
pub use files::*;

// Explicit compatibility surface for legacy `crate::task::*` callers.
pub use crate::process::{
	add_initproc, add_task, block_current_and_run_next,
	current_add_signal, current_task, current_tid, current_trap_cx,
	current_user_token, exit_current_and_run_next, get_process, handle_signals,
	kstack_alloc, lock_dispatch, run_tasks, suspend_current_and_run_next,
	wake_up_one, KernelStack, SignalAction, SignalActions, SignalFlags, MAX_SIG,
};
pub(crate) use crate::process::add_task_into_pool_unlocked;