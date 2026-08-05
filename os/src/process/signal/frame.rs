use crate::mm::{try_translated_read, try_translated_write};
use crate::process::{current_task, SignalFlags, TaskControlBlockInner};
use crate::process::trap::TrapContext;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SignalAltStack {
	pub ss_sp: usize,
	pub ss_flags: i32,
	pub _pad: i32,
	pub ss_size: usize,
}

pub const SS_ONSTACK: i32 = 1;
pub const SS_DISABLE: i32 = 2;
pub const SS_AUTODISARM: i32 = 0x8000_0000u32 as i32;

impl Default for SignalAltStack {
	fn default() -> Self {
		Self {
			ss_sp: 0,
			ss_flags: SS_DISABLE,
			_pad: 0,
			ss_size: 0,
		}
	}
}

impl SignalAltStack {
	pub fn is_enabled(&self) -> bool {
		self.ss_flags & SS_DISABLE == 0
	}

	pub fn is_autodisarm(&self) -> bool {
		self.ss_flags & SS_AUTODISARM != 0
	}

	pub fn contains(&self, sp: usize) -> bool {
		self.is_enabled()
			&& self
				.ss_sp
				.checked_add(self.ss_size)
				.is_some_and(|end| sp >= self.ss_sp && sp < end)
	}

	pub fn status_for_sp(&self, sp: usize) -> Self {
		let mut status = *self;
		status.ss_flags = if self.contains(sp) {
			SS_ONSTACK
		} else if self.is_enabled() {
			0
		} else {
			SS_DISABLE
		};
		status
	}
}

// musl's sigset_t stores 128 bytes, i.e. 16 unsigned long words on riscv64.
const USER_SIGSET_WORDS: usize = 128 / core::mem::size_of::<usize>();

#[cfg(target_arch = "riscv64")]
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct RiscvMContext {
	gregs: [usize; 32],
	fpregs: [u8; 528],
}

#[cfg(target_arch = "riscv64")]
impl RiscvMContext {
	fn program_counter(&self) -> usize {
		self.gregs[0]
	}

