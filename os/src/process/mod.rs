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
use crate::mm::translated_write;
use crate::{arch::trap, console::print, mm::translated_byte_buffer};
use crate::process::trap::TrapContext;
use manager::*;
pub use manager::{get_process, list_pids, pop_process, remove_process};
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
    let task = current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
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
    task_inner.task_status = TaskStatus::WaitSaving;
    drop(task_inner);
    drop(task);
    // 切换到下一个任务
    schedule(task_cx_ptr);
} */

// 让被阻塞的线程睡眠，加入等待队列
pub fn current_task_to_sleep(mut wait_queue: MutexGuard<WaitQueue>) {
    error!("DO NOT CALL THIS FUNC, it's wrong");
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
        drop(wait_queue);
        while task.inner_exclusive_access().task_status == TaskStatus::WaitSaving {
            // 希望唤醒的任务还没保存好,将执行流保存后让出cpu
            suspend_current_and_run_next();
        }
        let mut task_inner = task.inner_exclusive_access();
        task_inner.task_status = TaskStatus::Ready;
        task_inner.owner_hart = None;
        drop(task_inner);
        add_task_into_pool(task);
    } else {
        drop(wait_queue);
    }
    
}

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 1;

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next(exit_code: i32){
    let task = match current_task() {
        Some(t) => t,
        None => {
            println!("No current task found in exit_current_and_run_next!");
            schedule(&mut TaskContext::zero_init() as *mut _);
            return;
        }
    };
    
    // 线程级资源由 TCB 回收接口统一处理。
    task.recycle_on_exit(exit_code);
    
    //若线程是最后一个存活线程，则将其线程码写入进程退出码,并回收进程资源
    //同时将子进程移交给initproc
    let process = task.process();

    // 运行完自动退出内核
    if process.getpid() == IDLE_PID {
        println!("[kernel] Idle process exit with exit_code {} ...", exit_code);
        panic!("All applications completed!");
    }

    //println!("[kernel] Process {} is exiting with code {} ...", process.getpid(), exit_code);
    drop(task);
    let mut proc_inner = process.inner_exclusive_access();
    proc_inner.alive_task_count -= 1;

    if proc_inner.alive_task_count > 0 {
        // 还有其他线程存活，不回收进程资源，直接调度下一个线程
        drop(proc_inner);
        schedule(&mut TaskContext::zero_init() as *mut _);
    }else{
        // 最后一个线程退出，回收进程资源,并将子进程移交给initproc
        let orphan_children = proc_inner.recycle_on_exit(exit_code);
        drop(proc_inner);
        // 如果有子进程，移交给initproc
        if !orphan_children.is_empty() {
            println!("[kernel] Process {} orphans {} children to initproc", process.getpid(), orphan_children.len());
            let initproc = INITTASK.process();
            for child in orphan_children.iter() {
                child.inner_exclusive_access().parent = Some(Arc::downgrade(&initproc));
            }
            let mut initproc_inner = initproc.inner_exclusive_access();
            initproc_inner.children.extend(orphan_children);
        }
        drop(process);
        schedule(&mut TaskContext::zero_init() as *mut _);
    }  
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
    task_inner.signals |= signal;
    // println!(
    //     "[K] current_add_signal:: current task sigflag {:?}",
    //     task_inner.signals
    // );
}

/// 处理信号
/// bug：目前的实现一次只处理一个信号，效率可能较低
pub fn handle_signals() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();


    // 不可被屏蔽的信号集
    let unmaskable = (SignalFlags::SIGKILL | SignalFlags::SIGSTOP);
    // 从掩码中移除不可屏蔽
    task_inner.signal_mask.remove(unmaskable);
    // 未决（待处理）信号 = 进程信号 & ~掩码（屏蔽的信号）
    let raw_signals = task_inner.signals;
    let mask = task_inner.signal_mask;
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
        task_inner.signals.remove(flag); // 把信号从 pending 队列中拿走
        // 释放锁
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
    info!("[SIG PROBE] Calling handler for sig: {}", sig);
    let task = current_task().unwrap();
    let proc = task.process();
    let mut task_inner = task.inner_exclusive_access();
    let action = {
        let proc_inner = proc.inner_exclusive_access();
        proc_inner.signal_actions.table[sig] // table和内核态的信号位图起点是一致的，都是第0号对应信号1,不应减一
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
        let cur_mask = task_inner.signal_mask;
        task_inner.signal_mask_backup.push(cur_mask);
        
        // 屏蔽 action 中指定的掩码 + 当前信号自身
        task_inner.signal_mask |= mask;
        task_inner.signal_mask.insert(signal);
        
        let trap_ctx = task_inner.get_trap_cx();
        task_inner.trap_ctx_backup.push(*trap_ctx);
        
        trap_ctx.set_rt(handler);
        trap_ctx.set_a0(sig + 1 /* 内核编号->用户编号 */);
        // 保证信号处理完恢复
        set_sig_ret(trap_ctx);
    } else { 
        // 由内核处理
        match signal {
            SignalFlags::SIGCHLD | SignalFlags::SIGURG | SignalFlags::SIGWINCH => {
                // 目前的实现这些默认忽略
                // trace!("[K] ignore default signal {:?}", signal);
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
                task_inner.killed = true;
                // println!("[K] default terminate for signal {:?}", signal);
            }
        }
    }
}

fn set_sig_ret(trap_ctx: &mut TrapContext) {
    use crate::arch::config::*;
    trap_ctx.set_ra(*SIG_RT_ADDR);
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