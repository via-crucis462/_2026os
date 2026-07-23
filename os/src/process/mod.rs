//! Implementation of process management mechanism
//!
//! Here is the entry for process scheduling required by other modules
//! (such as syscall or clock interrupt).
//! By suspending or exiting the current process, you can
//! modify the process state, manage the process queue through TASK_MANAGER,
//! and switch the control flow through PROCESSOR.
//!
//! Be careful when you see [`__switch`]. Control flow around this function
//! might not be what you expect.pub mod task;
pub mod pcb;
pub mod schedule;
pub mod task;
pub mod id;
pub mod manager;

pub use schedule::*;
pub use id::{kstack_alloc, pid_alloc, tid_alloc, tid_from_pid, KernelStack, PidHandle};
use spin::{Mutex, MutexGuard};
pub use task::*;
pub use pcb::*;
use crate::mm::{translated_write, try_translated_read, try_translated_write};
use crate::{arch::trap, console::print, mm::translated_byte_buffer};
use crate::process::trap::TrapContext;
use manager::*;
use crate::sync::*;

/// 任务处理器，改为pub供外部调用
pub mod processor;

mod switch;
/// fork相关实现
pub mod clone;
#[allow(clippy::module_inception)]
#[allow(unused)]
use crate::fs::ROOT_DENTRY;
#[allow(unused)]
use crate::fs::{open_file, OpenFlags};
pub use crate::process::id::*;
use alloc::sync::Arc;
pub use context::TaskContext;
use lazy_static::*;
use manager::fetch_task;
use switch::__switch;
pub use task::{TaskControlBlock, task::taskstatus::TaskStatus, TaskControlBlockInner};

pub use action::{SignalAction, SignalActions};
pub use manager::{
    add_process, add_task, get_process, list_pids, remove_process, tid2task,
};

