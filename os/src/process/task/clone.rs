//! Task cloning implementation is exposed through `TaskStruct::do_clone`.

use crate::arch::{timer::get_time_us, trap::TrapContext};
use crate::mm::MemorySet;
use crate::process::{add_task, kstack_alloc, pid_alloc};
use crate::process::signal::{SigHand, Signal, SignalAltStack, Sigpending};
use crate::process::task::{context::ThreadStruct, *};
use crate::sync::MPSafeCell;
use crate::syscall::errno::Errno;
use alloc::{sync::Arc, vec::Vec};
use spin::rwlock::RwLock;
use core::sync::atomic::AtomicBool;

impl TaskStruct {
	/// clone 系统调用的核心实现。
	///
	/// 参考 Linux kernel_clone()，根据 flags 控制父子任务之间的资源共享粒度。
	/// 接收者必须是 `&Arc<Self>`，以便通过 `Arc::downgrade` 建立父子关系和线程组关系。
	///
	/// ## 参数
	/// - `flags`:   低 8 位为退出信号（CSIGNAL），高位为 CLONE_* 资源共享标志
	/// - `stack`:   子任务的用户栈顶；0 表示继承父任务的 sp
	/// - `ptid`:    父地址空间中的 `pid_t*`，由 `CLONE_PARENT_SETTID` 激活
	/// - `ctid`:    子地址空间中的 `pid_t*`，由 `CLONE_CHILD_SETTID` / `CLONE_CHILD_CLEARTID` 激活
	/// - `tls`:     TLS 指针，由 `CLONE_SETTLS` 激活
	///
	/// ## 返回值
	/// 成功时向父任务返回子任务 PID；子任务从 `clone()` 返回 0（通过 trap 上下文中设置 a0=0 实现）。
	pub fn do_clone(self: &Arc<Self>, flags: usize, stack: usize, ptid: usize, ctid: usize, tls: usize) -> isize {
		// ── clone flags 常量 ──
		const CSIGNAL: usize = 0x000000ff;              // 退出信号掩码（低 8 位）
		const CLONE_VM: usize = 0x00000100;              // 共享地址空间
		const CLONE_FS: usize = 0x00000200;              // 共享 fs_struct（根目录/工作目录）
		const CLONE_FILES: usize = 0x00000400;           // 共享文件描述符表
		const CLONE_SIGHAND: usize = 0x00000800;         // 共享信号处理函数表
		const CLONE_VFORK: usize = 0x00004000;           // 子进程 exec/exit 前挂起父进程
		const CLONE_SETTLS: usize = 0x00080000;          // 设置子任务 TLS 指针
		const CLONE_PARENT_SETTID: usize = 0x00100000;   // 向父地址空间写入子 TID
		const CLONE_CHILD_CLEARTID: usize = 0x00200000;  // 子任务退出时清零 ctid 并 futex 唤醒
		const CLONE_CHILD_SETTID: usize = 0x01000000;    // 向子地址空间写入 TID
		const CLONE_THREAD: usize = 0x00010000;           // 创建线程（共享 tgid）
		const CLONE_SYSVSEM: usize = 0x00040000;           // 共享 System V 信号量（todo）
		const CLONE_CLEAR_SIGHAND: usize = 0x1_0000_0000; // 清空子进程信号处理函数表

		// ── 0. 判断是创建线程还是独立进程 ──
		let clone_thread = flags & CLONE_THREAD != 0;

		// ── 1. 获取父任务页表 token，用于后续 TID 指针可行性检查 ──
		let parent_token = {
			let inner = self.inner_exclusive_access();
			if let Some(mm) = inner.mm.as_ref(){
				mm.read().token()
			}else {
				return Errno::EINVAL.as_isize();
			}
		};

		// ── 2. 参数合法性校验：检查 TID 指针对应的用户地址是否可写 ──
		// CLONE_PARENT_SETTID：内核在父地址空间中向 *ptid 写入子 TID
		if flags & CLONE_PARENT_SETTID != 0
			&& (ptid == 0
				|| !crate::mm::prepare_user_write(
					parent_token,
					ptid,
					core::mem::size_of::<u32>(),
				))
		{
			return Errno::EFAULT.as_isize();
		}
		// CLONE_CHILD_SETTID / CLONE_CHILD_CLEARTID：内核在子地址空间中写入或清零 *ctid
		if flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID) != 0
			&& (ctid == 0
				|| !crate::mm::prepare_user_write(
					parent_token,
					ctid,
					core::mem::size_of::<u32>(),
				))
		{
			return Errno::EFAULT.as_isize();
		}

