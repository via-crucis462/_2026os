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
pub use id::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle};
use spin::{Mutex, MutexGuard};
pub use task::*;
pub use pcb::*;

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
pub use task::{TaskControlBlock, TaskStatus, TaskControlBlockInner};

pub use action::{SignalAction, SignalActions};
pub use manager::{add_task, tid2task};

pub use processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task, current_tid
};
pub use signal::{SignalFlags, MAX_SIG};



/// Make current task suspended and switch to the next task
pub fn suspend_current_and_run_next() {
    //debug!("[kernel] suspend_current_and_run_next");
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    drop(task_inner);
    // ---- release current PCB

    // Keep main-hart-affined tasks on the current hart queue to avoid cross-hart
    // ping-pong; other tasks can be rebalanced via the global pool.
    let on_main_hart = task.process().inner_exclusive_access().on_main_hart;
    if on_main_hart {
        add_task_in_current_hart(task);
    } else {
        add_task_into_pool(task);
    }
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

// 让被阻塞的线程睡眠，加入等待队列
pub fn current_task_to_sleep(mut wait_queue: MutexGuard<WaitQueue>) {
    let task = take_current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Blocked;
    drop(task_inner);
    // push back to wait queue.
    wait_queue.push_back(task);
    drop(wait_queue);
    // 将current_task上下文保存后切换到idle线程
    schedule(task_cx_ptr);
}

// 从等待队列中唤醒一个线程到全局池
pub fn wake_up_one(mut wait_queue: MutexGuard<WaitQueue>) {
    if let Some(task) = wait_queue.pop_front() {
        let mut task_inner = task.inner_exclusive_access();
        task_inner.task_status = TaskStatus::Ready;
        drop(task_inner);
        add_task_into_pool(task);
    }
    drop(wait_queue);
}

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 0;

/// Exit the current 'Running' task and run the next task in task list.
/// 初步修改，逻辑待检查
/// 2026.3.18,当前实现中，exit_current_and_run_next会将当前进程的子进程移交给initproc，
/// 即子进程不会直接去世，而是仍会执行完剩余的代码，直到自己也调用exit_current_and_run_next退出。
pub fn exit_current_and_run_next(exit_code: i32) {
    //println!("called exit_current_and_run_next with exit_code {}", exit_code);
    // take from Processor
    let task = take_current_task().unwrap();

    let pid = task.getpid();
    if pid == IDLE_PID {
        println!(
            "[kernel] Idle process exit with exit_code {} ...",
            exit_code
        );
        panic!("All applications completed!");
    }

    // remove from tid2task
    remove_from_tid2task(task.gettid());
    // **** access current TCB exclusively
    let mut task_inner: MPSafeGuard<'_, TaskControlBlockInner> = task.inner_exclusive_access();
    let proc = task.process();
    let mut proc_inner = proc.inner_exclusive_access();
    // Change status to Zombie
    task_inner.task_status = TaskStatus::Zombie;
    // Record exit code
    task_inner.exit_code = exit_code;
    // do not move to its parent but under initproc
    proc_inner.alive_task_count -= 1;
    let parent_to_wake = proc_inner.parent.as_ref().and_then(|p| p.upgrade());
    let wake_parent = proc_inner.is_zombie();
    // ++++++ access initproc TCB exclusively
    // ++++++ release parent PCB
    if proc_inner.is_zombie() {
        /*println!(
            "[kernel] pid={} exit with exit_code {}",
            pid, exit_code
        );*/
        let initproc = INITTASK.process();
        let mut initproc_inner = initproc.inner_exclusive_access();
        for child in proc_inner.children.iter() {
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&initproc));
            initproc_inner.children.push(child.clone());
        }
        drop(initproc_inner);
        // recycle resources of the process
        proc_inner.children.clear();
        // deallocate user space
        proc_inner.memory_set.recycle_data_pages();
        // drop file descriptors
        proc_inner.fd_table.clear();
        remove_process(pid);
        if let Some(parent) = parent_to_wake {
            wake_up_one(parent.wait_queue.lock());
        }
    }
    // **** release current PCB
    drop(proc_inner);
    drop(proc);
    drop(task_inner);
    // drop task manually to maintain rc correctly
    drop(task);// proc 也会随之drop
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
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
    bytes: *include_bytes!("../arch/riscv/initproc"),
};

#[link_section = ".data"]
#[cfg(target_arch = "loongarch64")]
static INITPROC_DATA: &'static InitProcData<[u8]> = &InitProcData {
    _align: [],
    bytes: *include_bytes!("../arch/la/initproc"),
};