	fn set_program_counter(&mut self, pc: usize) {
		self.gregs[0] = pc;
	}
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SignalUserContext {
	uc_flags: usize,
	uc_link: usize,
	uc_stack: SignalAltStack,
	uc_sigmask: [usize; USER_SIGSET_WORDS],
	uc_mcontext: RiscvMContext,
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SignalUserContext {
	uc_flags: usize,
	uc_link: usize,
	uc_stack: SignalAltStack,
	uc_sigmask: [usize; USER_SIGSET_WORDS],
	__uc_pad: isize,
	uc_mcontext_pc: usize,
	uc_mcontext_gregs: [usize; 32],
	uc_mcontext_flags: u32,
}

#[cfg(target_arch = "riscv64")]
impl SignalUserContext {
	fn from_trap_ctx(
		trap_ctx: &TrapContext,
		sigmask: usize,
		alt_stack: SignalAltStack,
	) -> Self {
		let mut uc_sigmask = [0usize; USER_SIGSET_WORDS];
		let mut uc_mcontext = RiscvMContext {
			gregs: [0usize; 32],
			fpregs: [0; 528],
		};
		uc_mcontext.gregs.copy_from_slice(&trap_ctx.x);
		uc_mcontext.set_program_counter(trap_ctx.get_rt());
		uc_sigmask[0] = sigmask;
		Self {
			uc_flags: 0,
			uc_link: 0,
			uc_stack: alt_stack,
			uc_sigmask,
			uc_mcontext,
		}
	}

	fn program_counter(&self) -> usize {
		self.uc_mcontext.program_counter()
	}

	fn apply_to_trap_ctx(&self, trap_ctx: &mut TrapContext) {
		trap_ctx.x.copy_from_slice(&self.uc_mcontext.gregs);
		trap_ctx.x[0] = 0;
		trap_ctx.set_rt(self.program_counter());
	}

	fn ucontext_stack(&self) -> SignalAltStack {
		self.uc_stack
	}
}

#[cfg(target_arch = "loongarch64")]
impl SignalUserContext {
	fn from_trap_ctx(
		trap_ctx: &TrapContext,
		sigmask: usize,
		alt_stack: SignalAltStack,
	) -> Self {
		let mut uc_sigmask = [0usize; USER_SIGSET_WORDS];
		uc_sigmask[0] = sigmask;
		Self {
			uc_flags: 0,
			uc_link: 0,
			uc_stack: alt_stack,
			uc_sigmask,
			__uc_pad: 0,
			uc_mcontext_pc: trap_ctx.get_rt(),
			uc_mcontext_gregs: trap_ctx.r,
			uc_mcontext_flags: 0,
		}
	}

	fn program_counter(&self) -> usize {
		self.uc_mcontext_pc
	}

	fn apply_to_trap_ctx(&self, trap_ctx: &mut TrapContext) {
		trap_ctx.r.copy_from_slice(&self.uc_mcontext_gregs);
		trap_ctx.set_rt(self.program_counter());
	}

	fn ucontext_stack(&self) -> SignalAltStack {
		self.uc_stack
	}
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SignalFrame {
	info: crate::syscall::process::SigInfo,
	ucontext: SignalUserContext,
}

pub(super) fn push_signal_frame(
	task_inner: &mut TaskControlBlockInner,
	sig: usize,
	saved_mask: SignalFlags,
	use_alt_stack: bool,
) -> Option<(usize, usize)> {
	let trap_ctx = task_inner.get_trap_cx();
	let frame_size = core::mem::size_of::<SignalFrame>();
	let interrupted_sp = trap_ctx.get_sp();
	let alt_stack = task_inner.signal_alt_stack;
	let switching_to_alt_stack =
		use_alt_stack && alt_stack.is_enabled() && !alt_stack.contains(interrupted_sp);
	let user_sp = if switching_to_alt_stack {
		alt_stack.ss_sp.checked_add(alt_stack.ss_size)?
	} else {
		interrupted_sp
	};
	let frame_sp = (user_sp.checked_sub(frame_size)? & !0xfusize) as usize;
	let frame = SignalFrame {
		info: crate::syscall::process::SigInfo {
			si_signo: sig as i32 + 1,
			si_errno: 0,
			si_code: 0,
			_pad0: 0,
			si_pid: 0,
			si_uid: 0,
			si_status: 0,
			_pad1: 0,
			_pad: [0; 12],
		},
		ucontext: SignalUserContext::from_trap_ctx(
			trap_ctx,
			saved_mask.bits() as usize,
			alt_stack.status_for_sp(interrupted_sp),
		),
	};

	let token = task_inner.get_user_token();
	let mm = task_inner.mm.as_ref()?.clone();
	if !mm
		.exclusive_access()
		.ensure_writable_user_range(frame_sp, frame_size, user_sp)
	{
		return None;
	}
	if !try_translated_write(token, frame_sp as *mut SignalFrame, frame) {
		return None;
	}

	let info_ptr = frame_sp;
	let ucontext_ptr = frame_sp + core::mem::size_of::<crate::syscall::process::SigInfo>();
	task_inner.signal_user_context_backup.push(ucontext_ptr);
	if switching_to_alt_stack && alt_stack.is_autodisarm() {
		// SS_AUTODISARM 只在 handler 执行期间禁用备用栈；sigreturn
		// 会从 ucontext.uc_stack 恢复原配置。
		task_inner.signal_alt_stack = SignalAltStack::default();
	}
	Some((info_ptr, ucontext_ptr))
}

pub(crate) fn restore_signal_context(task_inner: &mut TaskControlBlockInner) -> Option<isize> {
	let ucontext_ptr = task_inner.signal_user_context_backup.pop()?;
	let _saved_mask = task_inner.signal_mask_backup.pop()?;
	let mut trap_ctx = task_inner.trap_ctx_backup.pop()?;
	let token = task_inner.get_user_token();
	let user_ctx: SignalUserContext = try_translated_read(token, ucontext_ptr as *const SignalUserContext)?;
	#[cfg(target_arch = "riscv64")]
	warn!(
		"[SIG_RESTORE TP] tid={} saved_pc={:#x} saved_sp={:#x} saved_ra={:#x} saved_tp={:#x} saved_a0={:#x} user_pc={:#x} user_sp={:#x} user_ra={:#x} user_tp={:#x} user_a0={:#x}",
		current_task().unwrap().gettid(),
		trap_ctx.get_rt(),
		trap_ctx.get_sp(),
		trap_ctx.x[1],
		trap_ctx.x[4],
		trap_ctx.get_a0(),
		user_ctx.program_counter(),
		user_ctx.uc_mcontext.gregs[2],
		user_ctx.uc_mcontext.gregs[1],
		user_ctx.uc_mcontext.gregs[4],
		user_ctx.uc_mcontext.gregs[10]
	);
	user_ctx.apply_to_trap_ctx(&mut trap_ctx);
	task_inner.blocked = SignalFlags::from_bits_truncate(user_ctx.uc_sigmask[0] as u64);
	task_inner.signal_alt_stack = user_ctx.ucontext_stack();
	*task_inner.get_trap_cx() = trap_ctx;
	#[cfg(target_arch = "riscv64")]
	warn!(
		"[SIG_RESTORE RET] tid={} restored_pc={:#x} restored_sp={:#x} restored_ra={:#x} restored_tp={:#x} restored_a0={:#x}",
		current_task().unwrap().gettid(),
		task_inner.get_trap_cx().get_rt(),
		task_inner.get_trap_cx().get_sp(),
		task_inner.get_trap_cx().x[1],
		task_inner.get_trap_cx().x[4],
		task_inner.get_trap_cx().get_a0()
	);
	Some(task_inner.get_trap_cx().get_a0() as isize)
}