		// ── 3. 开始复制/共享各类资源 ──

		// 先获取 exec/clone 互斥锁
		let exec_lock: Option<ExecUpdateGuard> = if clone_thread {
			Some(match self.wait_exec_update_lock() {
				Ok(guard) => guard,
				Err(()) => return Errno::EAGAIN.as_isize(),
			})
		} else {
			None
		};

		let parent_inner = self.inner_exclusive_access();

		// 3a. 地址空间（mm）
		let parent_mm = parent_inner.mm.as_ref().unwrap().clone();
		let child_mm = if flags & CLONE_VM != 0 {
			// CLONE_VM：共享同一页表（父子使用同一个 MemorySet），用于线程
			parent_mm.clone()
		} else {
			// 无 CLONE_VM：写时复制（COW），创建独立的地址空间副本
			let mut parent_memory = parent_mm.write();
			let child_memory = MemorySet::from_existed_user(&mut parent_memory);
			#[cfg(target_arch = "riscv64")]
			parent_memory.flush_tlb_targets();
			#[cfg(target_arch = "loongarch64")]
			crate::arch::mm::flush_tlb_for_asid(parent_memory.asid());
			Arc::new(RwLock::new(child_memory))
		};

		// 3b. 文件系统信息（fs_struct：根目录、当前目录、umask）
		let child_fs = if flags & CLONE_FS != 0 {
			// 共享 fs_struct
			parent_inner.fs.clone()
		} else {
			// 深拷贝
			Arc::new(MPSafeCell::new(parent_inner.fs.exclusive_access().clone()))
		};

		// 3c. 文件描述符表
		let child_files = if flags & CLONE_FILES != 0 {
			// 共享 fd 表（线程典型行为）
			parent_inner.files.clone()
		} else {
			// 深拷贝 fd 表
			Arc::new(MPSafeCell::new(parent_inner.files.exclusive_access().clone()))
		};

		// 3d. 信号处理函数表（SigHand）
		let child_signal_hand = if flags & CLONE_CLEAR_SIGHAND != 0 {
			Arc::new(MPSafeCell::new(SigHand::new()))
		} else if flags & CLONE_SIGHAND != 0 {
			// 共享信号处理函数（线程必须；Linux 要求 CLONE_SIGHAND ⇒ CLONE_VM）
			parent_inner.signal_hand.clone()
		} else {
			// 深拷贝
			Arc::new(MPSafeCell::new(parent_inner.signal_hand.exclusive_access().clone()))
		};

		// 3e. 信号状态（Signal：共享挂起信号、线程计数、rlimit）
		let child_signal = if clone_thread {
			// 线程：增加线程组引用计数，共享 Signal
			parent_inner.signal.exclusive_access().add_thread();
			parent_inner.signal.clone()
		} else {
			// 独立进程：从父进程 fork 新的 Signal（清空挂起信号，继承 rlimit）
			Arc::new(MPSafeCell::new(Signal::fork_from(
				&parent_inner.signal.exclusive_access(),
			)))
		};

		// 3f. 复制父任务 trap 上下文（寄存器快照），后续在子任务上修改 sp/a0 等
		let parent_trap_cx = *parent_inner.get_trap_cx();

		// 3g. 继承父进程的亲缘关系（线程继承，独立进程重置）
		let inherited_parent = parent_inner.parent.clone();           // 接收 SIGCHLD 的父进程
		let inherited_real_parent = parent_inner.real_parent.clone(); // 实际创建者