pub use processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task, current_tid
};
pub use signal::{SignalFlags, MAX_SIG};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SignalAltStack {
    ss_sp: usize,
    ss_flags: i32,
    _pad: i32,
    ss_size: usize,
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
    fn from_trap_ctx(trap_ctx: &TrapContext, sigmask: usize) -> Self {
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
            uc_stack: SignalAltStack::default(),
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
}

#[cfg(target_arch = "loongarch64")]
impl SignalUserContext {
    fn from_trap_ctx(trap_ctx: &TrapContext, sigmask: usize) -> Self {
        let mut uc_sigmask = [0usize; USER_SIGSET_WORDS];
        uc_sigmask[0] = sigmask;
        Self {
            uc_flags: 0,
            uc_link: 0,
            uc_stack: SignalAltStack::default(),
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
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SignalFrame {
    info: crate::syscall::process::SigInfo,
    ucontext: SignalUserContext,
}

fn push_signal_frame(
    task_inner: &mut TaskControlBlockInner,
    sig: usize,
    saved_mask: SignalFlags,
) -> Option<(usize, usize)> {
    let trap_ctx = task_inner.get_trap_cx();
    let frame_size = core::mem::size_of::<SignalFrame>();
    let frame_sp = (trap_ctx.get_sp().checked_sub(frame_size)? & !0xfusize) as usize;
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
        ucontext: SignalUserContext::from_trap_ctx(trap_ctx, saved_mask.bits() as usize),
    };
    
    let token = task_inner.get_user_token();
    if !try_translated_write(token, frame_sp as *mut SignalFrame, frame) {
        return None;
    }

    let info_ptr = frame_sp;
    let ucontext_ptr = frame_sp + core::mem::size_of::<crate::syscall::process::SigInfo>();
    task_inner.signal_user_context_backup.push(ucontext_ptr);
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



/// Make current task suspended and switch to the next task
pub fn suspend_current_and_run_next() {
    //debug!("[kernel] suspend_current_and_run_next");
    // There must be an application running.
    let task = current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
    // Change status to Ready
    task_inner.state = TaskStatus::Ready;
    drop(task_inner);
    drop(task);
    /*
    // ---- release current PCB

    // Keep main-hart-affined tasks on the current hart queue to avoid cross-hart
    // ping-pong; other tasks can be rebalanced via the global pool.
    let on_main_hart = task.process().inner_exclusive_access().on_main_hart;
    if on_main_hart {
        add_task_in_current_hart(task);
    } else {
        add_task_into_pool(task);
    }
    */
    // jump to scheduling cycle
    // 将释放留到schedule里统一处理，避免提前被别的核抢走
    schedule(task_cx_ptr);
}

/*
pub fn start_waiting_child() {
    let task = current_task().unwrap();
    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::BlockSaving;
    drop(task_inner);
    drop(task);
    // 切换到下一个任务
    schedule(task_cx_ptr);
} */

/// 将当前线程入队并调度，自动管理锁避免死锁。
/// 先加锁→入队→放锁，再 schedule，确保 schedule 时无锁持有。
pub fn block_current_and_run_next(queue: &Mutex<WaitQueue>) {
    // 此处不能take
    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        drop(task_inner);
        let mut guard = queue.lock();
        guard.push_back(task);
        ptr
    };
    // current_task 将在 idle_task（fn run_tasks) 中被释放并替换为下一个任务（如果有）
    schedule(task_cx_ptr);
}

// 从等待队列中唤醒一个线程到全局池
// 返回队列是否非空（即是否真的唤醒了一个线程）
pub fn wake_up_one(mut queue: &Mutex<WaitQueue>) -> bool {
    if let Some(task) = queue.lock().pop_front() {
        while task.inner_exclusive_access().state == TaskStatus::BlockSaving {
            println!("wake_up_one: task is still saving context"); // 调试用
            // 短暂等待
            core::hint::spin_loop();
            println!("wake_up_one: rechecking task status..."); // 调试用
        }
        let mut task_inner = task.inner_exclusive_access();
        task_inner.state = TaskStatus::Ready;
        drop(task_inner);
        add_task_into_pool(task);
        true
    } else {
        false
    }
}

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

    let last_thread = !manager::TID2TCB
        .exclusive_access()
        .values()
        .any(|other| other.getpid() == pid && other.gettid() != task.gettid());

    if last_thread {
        let (parent, orphan_children) = {
            let mut inner = task.inner_exclusive_access();
            (inner.parent.upgrade(), core::mem::take(&mut inner.children))
        };
        if let Some(parent) = parent {
            parent.inner_exclusive_access().pending.insert(SignalFlags::SIGCHLD);
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
    /*// 改为暂时不take，schedule到runtasks中统一处理
    let task = current_task().unwrap();
    // remove from tid2task
    remove_from_tid2task(task.gettid());
    let pid = task.getpid();
    
    info!("[kernel] Process {} is exiting with code {} ...", pid, exit_code);
    if pid == IDLE_PID {
        println!("[kernel] Idle process exit with exit_code {} ...", exit_code);
        panic!("All applications completed!");
    }

    // 修改当前任务状态
    let mut task_inner = task.inner_exclusive_access();
    // Change status to Zombie
    task_inner.task_status = TaskStatus::Zombie;
    task_inner.exit_code = exit_code;
    // 克隆一下arc指针
    let proc = task.process().clone();

    // fix:先释放掉tcb锁
    drop(task_inner);
    // fix:再获取pcb锁
    let mut proc_inner = proc.inner_exclusive_access();

    // 从 tasks 中移除自己，回收线程资源
    proc_inner.tasks.retain(|t| t.gettid() != task.gettid());

    // Decrease the number of alive tasks
    proc_inner.alive_task_count -= 1;
    //let parent_to_wake = proc_inner.parent.as_ref().and_then(|p| p.upgrade());
    let mut orphan_children = alloc::vec::Vec::new();
    
    if proc_inner.is_zombie() {
        orphan_children = core::mem::take(&mut proc_inner.children);
        if !orphan_children.is_empty() {
            warn!("[kernel] Process {} orphans {} children to initproc", pid, orphan_children.len());
        }
        
        // 清理资源
        proc_inner.memory_set.recycle_data_pages();
        proc_inner.fd_table.clear();

        
        // 唤醒父进程并发送 SIGCHLD 信号
        /*if let Some(parent) = parent_to_wake {
            let mut parent_inner = parent.inner_exclusive_access();
            parent_inner.signals.insert(SignalFlags::SIGCHLD);
            drop(parent_inner); 
            
            wake_up_one(parent.wait_queue.lock());
        }*/
    }
    
    // **** release current PCB
    drop(proc_inner);
    drop(proc);

    if !orphan_children.is_empty() {
        println!("[kernel] Process {} orphans {} children to initproc", pid, orphan_children.len());
        let initproc = INITTASK.process();
        for child in orphan_children.iter() {
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&initproc));
        }
        let mut initproc_inner = initproc.inner_exclusive_access();
        initproc_inner.children.extend(orphan_children);
    }

    // drop task manually to maintain rc correctly
    drop(task); 
    
    // schedule next task
    let mut _unused = TaskContext::zero_init();
    println!("[kernel] Process {} exits with code {}, switching to next task ...", pid, exit_code);*/
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
    /// Creation of initial process
    ///
    /// the name "initproc" may be changed to any other app name like "usertests",
    /// but we have user_shell, so we don't need to change it.
    pub static ref INITTASK: Arc<TaskStruct> = {
        //let inode = open_file(ROOT_DENTRY.clone(),"ch7b_initproc", OpenFlags::RDONLY).unwrap();
        //let v = inode.read_all();
        let task =  TaskStruct::init_proc(&INITPROC_DATA.bytes);
        // 将 initproc 加入全局进程列表
        //add_process(task.clone());
        task
    };
}

///Add init process to the manager
pub fn add_initproc() {
    add_task(INITTASK.clone());
    info!("add_initproc: pid={}", INITTASK.getpid());
}


/* rcore（小幅修改）的旧实现，留作参考
/// Check if the current task has any signal to handle
pub fn check_signals_error_of_current() -> Option<(i32, &'static str)> {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    if task_inner.killed {
        return Some((9, "Killed by signal"));
    }
    // println!(
    //     "[K] check_signals_error_of_current {:?}",
    //     task_inner.signals
    // );
    task_inner.signals.check_error()
}

/// call kernel signal handler
/// 由内核处理信号
fn call_kernel_signal_handler(signal: SignalFlags) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    match signal {
        SignalFlags::SIGSTOP => {
            task_inner.frozen = true;
            task_inner.signals ^= SignalFlags::SIGSTOP;
        }
        SignalFlags::SIGCONT => {
            if task_inner.signals.contains(SignalFlags::SIGCONT) {
                task_inner.signals ^= SignalFlags::SIGCONT;
                task_inner.frozen = false;
            }
        }
        _ => {
            // println!(
            //     "[K] call_kernel_signal_handler:: current task sigflag {:?}",
            //     task_inner.signals
            // );
            task_inner.killed = true;
        }
    }
}
*/

/// Add signal to the current task
/// 给当前任务加上信号
pub fn current_add_signal(signal: SignalFlags) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    task_inner.pending.insert(signal);
    // println!(
    //     "[K] current_add_signal:: current task sigflag {:?}",
    //     task_inner.signals
    // );
}

