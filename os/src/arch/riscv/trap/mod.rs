//! Trap handling functionality
//!
//! For rCore, we have a single trap entry point, namely `__alltraps`. At
//! initialization in [`init()`], we set the `stvec` CSR to point to it.
//!
//! All traps go through `__alltraps`, which is defined in `trap.S`. The
//! assembly language code does just enough work restore the kernel space
//! context, ensuring that Rust code safely runs, and transfers control to
//! [`trap_handler()`].
//!
//! It then calls different functionality based on what exactly the exception
//! was. For example, timer interrupts trigger task preemption, and syscalls go
//! to [`syscall()`].
mod context;
use crate::net::net_poll;
use crate::{KERNEL_STACK_SIZE, PAGE_SIZE, get_hart_id};
use crate::arch::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};
use crate::mm::VirtAddr;
use crate::syscall::syscall;
use crate::task::{
    KernelStack, SignalFlags, TaskStatus, add_task, current_task, current_tid,
    current_trap_cx, current_user_token, exit_current_and_run_next,
    suspend_current_and_run_next, handle_signals, current_add_signal
};
use crate::arch::timer::get_time_ms;
use alloc::sync::Arc;

use crate::arch::timer::set_next_trigger;
use core::arch::{asm, global_asm};
use riscv::register::{scause, stval, stvec, sie};
use scause::{Exception, Interrupt, Trap};
use stvec::TrapMode;

global_asm!(include_str!("trap.S"));

/// Initialize trap handling
pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    unsafe {
        stvec::write(trap_from_kernel as *const () as usize, TrapMode::Direct);
    }
}

fn set_user_trap_entry() {
    unsafe {
        stvec::write(TRAMPOLINE as *const () as usize, TrapMode::Direct);
    }
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    unsafe {
        sie::set_stimer();
    }
}