		// 3h. 继承调度、信号掩码、凭据等属性
		let sched_policy = parent_inner.sched_policy;
		let sched_priority = parent_inner.sched_priority;
		let prio = parent_inner.prio;
		let static_prio = parent_inner.static_prio;
		let normal_prio = parent_inner.normal_prio;
		let mut se = parent_inner.se;
		se.exec_start = 0;
		se.sum_exec_runtime = 0;
		se.prev_sum_exec_runtime = 0;
		let mut rt = parent_inner.rt;
		rt.time_slice = 0;
		let mut dl = parent_inner.dl;
		dl.remaining_runtime = dl.runtime;
		dl.absolute_deadline = u64::MAX;
		dl.throttled = false;
		let cpus_allowed = parent_inner.cpus_allowed;
		let parent_cpu = parent_inner.cpu;
		let blocked = parent_inner.blocked;               // 信号阻塞掩码
		let nsproxy = parent_inner.nsproxy.clone();       // 命名空间代理
		// 凭据：CLONE_THREAD 共享 cred（与 Linux 一致）；独立进程必须复制。
		// 否则子进程 setuid/seteuid 会通过共享的 Arc 把父进程/兄弟进程的 uid
		// 一起改掉，导致后续 fork 出的测试进程变成非 root，chmod/chown 报 EPERM。
		let (cred, real_cred) = if clone_thread {
			(parent_inner.cred.clone(), parent_inner.real_cred.clone())
		} else {
			(
				Arc::new(MPSafeCell::new(parent_inner.cred.exclusive_access().clone())),
				Arc::new(MPSafeCell::new(parent_inner.real_cred.exclusive_access().clone())),
			)
		};
		let personality = parent_inner.personality;
		let locked_bytes = parent_inner.locked_bytes;
		let comm = parent_inner.comm;
		let pgid = parent_inner.pgid;
		let sid = parent_inner.sid;
		let oom_score_adj = parent_inner.oom_score_adj;
		let exe_path = parent_inner.exe_path.clone();
		// 线程与父共享 exec 互斥锁，独立进程新建。
		let exec_update_lock = if clone_thread {
			parent_inner.exec_update_lock.clone()
		} else {
			Arc::new(AtomicBool::new(false))
		};
		let signal_alt_stack = if flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0 {
			// Linux 仅在普通 CLONE_VM 子任务中禁用备用栈；vfork 是例外。
			SignalAltStack::default()
		} else {
			parent_inner.signal_alt_stack
		};
		let vfork_completion = if flags & CLONE_VFORK != 0 {
			Some(Arc::new(VforkCompletion::new()))
		} else {
			None
		};
		drop(parent_inner); // 释放父任务锁，避免后续分配 PID/内核栈时持锁

		// ── 4. 分配新任务标识 ──
		let pid = Arc::new(pid_alloc());     // 全局唯一 PID
		let tgid = if clone_thread {
			// 线程：tgid 与父任务相同，getpid() 返回同一个值
			self.tgid.clone()
		} else {
			// 独立进程：pid == tgid
			pid.clone()
		};

		// ── 5. 分配内核栈，并在栈顶保留 trap 上下文 ──
		let kernel_stack = kstack_alloc();
		let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
		let kernel_stack_top = trap_cx_addr;

		// ── 6. 构造子任务 TaskStruct ──
		let thread_group_leader = self.group_leader.clone();
		let child = Arc::new_cyclic(|child_weak| {
			// 线程组 leader：线程继承父的 group_leader，独立进程以自身为 leader
			let group_leader = if clone_thread {
				thread_group_leader.clone()
			} else {
				child_weak.clone()
			};
			TaskStruct {
				pid: pid.clone(),
				tgid: tgid.clone(),
				group_leader: group_leader.clone(),
				inner: MPSafeCell::new(TaskStructInner {
					on_main_hart: false,
					nsproxy,
					thread: ThreadStruct {
						trap_ctx: trap_cx_addr,
						task_ctx: TaskContext::goto_trap_return(kernel_stack_top),
					},
					group_leader,
					kernel_stack,
					// 亲缘关系：线程继承父的 parent/real_parent，独立进程将 self 设为父
					real_parent: if clone_thread {
						inherited_real_parent
					} else {
						Arc::downgrade(self)
					},
					parent: if clone_thread {
						inherited_parent
					} else {
						Arc::downgrade(self)
					},
					children: Vec::new(),
					pgid,
					sid,
					state: TaskStatus::Ready,
					exit_state: 0,
					exit_code: 0,
					exit_signal: (flags & CSIGNAL) as i32, // 退出时向父进程发送的信号
					flags: 0,
					errno: 0,
					oom_score_adj,
					sched_policy,
					sched_priority,
					prio,
					static_prio,
					normal_prio,
					se,
					rt,
					dl,
					mm: Some(child_mm),
					fs: child_fs,
					files: child_files,
					exe_path,
					signal: child_signal,
					exec_update_lock,
					signal_hand: child_signal_hand,
					blocked,
					pending: Sigpending::new(),             // 子任务的私有挂起信号为空
					signal_interrupted: false,
					sigsuspend_saved_mask: None,
					signal_mask_backup: Vec::new(),
					trap_ctx_backup: Vec::new(),
					signal_user_context_backup: Vec::new(),
					signal_alt_stack,
					term_signal: None,
					frozen: false,
					cred,
					real_cred,
					start_time: get_time_us() as u64,
					start_boottime: get_time_us() as u64,
					on_cpu: false,
					on_rq: false,
					cpu: parent_cpu,
					cpus_allowed,
					need_resched: false,
					clear_child_tid: if flags & CLONE_CHILD_CLEARTID != 0 {
						ctid  // 退出时清零此地址并 futex 唤醒
					} else {
						0
					},
					vfork_completion: vfork_completion.clone(),
					personality,
					locked_bytes,
					comm,
				}),
			}
		});