pub fn mark_signal_interrupted(task: &Arc<TaskControlBlock>) {
    let mut task_inner = task.inner_exclusive_access();
    task_inner.signal_interrupted = true;
}

pub fn take_current_signal_interrupted() -> bool {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let interrupted = task_inner.signal_interrupted;
    task_inner.signal_interrupted = false;
    interrupted
}

/// 处理信号
/// bug：目前的实现一次只处理一个信号，效率可能较低
pub fn handle_signals() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();


    // 不可被屏蔽的信号集
    let unmaskable = (SignalFlags::SIGKILL | SignalFlags::SIGSTOP);
    // 从掩码中移除不可屏蔽
    task_inner.blocked.remove(unmaskable);
    let raw_signals = task_inner.pending.flags();
    let mask = task_inner.blocked;
    let pending = {
        let mut copy = raw_signals;
        copy.remove(mask);
        copy
    };

    let pending_bits = pending.bits();

    if pending_bits != 0 {
        // 内核m号信号flag刚好对应尾部m个0
        // 内核的处理函数统一用减一后的编号
        let sig = pending_bits.trailing_zeros() as usize;
        let flag = SignalFlags::from_bits(1 << sig).unwrap();
        task_inner.pending.remove(flag);
        drop(task_inner);
        // 跳到处理函数
        call_signal_handler(sig, flag);
    } else {
        // 无待处理信号
        if raw_signals.bits() != 0 {
            warn!("[SIG PROBE] Signals exist ({:#x}) but fully masked ({:#x})", raw_signals, mask);
        }
    }
}