/// trap handler
#[no_mangle]
pub fn trap_handler() -> ! {
    trace!("[kernel] trap_handler: a trap from user space");
    let scause = riscv::register::scause::read();
    let sepc = riscv::register::sepc::read();
    let stval = riscv::register::stval::read();

    debug!(
        "trap_handler: cause: {:?}, sepc: {:#x}, stval: {:#x}", 
        scause.cause(), 
        sepc, 
        stval
    );
    set_kernel_trap_entry();
    let scause = scause::read();
    let stval = stval::read();
    
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            let mut cx = current_trap_cx();
            cx.set_rt(cx.get_rt() + 4);
            // get system call return valuehandle_signals
            let result = syscall(
                cx.x[17], 
                [cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]]
            );
            current_task().unwrap().inner_exclusive_access().errno = if result < 0 {
                (-result) as i32
            } else {
                0
            };
            if result < 0 {
                warn!("pid[{}] syscall {} returned error code {}", current_task().unwrap().process().pid.0, cx.x[17], result);
            }
            // cx is changed during sys_exec, so we have to call it again
            //println!("[kernel] syscall: id={}, args={:x?}, ret={:#x}", cx.x[17], [cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]], result);
            cx = current_trap_cx();
            cx.set_a0(result as usize);
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();
            let current_ms = get_time_ms();
            let expired_pids = crate::timer::TIMER_MANAGER.lock().tick(current_ms);
            for pid in expired_pids {
                if let Some(process) = crate::task::get_process(pid) {
                    let mut process_inner = process.inner_exclusive_access();

                    for task in process_inner.tasks.iter() {
                        let mut task_inner = task.inner_exclusive_access();
                        task_inner.signals |= crate::task::SignalFlags::SIGALRM;
                        if task_inner.task_status == crate::task::TaskStatus::Blocked {
                            task_inner.task_status = crate::task::TaskStatus::Ready;
                            crate::task::add_task(Arc::clone(task)); 
                        }
                    }
                }
            }
            net_poll();
            suspend_current_and_run_next();
        }
        Trap::Exception(Exception::StorePageFault) |
        Trap::Exception(Exception::LoadPageFault) |
        Trap::Exception(Exception::InstructionPageFault) => {
            /*println!(
                "[kernel] error  {:#x},  {:#x}",
                stval, sepc
            );*/
            let task = current_task().unwrap();
            let process = task.process(); 
            let mut process_inner = process.inner_exclusive_access();
            
            // 【修改 1】：获取当前的栈指针 SP
            let sp = current_trap_cx().x[2];
            //
            if process_inner.memory_set.handle_cow_fault(stval) {
                info!("[WATCHDOG][COW] : {:#x}, PC: {:#x}", stval, sepc);
                drop(process_inner);
                drop(process);
                drop(task);
            } else if process_inner.memory_set.handle_page_fault(stval, sp) {
                info!("[WATCHDOG] : {:#x}, PC: {:#x}", stval, sepc);
                // 修复成功！释放锁
                drop(process_inner);
                drop(process);
                drop(task);
            } else {
                // 【新增】检查 userfaultfd 注册范围
                /*println!(
                    "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}, sp={:#x}",
                    process.pid.0,
                    scause.cause(),
                    current_trap_cx().get_rt(),
                    stval,
                    sp
                );*/
                let fd_table = &process_inner.fd_table;
                let mut uffd_handled = false;

                // 调试：打印 fd_table 中每个条目的文件类型
                /*info!("=== fd_table dump for PID {} ===", process.pid.0);
                for (idx, fd_entry) in fd_table.iter().enumerate() {
                    match &fd_entry.file {
                        Some(file) => {
                            let type_name = if file.as_any().downcast_ref::<crate::fs::UserPageFaultInfo>().is_some() {
                                "UserPageFaultInfo"
                            } else if file.as_any().downcast_ref::<crate::fs::Stdin>().is_some() {
                                "Stdin"
                            } else if file.as_any().downcast_ref::<crate::fs::Stdout>().is_some() {
                                "Stdout"
                            } else if file.as_any().downcast_ref::<crate::fs::Stderr>().is_some() {
                                "Stderr"
                            } else {
                                "Other"
                            };
                            info!("  fd[{}] = Some({})", idx, type_name);
                        }
                        None => info!("  fd[{}] = None", idx),
                    }
                }
                info!("=== end fd_table dump ===");*/
                process_inner.info_map_areas();
                for fd_entry in fd_table.iter() {
                    if let Some(file) = &fd_entry.file {
                        if let Some(uffd) = file.as_any()
                            .downcast_ref::<crate::fs::UserPageFaultInfo>()
                        {
                            println!("Checking UFFD registered ranges for PID {}...", process.pid.0);
                            let in_range = uffd.registered_ranges.exclusive_access()
                                .iter().map(|&(start, len)| {
                                    println!("  Comparing fault address {:#x} with registered range {:#x} - {:#x}", stval, start, start + len);
                                    stval >= start && stval < start + len
                                }).any(|x| x);
                            if in_range {
                                println!("Page fault address {:#x} is within a registered UFFD range, handling with UFFD", stval);
                                *uffd.faulting_address.exclusive_access() = stval;
                                *uffd.faulting_task.exclusive_access() = Some(task.clone());
                                // 唤醒一个阻塞在 read(uffd) 上的 handler 线程
                                let mut guard = uffd.read_waiters.exclusive_access();
                                if let Some(handler) = guard.pop_front() {
                                    drop(guard);
                                    let mut h_inner = handler.inner_exclusive_access();
                                    h_inner.task_status = TaskStatus::Ready;
                                    h_inner.owner_hart = None;
                                    drop(h_inner);
                                    add_task(handler);
                                }
                                uffd_handled = true;
                                break;
                            }
                        }
                    }
                }

                if uffd_handled {
                    // 释放所有锁，挂起当前缺页线程，等待 UFFDIO_COPY 唤醒
                    drop(process_inner);
                    drop(process);
                    drop(task);
                    suspend_current_and_run_next();
                } else {
                    error!(
                        "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}, sp={:#x}",
                        crate::task::current_task().unwrap().process().pid.0,
                        scause.cause(),
                        current_trap_cx().get_rt(),
                        stval,
                        sp
                    );
                    process_inner.info_map_areas();
                    drop(process_inner);
                    drop(process);
                    drop(task);
                    current_add_signal(SignalFlags::SIGSEGV);
                }
            }
        }
        _ => {
            error!(
                "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}",
                crate::task::current_task().unwrap().process().pid.0,
                scause.cause(),
                current_trap_cx().get_rt(),
                stval
            );
            error!("[kernel] Trap! Source: User");
            error!("[kernel] Scause: {:?} (Code: {})", scause.cause(), scause.bits());
            error!("[kernel] Stval:  {:#x} (Bad Address)", stval);
            error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                scause.cause(),
                current_task().unwrap().process().pid.0,
                stval,
                current_trap_cx().get_rt(),
            );
            current_add_signal(SignalFlags::SIGSEGV);
        }
    }
    //let cause = scause::read().cause();
    //println!("[PROBE 2] trap_handler ending (cause: {:?}), preparing to handle_signals", cause);


    trap_return();
    
}

