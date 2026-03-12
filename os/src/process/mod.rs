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
pub use id::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, IdHandle};
pub use task::*;
pub use pcb::*;

use manager::*;



/// 任务处理器，改为pub供外部调用
pub mod processor;

mod switch;
/// fork相关实现
pub mod clone;
#[allow(clippy::module_inception)]


use crate::fs::ROOT_DENTRY;
use crate::fs::{open_file, OpenFlags};
pub use crate::process::id::*;
use alloc::sync::Arc;
pub use context::TaskContext;
use lazy_static::*;
use manager::fetch_task;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

pub use action::{SignalAction, SignalActions};
pub use manager::{add_task, tid2task};

pub use processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task, current_tid
};
pub use signal::{SignalFlags, MAX_SIG};



/// Make current task suspended and switch to the next task
pub fn suspend_current_and_run_next() {
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    drop(task_inner);
    // ---- release current PCB

    // push back to ready queue.
    add_task(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 0;

/// Exit the current 'Running' task and run the next task in task list.
/// 初步修改，逻辑待检查
pub fn exit_current_and_run_next(exit_code: i32) {
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
    let mut task_inner: spin::MutexGuard<'_, TaskControlBlockInner> = task.inner_exclusive_access();
    let proc = task.process();
    let mut proc_inner = proc.inner_exclusive_access();
    // Change status to Zombie
    task_inner.task_status = TaskStatus::Zombie;
    // Record exit code
    task_inner.exit_code = exit_code;
    // do not move to its parent but under initproc

    // ++++++ access initproc TCB exclusively
    // 
    {
        let initproc = INITTASK.process();
        let mut initproc_inner = initproc.inner_exclusive_access();
        for child in proc_inner.children.iter() {
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&initproc));
            initproc_inner.children.push(child.clone());
        }
    }
    // ++++++ release parent PCB

    proc_inner.children.clear();
    // deallocate user space
    proc_inner.memory_set.recycle_data_pages();
    // drop file descriptors
    proc_inner.fd_table.clear();
    drop(task_inner);
    // **** release current PCB
    // drop task manually to maintain rc correctly
    drop(task);// proc 也会随之drop
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
}

lazy_static! {
    /// Creation of initial process
    ///
    /// the name "initproc" may be changed to any other app name like "usertests",
    /// but we have user_shell, so we don't need to change it.
    pub static ref INITTASK: Arc<TaskControlBlock> = {
        let inode = open_file(ROOT_DENTRY.clone(),"ch7b_initproc", OpenFlags::RDONLY).unwrap();
        let v = inode.read_all();
        let (proc, task) =  ProcessControlBlock::new(v.as_slice());
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
        // default action
        println!("[K] task/call_user_signal_handler: default action: ignore it or kill process");
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