/// call user signal handler
/// 跳到用户态的信号处理函数
fn  call_signal_handler(sig: usize, signal: SignalFlags) {
    warn!("[SIG PROBE] Calling handler for sig: {}", sig);
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let action = {
        task_inner.signal_hand.exclusive_access().action(sig)
    };  
    let handler = action.handler;
    let mask = action.mask;

    // handler如果是0，1 表示默认/忽略
    // 默认，表示由内核处理
    const SIG_DFL: usize = 0;
    // 忽略
    const SIG_IGN: usize = 1;

    if handler == SIG_IGN {
        return; // 返回，正常trap_return
    }
    
    if handler != SIG_DFL {
        // 非默认，回到用户态处理
        // 先保存 mask 和上下文
        let cur_mask = task_inner.blocked;
        task_inner.signal_mask_backup.push(cur_mask);
        
        // 屏蔽 action 中指定的掩码
        task_inner.blocked |= mask;

        const SA_NODEFER: usize = 0x40000000;
        // 如果没有 SA_NODEFER 标志，则在处理信号时自动屏蔽该信号
        if action.flags & SA_NODEFER == 0 {
            task_inner.blocked.insert(signal);
        }

        let trap_ctx = task_inner.get_trap_cx();
        task_inner.trap_ctx_backup.push(*trap_ctx);
        let Some((info_ptr, ucontext_ptr)) = push_signal_frame(&mut task_inner, sig, cur_mask) else {
            task_inner.term_signal = Some(sig as i32 + 1);
            return;
        };

        #[cfg(target_arch = "riscv64")]
        if sig + 1 == 33 {
            warn!(
                "[SIGCANCEL TP] tid={} handler={:#x} pc={:#x} sp={:#x} ra={:#x} tp={:#x} info={:#x} uctx={:#x}",
                task.gettid(),
                handler,
                trap_ctx.get_rt(),
                trap_ctx.get_sp(),
                trap_ctx.x[1],
                trap_ctx.x[4],
                info_ptr,
                ucontext_ptr
            );
        }
        
        trap_ctx.set_rt(handler);
        trap_ctx.set_a0(sig + 1 /* 内核编号->用户编号 */);
        trap_ctx.set_a1(info_ptr);
        trap_ctx.set_a2(ucontext_ptr);
        trap_ctx.set_sp(info_ptr);
        // 保证信号处理完恢复
        set_sig_ret(trap_ctx);
    } else { 
        warn!(
            "[SIG PROBE] PID {} (tid {}) default handling for signal {} ({:?})",
            task.getpid(),
            task.gettid(),
            sig,
            signal
        );
        // 由内核处理
        match signal {
            SignalFlags::SIGCHLD 
            | SignalFlags::SIGURG 
            | SignalFlags::SIGWINCH => {
                // 目前的实现这些默认忽略
                info!("[K] ignore default signal {:?}", signal);
            }
             SignalFlags::SIGSTOP => {
                task_inner.frozen = true;
            }
            SignalFlags::SIGCONT => { // continue
                task_inner.frozen = false;
            }
            _ => {
                // 其他信号默认杀死任务
                // 此处标注为kill后稍后会调用exit_current_and_run_next，这里不直接调用
                task_inner.term_signal = Some(sig as i32 + 1);
                let pid = task.getpid();
                warn!("[SIG_DEATH] PID {} killed by signal {} ({:?})", pid, sig as i32 + 1, signal);
            }
        }
    }
}

fn set_sig_ret(trap_ctx: &mut TrapContext) {
    use crate::arch::config::*;
    trap_ctx.set_ra(*SIG_RT_ADDR);
}

/// 检查当前任务是否有未屏蔽的挂起信号
pub fn check_pending_signal() -> bool {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    let raw_signals = task_inner.pending.flags();
    let pending = raw_signals.bits() & !(
        task_inner.blocked.bits() & 
        !(SignalFlags::SIGKILL | SignalFlags::SIGSTOP).bits()
    );
    pending!= 0
}

/* rcore的实现修改而来，目前不被调用了，留作参考
/// Check if the current task has any signal to handle
/// 仅部分修改
fn check_pending_signals() {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    
    let signals = task_inner.signals.bits();
    let mask = task_inner.signal_mask.bits();
    let handling = task_inner.handling_sig;

    if signals != 0 {
        info!("[PROBE 3.1] check_pending: signals={:#x}, mask={:#x}, handling_sig={}", signals, mask, handling);
    }
    drop(task_inner); 

    for sig in 1..=MAX_SIG { 
        let task = current_task().unwrap();
        let proc = task.process();
        let task_inner = task.inner_exclusive_access();
        let proc_inner = proc.inner_exclusive_access();
        
        let signal = match SignalFlags::from_bits(1 << (sig - 1)) {
            Some(s) => s,
            None => continue,
        };
        
        if task_inner.signals.contains(signal) {
            let is_masked = task_inner.signal_mask.contains(signal);
            
         
            info!("[PROBE 3.2] found pending sig: {}, is_masked: {}", sig, is_masked);
            
            if !is_masked {
                let mut masked = false;
                if task_inner.handling_sig != -1 {
                    masked = true; 
                    info!("[PROBE 3.3] skipped sig {} because currently handling {}", sig, task_inner.handling_sig);
                }
                if !masked {
                    info!("[PROBE 3.4] delivering sig {} to user handler!", sig);
                    drop(proc_inner);
                    drop(task_inner);
                    if signal == SignalFlags::SIGKILL || signal == SignalFlags::SIGSTOP || signal == SignalFlags::SIGCONT {
                        call_kernel_signal_handler(signal);
                    } else {
                        call_user_signal_handler(sig, signal);
                        return;
                    }
                }
            }
        }
    }
}*/