		// ── 7. 设置子任务的 trap 上下文 ──
		{
			let child_inner = child.inner_exclusive_access();
			let trap_cx = child_inner.get_trap_cx();
			// 从父任务复制寄存器快照
			*trap_cx = parent_trap_cx;
			// RISC-V：更新内核栈指针
			#[cfg(target_arch = "riscv64")]
			{
				trap_cx.kernel_sp = kernel_stack_top;
			}
			// 设置用户栈指针
			if stack != 0 {
				trap_cx.set_sp(stack);
			}
			// CLONE_SETTLS：将 TLS 写入线程指针寄存器（RISC-V: x4/tp, LoongArch: r2/tp）
			if flags & CLONE_SETTLS != 0 {
				trap_cx.set_tls(tls);
			}
			// ★ 关键：子任务从 clone() 返回 0
			trap_cx.set_a0(0);
		}

		// ── 8. 向用户空间写入 TID（按标志位分别处理） ──
		let child_tid = child.gettid() as u32;
		// CLONE_PARENT_SETTID：向父地址空间的 *ptid 写入子 TID
		if flags & CLONE_PARENT_SETTID != 0
			&& !crate::mm::try_translated_write(parent_token, ptid as *mut u32, child_tid)
		{
			return Errno::EFAULT.as_isize();
		}
		// CLONE_CHILD_SETTID：向子地址空间的 *ctid 写入自身 TID
		if flags & CLONE_CHILD_SETTID != 0 {
			let child_token = {
				let child_inner = child.inner_exclusive_access();
				let child_memory = child_inner.mm.as_ref().unwrap().read();
				child_memory.token()
			};
			if !crate::mm::try_translated_write(child_token, ctid as *mut u32, child_tid) {
				return Errno::EFAULT.as_isize();
			}
		}
		// 注意：CLONE_CHILD_CLEARTID 在步骤 6 中将 ctid 保存到 clear_child_tid 字段，
		// 实际的清零 + futex 唤醒操作在子任务退出时由 exit 路径完成。

		// ── 9. 建立父子关系并加入调度 ──
		// 只有独立进程才加入父进程的 children 链表（线程属于同一线程组，不需要）
		if !clone_thread {
			self.inner_exclusive_access().children.push(child.clone());
		}
		// 注册到全局 TID→TaskStruct 映射，并加入调度就绪队列
		warn!("do_clone: adding child task with PID {} and TID {}", child.getpid(), child.gettid());
		#[cfg(target_arch = "loongarch64")]
		{
			let inner = child.inner_exclusive_access();
			warn!(
				"[la-clone] child pid={} tid={} task_ra={:#x} task_sp={:#x} trap_ctx={:#x} user_era={:#x} user_sp={:#x}",
				child.getpid(),
				child.gettid(),
				inner.thread.task_ctx.ra,
				inner.thread.task_ctx.sp,
				inner.thread.trap_ctx,
				inner.get_trap_cx().get_rt(),
				inner.get_trap_cx().get_sp(),
			);
		}
		add_task(child);

		// 克隆对exec可能访问资源的访问已完成
		drop(exec_lock);

		// 如果是 CLONE_VFORK，则父任务阻塞等待子任务完成 exec/exit
		if let Some(completion) = vfork_completion {
			completion.wait();
		}

		// 返回子任务的 PID（父任务视角）
		pid.0 as isize
	}
}
