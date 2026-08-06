//! Implementation of process management mechanism
//!
//! Here is the entry for process scheduling required by other modules
//! (such as syscall or clock interrupt).
//! By suspending or exiting the current process, you can
//! modify the process state, manage the process queue through TASK_MANAGER,
//! and switch the control flow through PROCESSOR.
//!
//! Be careful when you see [`__switch`]. Control flow around this function
//! might not be what you expect.
pub mod scheduler;
pub mod task;
pub mod id;
pub mod registry;
pub mod signal;
pub mod timer;
pub mod init;
pub mod child_wait;

pub use scheduler::*;
pub use id::{kstack_alloc, pid_alloc, tid_alloc, tid_from_pid, KernelStack, PidHandle};
use spin::{Mutex, MutexGuard};
pub use task::{
    Cred, FdFlags, FileDescriptor, FileDescriptorTable, FsStruct, Rlimit,
    Rlimit64, Rlimits, TaskContext, TaskControlBlock, TaskControlBlockInner,
    TaskStatus, TaskStruct, TaskStructInner, ThreadStruct,
};
use crate::mm::translated_byte_buffer;
use crate::{arch::trap, console::print};
use registry::*;
use crate::sync::*;

#[allow(clippy::module_inception)]
#[allow(unused)]
use crate::fs::ROOT_DENTRY;
#[allow(unused)]
use crate::fs::{open_file, OpenFlags};
pub use crate::process::id::*;
use alloc::sync::Arc;
use lazy_static::*;

pub use signal::{
    check_pending_signal, current_add_signal, handle_signals, pending_signal_should_restart,
    mark_signal_interrupted, take_current_signal_interrupted, SignalAction,
    SignalActions, SignalFlags, MAX_SIG,
};
pub use timer::{
    add_posix_timer, check_posix_timers, delete_posix_timer, get_posix_timer_spec,
    remove_posix_timer, remove_process_posix_timers, set_posix_timer, ITimerSpec,
    KernelSigEvent, PosixTimer,
};
pub(crate) use signal::restore_signal_context;
pub use init::{add_initproc, INITTASK};
pub use task::exit::{exit_current_and_run_next, IDLE_PID};
pub use registry::{
    add_process, add_task, get_process, list_pids, remove_process, tid2task,
};

pub use scheduler::processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task, current_tid
};
pub use scheduler::wait::{
    block_current_and_run_next, block_current_and_run_next_if,
    block_current_and_run_next_if_task,
    suspend_current_and_run_next, wake_up_all, wake_up_one,
};
pub use child_wait::{wait4_block_current, waitid_block_current, wake_child_exit_waiters};

// Make current task suspended and switch to the next task
/*pub fn suspend_current_and_run_next() {
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
}*/

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

// 将当前线程入队并调度，自动管理锁避免死锁。
// 先加锁→入队→放锁，再 schedule，确保 schedule 时无锁持有。
/*pub fn block_current_and_run_next(queue: &Mutex<WaitQueue>) {
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
}*/

// 在等待队列锁保护下重新检查阻塞条件，避免“检查条件”和“加入等待队列”
// 之间发生唤醒而造成永久睡眠。返回值表示当前任务是否实际阻塞过。
/*pub fn block_current_and_run_next_if<F>(queue: &Mutex<WaitQueue>, should_block: F) -> bool
where
    F: FnOnce() -> bool,
{
    let mut guard = queue.lock();
    if !should_block() {
        return false;
    }

    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        ptr
    };
    guard.push_back(task);
    drop(guard);
    schedule(task_cx_ptr);
    true
}*/

// 从等待队列中唤醒一个线程到全局池
// 返回队列是否非空（即是否真的唤醒了一个线程）
/*pub fn wake_up_one(mut queue: &Mutex<WaitQueue>) -> bool {
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
}*/

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
        info!("[PROBE 3.1] check_pending: signals=0x{:x}, mask=0x{:x}, handling_sig={}", signals, mask, handling);
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