// 注意：不用VirtAddr包装，因为sv39要求高位符号扩展
pub fn current_trap_cx_user_va() -> usize {
    current_task().unwrap().kernel_stack.get_top() - KERNEL_STACK_SIZE
}

pub fn trap_cx_va_by_tid(tid: usize) -> usize {
    TRAMPOLINE - tid * (KERNEL_STACK_SIZE + PAGE_SIZE) - KERNEL_STACK_SIZE
}

pub fn trap_cx_va_by_kernel_stack(kernel_stack: &KernelStack) -> usize {
    let kernel_stack_top = kernel_stack.get_top();
    kernel_stack_top - KERNEL_STACK_SIZE
}

#[no_mangle]
/// return to user space
pub fn trap_return() -> ! {
    handle_signals();
    let term_signal = {
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        if inner.killed {
            inner.term_signal.unwrap_or(1)
        } else {
            0
        }
    };
    if term_signal != 0 {
        info!("[SIG PROBE] EXECUTING DEATH SENTENCE FOR PID!");
        exit_current_and_run_next(-term_signal);
    }
    set_user_trap_entry();
    let trap_cx_ptr = current_trap_cx_user_va();
    let user_satp = current_user_token();
    // println!("[kernel] trap_return: to user mode");
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    let restore_va = __restore as *const () as usize - __alltraps as *const () as usize + TRAMPOLINE;
    trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,
            in("a1") user_satp,
            options(noreturn)
        );
    }
}
#[no_mangle]
pub fn debug_info() {
    println!("2");
}


#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
pub fn trap_from_kernel() -> ! {
    use riscv::register::{satp, sepc, sstatus};
    let cause = scause::read().cause();
    let stval_v = stval::read();
    let sepc_v = sepc::read();
    let sstatus_v = sstatus::read().bits();
    let satp_v = satp::read().bits();
    let hart_id = crate::get_hart_id();

    error!(
        "[kernel][panic] trap_from_kernel: hart={}, cause={:?}, sepc={:#x}, stval={:#x}, sstatus={:#x}, satp={:#x}",
        hart_id,
        cause,
        sepc_v,
        stval_v,
        sstatus_v,
        satp_v
    );

    if let Some(task) = crate::task::current_task() {
        error!(
            "[kernel][panic] current task snapshot: pid={}, tid={}, task_ptr={:#x}",
            task.getpid(),
            task.gettid(),
            (&*task) as *const _ as usize
        );
    } else {
        error!("[kernel][panic] no current task on this hart");
    }

    panic!(
        "a trap {:?} from kernel! sepc={:#x}, stval={:#x}, hart={}.",
        cause,
        sepc_v,
        stval_v,
        hart_id
    );
}

pub use context::TrapContext;
