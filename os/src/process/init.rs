use crate::arch::{
	timer::get_time_us,
	trap::{trap_handler, TrapContext},
};
use crate::fs::ROOT_DENTRY;
use crate::ipc::namespace::{IPCNamespace, NsProxy};
use crate::mm::{KERNEL_SPACE, MemorySet};
use crate::process::scheduler::runqueue::{SCHED_IDLE, SCHED_OTHER};
use crate::process::signal::{SigHand, Signal, SignalAltStack, Sigpending, SignalFlags};
use crate::process::task::{
	context::ThreadStruct, Cred, FileDescriptorTable, FsStruct, TaskContext,
	SchedDlEntity, SchedEntity, SchedRtEntity, TaskControlBlock, TaskStatus,
	TaskStruct, TaskStructInner,
};
use crate::process::{add_task, kstack_alloc, pid_alloc};
use crate::sync::MPSafeCell;
use alloc::{string::String, sync::{Arc, Weak}, vec::Vec};
use core::sync::atomic::AtomicBool;
use lazy_static::*;

impl TaskStruct {
	 pub fn init_proc(elf_data: &[u8]) -> Arc<TaskControlBlock> {
		//println!("[kernel] TaskControlBlock::new: start creating a new process");

		//处理 ELF 文件，创建内存空间，返回的memory_set中已经包含了用户程序的代码段、数据段、bss段以及长度为1的堆段
		let Some((memory_set, heap_bottom, user_sp, entry_point, _main_entry, _phdr, _phnum, _phent, _interp_base))
			= MemorySet::from_elf(elf_data) else {
				panic!("TaskControlBlock::new: invalid ELF for init process");
			};
		debug!(
			"TaskControlBlock::new: entry_point={:#x}",
			entry_point
		);
        
		//pid ，tid 和内核栈的分配
		let pid_handle = Arc::new(pid_alloc());
		//println!("[kernel] TaskControlBlock::new: allocated PID {}", pid_handle.0);
		//let tid_handle = Arc::new(tid_from_pid(pid_handle.0));
		//println!("[kernel] TaskControlBlock::new: allocated TID {}", tid_handle.0);
		let kernel_stack = kstack_alloc();
        
		let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
		let kernel_stack_top = trap_cx_addr;
		let initial_user_sp = user_sp;
		debug!("TaskControlBlock::new: kernel_stack_top={:#x}", kernel_stack.get_top());
		// 进程控制块

		// 为pcb创建主线程
		let task_control_block = Arc::new_cyclic(|task_weak| TaskStruct {
			pid: pid_handle.clone(),
			tgid: pid_handle.clone(),
			group_leader: task_weak.clone(),
			inner: MPSafeCell::new(TaskStructInner {
				on_main_hart: false,
				nsproxy: Arc::new(NsProxy::new(IPCNamespace::new())),
				thread: ThreadStruct {
					trap_ctx: trap_cx_addr,
					task_ctx: TaskContext::goto_trap_return(kernel_stack_top),
				},
				group_leader: task_weak.clone(),
				kernel_stack,
				real_parent: Weak::new(),
				parent: Weak::new(),
				children: Vec::new(),
				pgid: pid_handle.0,
				sid: pid_handle.0,
				state: TaskStatus::Ready,
				exit_state: 0,
				exit_code: 0,
				exit_signal: 0,
				flags: 0,
				errno: 0,
				oom_score_adj: 0,
				sched_policy: SCHED_IDLE, // initproc默认用SCHED_IDLE策略
				sched_priority: 0,
				prio: 120,
				static_prio: 120,
				normal_prio: 120,
				se: SchedEntity::new(),
				rt: SchedRtEntity::new(),
				dl: SchedDlEntity::new(),
				mm: Some(Arc::new(memory_set)),
				fs: Arc::new(MPSafeCell::new(FsStruct::new(ROOT_DENTRY.clone(), ROOT_DENTRY.clone()))),
				files: Arc::new(MPSafeCell::new(FileDescriptorTable::new())),
				exe_path: String::from("/initproc"),
				signal: Arc::new(MPSafeCell::new(Signal::new())),
				exec_update_lock: Arc::new(AtomicBool::new(false)),
				signal_hand: Arc::new(MPSafeCell::new(SigHand::new())),
				blocked: SignalFlags::empty(),
				pending: Sigpending::new(),
				signal_interrupted: false,
				sigsuspend_saved_mask: None,
				signal_mask_backup: Vec::new(),
				trap_ctx_backup: Vec::new(),
				signal_user_context_backup: Vec::new(),
				signal_alt_stack: SignalAltStack::default(),
				term_signal: None,
				frozen: false,
				cred: Arc::new(MPSafeCell::new(Cred::new(0, 0, 0, 0, 0, 0, 0, 0))),
				real_cred: Arc::new(MPSafeCell::new(Cred::new(0, 0, 0, 0, 0, 0, 0, 0))),
				start_time: get_time_us() as u64,
				start_boottime: get_time_us() as u64,
				on_cpu: false,
				on_rq: false,
				cpu: 0,
				cpus_allowed: if crate::arch::config::CPU_CORE_NUM >= usize::BITS as usize {
					usize::MAX
				} else {
					(1usize << crate::arch::config::CPU_CORE_NUM) - 1
				},
				need_resched: false,
				clear_child_tid: 0,
				vfork_completion: None,
				personality: 0,
				locked_bytes: 0,
				comm: [0; 10],
			}),
		});
        
		// prepare TrapContext in user space
		let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
		// 发现问题：这样解引用写入会炸
		// 已解决：只分配了256MB内存，之前的实现写到了有效区之外
		*trap_cx = TrapContext::app_init_context(
			entry_point,
			initial_user_sp,
			KERNEL_SPACE.token(),
			kernel_stack_top,
			trap_handler as *const () as usize,
		);
		debug!("TaskControlBlock::new: finished creating a new process");
		//println!("[kernel] TaskControlBlock::new: created init process with PID {}, main thread TID {}, entry_point={:#x}", proc_control_block.getpid(), task_control_block.gettid(), entry_point);
		// 返回PCB和主线程
		task_control_block
	}
}