lazy_static! {
    /// Creation of initial process
    ///
    /// the name "initproc" may be changed to any other app name like "usertests",
    /// but we have user_shell, so we don't need to change it.
    pub static ref INITTASK: Arc<TaskControlBlock> = {
        //let inode = open_file(ROOT_DENTRY.clone(),"ch7b_initproc", OpenFlags::RDONLY).unwrap();
        //let v = inode.read_all();
        let (proc, task) =  ProcessControlBlock::new(&INITPROC_DATA.bytes);
        // 将 initproc 加入全局进程列表
        add_process(proc);
        task
    };
}

///Add init process to the manager
pub fn add_initproc() {
    add_task(INITTASK.clone());
    info!("add_initproc: pid={}", INITTASK.getpid());
}

/// Check if the current task has any signal to handle
pub fn check_signals_error_of_current() -> Option<(i32, &'static str)> {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    // println!(
    //     "[K] check_signals_error_of_current {:?}",
    //     task_inner.signals
    // );
    task_inner.signals.check_error()
}

/// Add signal to the current task
pub fn current_add_signal(signal: SignalFlags) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    task_inner.signals |= signal;
    // println!(
    //     "[K] current_add_signal:: current task sigflag {:?}",
    //     task_inner.signals
    // );
}

/// call kernel signal handler
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

/// call user signal handler
fn call_user_signal_handler(sig: usize, signal: SignalFlags) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let proc = task.process();
    let proc_inner = proc.inner_exclusive_access();
    let handler = proc_inner.signal_actions.table[sig].handler;
    if handler != 0 {
        // user handler

        // handle flag
        task_inner.handling_sig = sig as isize;
        task_inner.signals ^= signal;

        // backup trapframe
        let trap_ctx = task_inner.get_trap_cx();
        task_inner.trap_ctx_backup = Some(*trap_ctx);

        // modify trapframe
        trap_ctx.set_rt(handler);

        // put args (a0)
        trap_ctx.set_a0(sig);
    } else {
        // Default action when user does not install a handler:
        // 1) always consume the pending signal bit;
        // 2) ignore known ignore-by-default signals;
        // 3) terminate for the rest.
        task_inner.signals.remove(signal);
        match signal {
            SignalFlags::SIGCHLD | SignalFlags::SIGURG | SignalFlags::SIGWINCH => {
                trace!(
                    "[K] task/call_user_signal_handler: ignore default signal {:?}",
                    signal
                );
            }
            _ => {
                task_inner.killed = true;
                println!(
                    "[K] task/call_user_signal_handler: default terminate for signal {:?}",
                    signal
                );
            }
        }
    }
}

/// Check if the current task has any signal to handle
/// 仅部分修改
fn check_pending_signals() {
    for sig in 0..(MAX_SIG + 1) {
        let task = current_task().unwrap();
        let proc = task.process();
        let task_inner = task.inner_exclusive_access();
        let proc_inner = proc.inner_exclusive_access();
        let signal = SignalFlags::from_bits(1 << sig).unwrap();
        if task_inner.signals.contains(signal) && (!task_inner.signal_mask.contains(signal)) {
            let mut masked = true;
            let handling_sig = task_inner.handling_sig;
            if handling_sig == -1 {
                masked = false;
            } else {
                let handling_sig = handling_sig as *const () as usize;
                if !proc_inner.signal_actions.table[handling_sig]
                    .mask
                    .contains(signal)
                {
                    masked = false;
                }
            }
            if !masked {
                drop(task_inner);
                drop(task);
                drop(proc_inner);
                drop(proc);
                if signal == SignalFlags::SIGKILL
                    || signal == SignalFlags::SIGSTOP
                    || signal == SignalFlags::SIGCONT
                    || signal == SignalFlags::SIGDEF
                {
                    // signal is a kernel signal
                    call_kernel_signal_handler(signal);
                } else {
                    // signal is a user signal
                    call_user_signal_handler(sig, signal);
                    return;
                }
            }
        }
    }
}

/// Handle signals for the current process
pub fn handle_signals() {
    loop {
        check_pending_signals();
        let (frozen, killed) = {
            let task = current_task().unwrap();
            let task_inner = task.inner_exclusive_access();
            (task_inner.frozen, task_inner.killed)
        };
        if !frozen || killed {
            break;
        }
        suspend_current_and_run_next();
    }
}
