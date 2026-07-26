//! Task image replacement is exposed through `TaskStruct::do_exec`.

use crate::arch::config::PAGE_SIZE;
use crate::arch::timer::get_time_us;
use crate::arch::trap::{trap_handler, TrapContext};
use crate::fs::{open_file, OpenFlags};
use crate::process::FdFlags;
use crate::mm::{translated_write, KERNEL_SPACE, MemorySet, VirtAddr};
use crate::process::registry::{remove_from_tid2task, TID2TCB};
use crate::process::scheduler::runqueue::{
	lock_dispatch, remove_task_from_all_local_queues_unlocked,
	remove_task_from_global_pool_unlocked,
};
use crate::process::signal::{SigHand, Signal, Sigpending};
use crate::process::task::{TaskControlBlock, TaskStatus, TaskStruct};
use crate::sync::MPSafeCell;
use alloc::{string::String, sync::Arc, vec, vec::Vec};

impl TaskStruct {
	pub fn do_exec(
		self: &Arc<Self>,
		caller_task: Arc<TaskControlBlock>,
		elf_data: &[u8],
		args: Vec<String>,
		envs: Vec<String>,
		on_main_hart: bool,
	) {

		const AT_BASE: usize = 7;
		const AT_PHDR: usize = 3;
		const AT_PHENT: usize = 4;
		const AT_PHNUM: usize = 5;
		const AT_PAGESZ: usize = 6;
		const AT_ENTRY: usize = 9;
		const AT_RANDOM: usize = 25;

		fn prepare_stack_pages(memory_set: &mut MemorySet, start: usize, end: usize) {
			let mut page = start / PAGE_SIZE * PAGE_SIZE;
			while page < end {
				memory_set.handle_page_fault(page, end);
				page += PAGE_SIZE;
			}
		}

		let cwd = self.inner_exclusive_access().fs.exclusive_access().get_pwd();
		let mut has_interp = false;
		let Some((
			mut memory_set,
			_heap_bottom,
			mut user_sp,
			final_entry_point,
			main_entry_point,
			phdr_addr,
			phnum,
			phent,
			interp_base,
		)) = MemorySet::from_elf_with_interp_loader(elf_data, |interp_path| {
			open_file(cwd.clone(), interp_path, OpenFlags::RDONLY, 0).map(|inode| {
				has_interp = true;
				inode.read_all()
			})
		}) else {
			return;
		};

		#[cfg(target_arch = "riscv64")]
		let (trap_cx_addr, kernel_stack_top) = {
			let trap_cx_addr = caller_task.inner_exclusive_access().thread.trap_ctx;
			let trap_cx_va = VirtAddr::from(trap_cx_addr);
			let trap_cx_ppn = KERNEL_SPACE
				.exclusive_access()
				.translate(trap_cx_va.std_floor())
				.expect("kernel TrapContext is not mapped")
				.ppn();
			memory_set.install_trap_context_page(trap_cx_va, trap_cx_ppn);
			(trap_cx_addr, trap_cx_addr)
		};

		#[cfg(target_arch = "loongarch64")]
		let (trap_cx_addr, kernel_stack_top) = {
			let inner = caller_task.inner_exclusive_access();
			(inner.thread.trap_ctx, inner.thread.trap_ctx)
		};

		let token = memory_set.token();
		let mut argv_ptrs = Vec::with_capacity(args.len());
		let arg_size = args.iter().map(|arg| arg.len() + 1).sum::<usize>();
		if arg_size != 0 {
			prepare_stack_pages(&mut memory_set, user_sp - arg_size, user_sp);
		}
		for arg in &args {
			user_sp -= arg.len() + 1;
			for (offset, byte) in arg.as_bytes().iter().enumerate() {
				translated_write(token, (user_sp + offset) as *mut u8, *byte);
			}
			translated_write(token, (user_sp + arg.len()) as *mut u8, 0);
			argv_ptrs.push(user_sp);
		}

		let mut envp_ptrs = Vec::with_capacity(envs.len());
		let env_size = envs.iter().map(|env| env.len() + 1).sum::<usize>();
		if env_size != 0 {
			prepare_stack_pages(&mut memory_set, user_sp - env_size, user_sp);
		}
		for env in &envs {
			user_sp -= env.len() + 1;
			for (offset, byte) in env.as_bytes().iter().enumerate() {
				translated_write(token, (user_sp + offset) as *mut u8, *byte);
			}
			translated_write(token, (user_sp + env.len()) as *mut u8, 0);
			envp_ptrs.push(user_sp);
		}

		user_sp -= 16;
	prepare_stack_pages(&mut memory_set, user_sp, user_sp + 16);
		let random_at = user_sp;
		for offset in 0..16 {
			translated_write(token, (random_at + offset) as *mut u8, 0x23);
		}
		user_sp -= user_sp % core::mem::size_of::<usize>();

		let mut auxv = vec![
			(AT_PHDR, phdr_addr),
			(AT_PHENT, phent),
			(AT_PHNUM, phnum),
			(AT_PAGESZ, PAGE_SIZE),
			(AT_ENTRY, main_entry_point),
			(AT_RANDOM, random_at),
		];
		if has_interp {
			if let Some(base) = interp_base {
				auxv.push((AT_BASE, base));
			}
		}
		auxv.push((0, 0));

		let word_size = core::mem::size_of::<usize>();
		let metadata_size = (auxv.len() * 2
			+ envp_ptrs.len() + 1
			+ argv_ptrs.len() + 1
			+ 1)
			* word_size;
		user_sp -= (user_sp - metadata_size) % 16;
		prepare_stack_pages(&mut memory_set, user_sp - metadata_size, user_sp);

		for (id, value) in auxv.iter().rev() {
			user_sp -= word_size;
			translated_write(token, user_sp as *mut usize, *value);
			user_sp -= word_size;
			translated_write(token, user_sp as *mut usize, *id);
		}

		user_sp -= word_size;
		translated_write(token, user_sp as *mut usize, 0usize);
		for env_ptr in envp_ptrs.iter().rev() {
			user_sp -= word_size;
			translated_write(token, user_sp as *mut usize, *env_ptr);
		}

		user_sp -= word_size;
		translated_write(token, user_sp as *mut usize, 0usize);
		for arg_ptr in argv_ptrs.iter().rev() {
			user_sp -= word_size;
			translated_write(token, user_sp as *mut usize, *arg_ptr);
		}
		let argv_base = user_sp;
		user_sp -= word_size;
		translated_write(token, user_sp as *mut usize, args.len());

		let mut trap_cx = TrapContext::app_init_context(
			final_entry_point,
			user_sp,
			KERNEL_SPACE.exclusive_access().token(),
			kernel_stack_top,
			trap_handler as *const () as usize,
		);
		trap_cx.set_a0(args.len());
		trap_cx.set_a1(argv_base);

		let old_signal = {
			let mut inner = caller_task.inner_exclusive_access();
			let old_signal = inner.signal.clone();
			inner.thread.trap_ctx = trap_cx_addr;
			inner.mm = Some(Arc::new(MPSafeCell::new(memory_set)));
			inner.on_main_hart = on_main_hart;
			inner.signal = Arc::new(MPSafeCell::new(Signal::fork_from(
				&old_signal.exclusive_access(),
			)));
			inner.signal_hand = Arc::new(MPSafeCell::new(SigHand::new()));
			inner.pending = Sigpending::new();
			inner.signal_interrupted = false;
			inner.signal_mask_backup.clear();
			inner.trap_ctx_backup.clear();
			inner.signal_user_context_backup.clear();
			inner.term_signal = None;
			inner.frozen = false;
			inner.clear_child_tid = 0;
			inner.start_time = get_time_us() as u64;
			if let Some(argv0) = args.first() {
				inner.comm = [0; 10];
				let bytes = argv0.as_bytes();
				let copy_len = bytes.len().min(inner.comm.len() - 1);
				inner.comm[..copy_len].copy_from_slice(&bytes[..copy_len]);
			}
			*inner.get_trap_cx() = trap_cx;
			inner.files.clone()
		};
		{
			let mut files = old_signal.exclusive_access();
			for fd in 0..files.fds.len() {
				if files.fds[fd].flags.contains(FdFlags::CLOEXEC) {
					files.clear_fd(fd);
				}
			}
		}

		let sibling_tasks = {
			let tasks = TID2TCB.exclusive_access();
			tasks
				.values()
				.filter(|task| task.gettgid() == caller_task.gettgid())
				.filter(|task| !Arc::ptr_eq(task, &caller_task))
				.cloned()
				.collect::<Vec<_>>()
		};
		for sibling in sibling_tasks {
			let mut sibling_inner = sibling.inner_exclusive_access();
			#[cfg(target_arch = "riscv64")]
			if let Some(mm) = sibling_inner.mm.as_ref() {
				mm.exclusive_access().remove_trap_context_page(VirtAddr::from(
					sibling_inner.thread.trap_ctx,
				));
			}
			sibling_inner.state = TaskStatus::Zombie;
			sibling_inner.mm.take();
			drop(sibling_inner);
			let _dispatch = lock_dispatch();
			remove_from_tid2task(sibling.gettid());
			remove_task_from_all_local_queues_unlocked(sibling.gettid());
			remove_task_from_global_pool_unlocked(sibling.gettid());
		}
	}
}