#[repr(C)]
struct InitProcData<T: ?Sized> {
	pub _align: [u64; 0],
	pub bytes: T,
}

#[link_section = ".data"]
#[cfg(target_arch = "riscv64")]
static INITPROC_DATA: &'static InitProcData<[u8]> = &InitProcData {
	_align: [],
	#[cfg(initproc = "default")]
	bytes: *include_bytes!("../arch/riscv/initproc"),
	#[cfg(initproc = "sh")]
	bytes: *include_bytes!("../arch/riscv/initproc_sh"),
	#[cfg(initproc = "ltp")]
	bytes: *include_bytes!("../arch/riscv/initproc_ltp")
};

#[link_section = ".data"]
#[cfg(target_arch = "loongarch64")]
static INITPROC_DATA: &'static InitProcData<[u8]> = &InitProcData {
	_align: [],
	#[cfg(initproc = "default")]
	bytes: *include_bytes!("../arch/la/initproc"),
	#[cfg(initproc = "sh")]
	bytes: *include_bytes!("../arch/la/initproc_sh"),
	#[cfg(initproc = "ltp")]
	bytes: *include_bytes!("../arch/la/initproc_ltp")
};

lazy_static! {
	pub static ref INITTASK: Arc<TaskStruct> = {
		TaskStruct::init_proc(&INITPROC_DATA.bytes)
	};
}

pub fn add_timer_worker() {
	let worker_task =
		TaskStruct::new_kernel_worker(crate::process::task::worker::timer_kernel_worker, SCHED_OTHER);
	add_task(worker_task.clone());
	info!("add_timer_worker: pid={}", worker_task.getpid());
}

pub fn add_net_worker() {
	let worker_task =
		TaskStruct::new_kernel_worker(crate::process::task::worker::net_kernel_worker, SCHED_OTHER);
	add_task(worker_task.clone());
	info!("add_net_worker: pid={}", worker_task.getpid());
}

pub fn add_writeback_worker() {
	let worker_task =
		TaskStruct::new_kernel_worker(crate::process::task::worker::writeback_kernel_worker, SCHED_IDLE);
	add_task(worker_task.clone());
	info!("add_writeback_worker: pid={}", worker_task.getpid());
}

pub fn add_initproc() {
	add_task(INITTASK.clone());
	info!("add_initproc: pid={}", INITTASK.getpid());
}
