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
use crate::mm::translated_byte_buffer;

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
pub const IDLE_PID: usize = 0;

/// Exit the current 'Running' task and run the next task in task list.
    pub fn exit_current_and_run_next(exit_code: i32) {
    // 改为暂时不take，schedule到runtasks中统一处理
    let task = current_task().unwrap();
    // remove from tid2task
    remove_from_tid2task(task.gettid());

    let pid = task.getpid();
    if pid == IDLE_PID {
        println!("[kernel] Idle process exit with exit_code {} ...", exit_code);
        panic!("All applications completed!");
    }

    // **** access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let proc = task.process();
    let mut proc_inner = proc.inner_exclusive_access();
    
    // Change status to Zombie
    task_inner.task_status = TaskStatus::Zombie;
    task_inner.exit_code = exit_code;
    
    // Decrease the number of alive tasks
    proc_inner.alive_task_count -= 1;
    let parent_to_wake = proc_inner.parent.as_ref().and_then(|p| p.upgrade());
    

    if proc_inner.alive_task_count == 0 || proc_inner.is_zombie {
        // 1. 确保标志位被设为 true，这样 wait4 遍历 children 时一抓一个准
        proc_inner.is_zombie = true;
        // 注意：如果是单线程程序，alive_task_count 减到 0 时，is_zombie 之前是 false，
        // 这里会把它变成 true，正式宣告进程进入僵尸态。

        let initproc = INITTASK.process();
        let mut initproc_inner = initproc.inner_exclusive_access();
        if !proc_inner.children.is_empty() {
            warn!("[kernel] Process {} orphans {} children to initproc", pid, proc_inner.children.len());
        }
        // 2. 托孤：把所有未退出的子进程交给 initproc
        for child in proc_inner.children.iter() {
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&initproc));
            initproc_inner.children.push(child.clone());
        }
        drop(initproc_inner);
        
        // 3. 清理当前进程持有的资源
        proc_inner.children.clear();
        proc_inner.memory_set.recycle_data_pages();
        proc_inner.fd_table.clear();

        
        // 4. 唤醒父进程并发送 SIGCHLD 信号
        if let Some(parent) = parent_to_wake {
            let mut parent_inner = parent.inner_exclusive_access();
            parent_inner.signals.insert(SignalFlags::SIGCHLD);
            drop(parent_inner); 
            
            wake_up_one(parent.wait_queue.lock());
        }
    }
    
    // **** release current PCB
    drop(proc_inner);
    drop(proc);
    drop(task_inner);
    // drop task manually to maintain rc correctly
    drop(task); 
    
    // schedule next task
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
    if task_inner.killed {
        return Some((9, "Killed by signal"));
    }
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
pub fn handle_signals() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    //--------------------调试信息----------------
    let raw_signals = task_inner.signals.bits();
    let raw_mask = task_inner.signal_mask.bits();
    /*println!(
        "[SIG_DEBUG] PID: {} | Pending: {:#x} | Mask: {:#x}", 
        task.getpid(), 
        raw_signals, 
        raw_mask
    );*/
    //--------------------调试信息----------------
    
    // 2. 检查是否有未屏蔽的信号 (或者不可屏蔽的 SIGKILL/SIGSTOP)
    let pending = task_inner.signals.bits() & !task_inner.signal_mask.bits();
    let unmaskable = SignalFlags::SIGKILL.bits() | SignalFlags::SIGSTOP.bits();
    let final_pending = pending | unmaskable;
    //--------------------调试信息----------------
   /*  if raw_signals != 0 {
        println!(
            "[SIG_DEBUG] Final Pending: {:#x} (Unmaskable: {:#x})", 
            final_pending, 
            unmaskable
        );
    }*/
    //--------------------调试信息----------------
    if final_pending != 0 {
        // 3. 取出第一个需要处理的信号编号 (1-based)
       
        let sig = final_pending.trailing_zeros() as usize + 1;
        let flag = SignalFlags::from_bits(1 << (sig - 1)).unwrap();
       
       
        info!("[SIG PROBE] Calling handler for sig: {}, final_pending: {:#x}", sig, final_pending);
        //  核心：必须先放锁，再调用你写好的处理函数！
        drop(task_inner); 

       
        call_user_signal_handler(sig, flag);
    }else {

        let raw_signals = task_inner.signals.bits();
        let raw_mask = task_inner.signal_mask.bits();
        if raw_signals != 0 {
            warn!("[SIG PROBE] Signals exist ({:#x}) but fully masked ({:#x})", raw_signals, raw_mask);
        }
    }
}
/// call user signal handler
fn call_user_signal_handler(sig: usize, signal: SignalFlags) {
    let task = current_task().unwrap();
    let proc = task.process();
    
    let action = {
        let proc_inner = proc.inner_exclusive_access();
        proc_inner.signal_actions.table[sig - 1]
    };  
    let handler = action.handler;
    let restorer = action.restorer;

    const SIG_DFL: usize = 0;
    const SIG_IGN: usize = 1;
    
    let mut task_inner = task.inner_exclusive_access();
    let before_bits = task_inner.signals.bits();
    task_inner.signals.remove(signal); // 把信号从 pending 队列中拿走
    let after_bits = task_inner.signals.bits(); 
    info!(
        "[SIG_EVENT] Process: {} | Signal: {:?}({}) | Pending: {:#x} -> {:#x}", 
        task.getpid(), signal, sig, before_bits, after_bits
    );


    if handler == SIG_IGN {
        return; // 直接返回，无事发生，绝不修改 handling_sig！
    } 

    else if handler != SIG_DFL {

        
        let trap_ctx = task_inner.get_trap_cx();
        task_inner.trap_ctx_backup = Some(*trap_ctx);
        task_inner.signal_mask_backup = Some(task_inner.signal_mask);
        
        // 设置当前正在处理的信号
        task_inner.handling_sig = sig as isize;
        // 临时屏蔽当前信号，防止处理时被同一个信号再次打断
        task_inner.signal_mask.insert(signal);
        
        // (可选：如果 action 里有 sa_mask，也应该在这里合并进来)
        // task_inner.signal_mask.bits |= action.sa_mask;

        // 设置用户态入口和参数
        // 注意：这里应该是修改 PC 指针，如果是 rCore 通常叫 set_sepc 或修改 trap_ctx.sepc
        trap_ctx.set_rt(handler);
        trap_ctx.set_a0(sig);

        // 设置返回地址 (ra)
        if restorer != 0 {
            trap_ctx.set_ra(restorer);
        } else {
            // ... 注入栈上蹦床代码 (逻辑保持你原来的写法) ...
            warn!("[KERNEL WARNING] restorer is 0! Injecting trampoline on stack...");
            // ... 你的计算 sp, 写 trampoline, 设置 set_ra(sp) 的代码 ...
            // trap_ctx.set_ra(sp);
            // trap_ctx.x[2] = sp;
        }

    } 
  
    else {
        match signal {
            SignalFlags::SIGCHLD | SignalFlags::SIGURG | SignalFlags::SIGWINCH => {
                // 默认忽略的信号，直接打个日志就行
                // trace!("[K] ignore default signal {:?}", signal);
            }
             SignalFlags::SIGSTOP => {
                task_inner.frozen = true;
            }
            SignalFlags::SIGCONT => {
                task_inner.frozen = false;
            }
            _ => {
                // 默认终止进程的信号
                task_inner.killed = true;
                // println!("[K] default terminate for signal {:?}", signal);
            }
        }
    }
}   
/// Check if the current task has any signal to handle
/// 仅部分修改
fn check_pending_signals() {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    
    let signals = task_inner.signals.bits();
    let mask = task_inner.signal_mask.bits();
    let handling = task_inner.handling_sig;
    
    // 🚨 探头 3.1：进门第一眼，看看进程当前真实状态！
    if signals != 0 {
        info!("[PROBE 3.1] check_pending: signals={:#x}, mask={:#x}, handling_sig={}", signals, mask, handling);
    }
    drop(task_inner); // 先放锁，免得死锁

    // 注意：信号编号从 1 开始，最大通常是 64。不能从 0 开始，否则 0 - 1 会溢出！
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
            
            // 🚨 探头 3.2：看看每一个存在的信号，它是怎么被判定拦截的！
            info!("[PROBE 3.2] found pending sig: {}, is_masked: {}", sig, is_masked);
            
            if !is_masked {
                let mut masked = false;
                if task_inner.handling_sig != -1 {
                    // 这里原本逻辑有点绕，简化一下：如果你正在处理信号，我们保守点先不打断
                    masked = true; 
                    info!("[PROBE 3.3] skipped sig {} because currently handling {}", sig, task_inner.handling_sig);
                }
                
                if !masked {
                    info!("[PROBE 3.4] delivering sig {} to user handler!", sig);
                    drop(task_inner);
                    drop(proc_inner);
                    
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
}