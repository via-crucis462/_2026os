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
use crate::arch::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};
use crate::mm::VirtAddr;
use crate::net::net_poll;
use crate::syscall::syscall;
use crate::task::{
    add_task, current_add_signal, current_task, current_tid, current_trap_cx, current_user_token,
    exit_current_and_run_next, handle_signals, suspend_current_and_run_next, KernelStack,
    SignalFlags, TaskStatus,
};
use crate::{get_hart_id, get_time_ms, KERNEL_STACK_SIZE, PAGE_SIZE};
use alloc::sync::Arc;

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
        riscv::register::sie::set_stimer();
    }
    println!("[timer] STIE enabled");
    #[cfg(board = "visionfive2")]
    unsafe {
        riscv::register::sstatus::set_sie();
    }
    println!("[timer] global SIE enabled");
}

/// trap handler
#[no_mangle]
pub fn trap_handler() -> ! {
    //println!("[kernel] trap_handler called CPU ID: {}", get_hart_id());
    trace!("[kernel] trap_handler: a trap from user space");
    let scause = riscv::register::scause::read();
    let sepc = riscv::register::sepc::read();
    let stval = riscv::register::stval::read();

    debug!(
        "trap_handler: cause: {:?}, sepc: 0x{:x}, stval: 0x{:x}", 
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
                [cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]],
            );
            current_task().unwrap().inner_exclusive_access().errno =
                if result == crate::syscall::errno::Errno::ERESTART.as_isize() {
                    0
                } else if result < 0 {
                    (-result) as i32
                } else {
                    0
                };
            if result < 0 {
                warn!(
                    "pid[{}] syscall {} returned error code {}",
                    current_task().unwrap().getpid(),
                    cx.x[17],
                    result
                );
            }
            // cx is changed during sys_exec, so we have to call it again
            //println!("[kernel] syscall: id={}, args={:x?}, ret=0x{:x}", cx.x[17], [cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]], result);
            cx = current_trap_cx();
            if result == crate::syscall::errno::Errno::ERESTART.as_isize() {
                cx.set_rt(cx.get_rt() - 4);
            } else {
                cx.set_a0(result as usize);
            }
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            let current_ms = get_time_ms();
            //定时打印clone的子进程数量
            /*let last_print_ms = LAST_CLONE_COUNT_PRINT_MS.load(Ordering::Relaxed);
            if current_ms.saturating_sub(last_print_ms) >= CLONE_COUNT_PRINT_INTERVAL_MS
                && LAST_CLONE_COUNT_PRINT_MS
                    .compare_exchange(
                        last_print_ms,
                        current_ms,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
            {
                if let Some(current) = current_task() {
                    let pid = current.getpid();
                    let current_tid = current.gettid();
                    let live_tasks = crate::process::registry::TID2TCB
                        .exclusive_access()
                        .values()
                        .filter(|task| task.getpid() == pid)
                        .count();
                    println!(
                        "[CLONE COUNT] pid={} current_tid={} live_tasks={} clone_children={}",
                        pid,
                        current_tid,
                        live_tasks,
                        live_tasks.saturating_sub(1)
                    );
                }
            }*/
            crate::timer::check_timers();
            net_poll();
            crate::mm::mmap::tick_sync();
            suspend_current_and_run_next();
        }
        Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::LoadPageFault)
        | Trap::Exception(Exception::InstructionPageFault) => {
            // 将 pagefault 处理放在独立块中，保证锁和 Arc 自动释放
            //
            // 龙芯的旧实现在每个分支处理成功后，drop 遗漏了 mm 的 Arc，
            // 却直接调用 trap_return，导致出现内存泄露
            //
            // riscv 旧实现无此问题，但统一修改一下，避免未来遗忘
            'fault: {
                let Some(task) = current_task() else {
                    break 'fault;
                };
                let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
                    current_add_signal(SignalFlags::SIGSEGV);
                    break 'fault;
                };
                let files = task.inner_exclusive_access().files.clone();
                let mut memory = mm.write();

                // 【修改 1】：获取当前的栈指针 SP
                let sp = current_trap_cx().x[2];
                let vpn = VirtAddr::from(stval).std_floor();
                //
                if scause.cause() == Trap::Exception(Exception::StorePageFault)
                    && memory.set_pte_dirty(vpn)
                {
                    break 'fault;
                } else if memory.handle_cow_fault(stval) {
                    info!("[WATCHDOG][COW] : {:#x}, PC: {:#x}", stval, sepc);
                    break 'fault;
                } else if memory.handle_page_fault(stval, sp) {
                    info!("[WATCHDOG] : {:#x}, PC: {:#x}", stval, sepc);
                    break 'fault;
                } else if memory.check_mmap_page_fault(stval){
                    error!("[WATCHDOG][BUS] : {:#x}, PC: {:#x}", stval, sepc);
                    error!(
                        "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}",
                        task.getpid(),
                        scause.cause(),
                        current_trap_cx().get_rt(),
                        stval
                    );
                    error!("[kernel] Trap! Source: User");
                    error!(
                        "[kernel] Scause: {:?} (Code: {})",
                        scause.cause(),
                        scause.bits()
                    );
                    error!("[kernel] Stval:  {:#x} (Bad Address)", stval);
                    error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                    scause.cause(),
                    task.getpid(),
                    stval,
                    current_trap_cx().get_rt(),
                );
                    // 触发了文件映射区域的page fault，说明是超出文件大小访问了，发送SIGBUS信号
                    current_add_signal(SignalFlags::SIGBUS);
                    break 'fault;
                } else {
                    // 处理并发缺页
                    //
                    // 另一个线程可能刚刚处理了缺页，但此线程已经触发了缺页异常：
                    // 检查页表，如果页存在且已经满足了访问权限要求，直接返回。
                    let bad_vpn = VirtAddr::from(stval).std_floor();
                    let retry = match scause.cause() {
                        Trap::Exception(Exception::InstructionPageFault) => {
                            memory.pte_satisfies(bad_vpn, false, false, true)
                        }
                        Trap::Exception(Exception::LoadPageFault) => {
                            memory.pte_satisfies(bad_vpn, true, false, false)
                        }
                        Trap::Exception(Exception::StorePageFault) => {
                            memory.pte_satisfies(bad_vpn, false, true, false)
                        }
                        _ => false,
                    };
                    if retry {
                        break 'fault;
                    }

                    // 【新增】检查 userfaultfd 注册范围
                    drop(memory);
                let files_guard = files.exclusive_access();
                let fd_table = &files_guard.fds;
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
                for fd_entry in fd_table.iter() {
                    if let Some(file) = &fd_entry.file {
                        if let Some(uffd) =
                            file.as_any().downcast_ref::<crate::fs::UserPageFaultInfo>()
                        {
                            println!("Checking UFFD registered ranges for PID {}...", task.getpid());
                            let in_range = uffd.registered_ranges.lock()
                                .iter().map(|&(start, len)| {
                                    println!("  Comparing fault address 0x{:x} with registered range 0x{:x} - 0x{:x}", stval, start, start + len);
                                    stval >= start && stval < start + len
                                }).any(|x| x);
                            if in_range {
                                println!("Page fault address 0x{:x} is within a registered UFFD range, handling with UFFD", stval);
                                *uffd.faulting_address.lock() = stval;
                                *uffd.faulting_task.lock() = Some(task.clone());
                                // 唤醒一个阻塞在 read(uffd) 上的 handler 线程
                                let mut guard = uffd.read_waiters.lock();
                                if let Some(handler) = guard.pop_front() {
                                    drop(guard);
                                    let mut h_inner = handler.inner_exclusive_access();
                                    h_inner.state = TaskStatus::Ready;
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
                    drop(files_guard);
                    drop(mm);
                    drop(files);
                    drop(task);
                    suspend_current_and_run_next();
                } else {
                    let exe_path = task.inner_exclusive_access().exe_path.clone();
                    println!(
                        "[user-fault] pid={} tid={} exe={} cause={:?} pc={:#x} badaddr={:#x} sp={:#x}",
                        task.getpid(),
                        task.gettid(),
                        exe_path,
                        scause.cause(),
                        current_trap_cx().get_rt(),
                        stval,
                        sp,
                    );
                    error!(
                        "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}, sp={:#x}",
                        task.getpid(),
                        scause.cause(),
                        current_trap_cx().get_rt(),
                        stval,
                        sp
                    );
                    drop(files_guard);
                    current_add_signal(SignalFlags::SIGSEGV);
                }
                }
            }
        }
        _ => {
            let task = crate::task::current_task().unwrap();
            let exe_path = task.inner_exclusive_access().exe_path.clone();
            println!(
                "[user-fault] pid={} tid={} exe={} cause={:?} pc={:#x} badaddr={:#x}",
                task.getpid(),
                task.gettid(),
                exe_path,
                scause.cause(),
                current_trap_cx().get_rt(),
                stval,
            );
            error!(
                "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}",
                task.getpid(),
                scause.cause(),
                current_trap_cx().get_rt(),
                stval
            );
            error!("[kernel] Trap! Source: User");
            error!(
                "[kernel] Scause: {:?} (Code: {})",
                scause.cause(),
                scause.bits()
            );
            error!("[kernel] Stval:  {:#x} (Bad Address)", stval);
            error!(
                "[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                scause.cause(),
                current_task().unwrap().getpid(),
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

// The TrapContext lives on the real kernel stack. Every user page table shares
// the kernel's supervisor-only high-half mappings, so trap entry can save
// registers before switching SATP.
pub fn current_trap_cx_user_va() -> usize {
    current_task()
        .unwrap()
        .inner_exclusive_access()
    .thread
    .trap_ctx
}

pub fn trap_cx_va_by_tid(tid: usize) -> usize {
    TRAMPOLINE - tid * (KERNEL_STACK_SIZE + PAGE_SIZE) - KERNEL_STACK_SIZE
}

pub fn trap_cx_va_by_kernel_stack(kernel_stack: &KernelStack) -> usize {
    kernel_stack.position_for::<TrapContext>()
}

#[no_mangle]
/// return to user space
pub fn trap_return() -> ! {
    handle_signals();
    let term_signal = current_task()
        .unwrap()
        .inner_exclusive_access()
        .term_signal;
    if let Some(signal) = term_signal {
        exit_current_and_run_next(-signal);
    }
    set_user_trap_entry();
    let trap_cx_ptr = current_trap_cx_user_va();
    let user_satp = current_user_token();
    crate::mm::switch_mm(user_satp);
    // println!("[kernel] trap_return: to user mode");
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    let restore_va =
        __restore as *const () as usize - __alltraps as *const () as usize + TRAMPOLINE;
    trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,
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
        "[kernel][panic] trap_from_kernel: hart={}, cause={:?}, sepc=0x{:x}, stval=0x{:x}, sstatus=0x{:x}, satp=0x{:x}",
        hart_id,
        cause,
        sepc_v,
        stval_v,
        sstatus_v,
        satp_v
    );

    if let Some(task) = crate::task::current_task() {
        error!(
            "[kernel][panic] current task snapshot: pid={}, tid={}, task_ptr=0x{:x}",
            task.getpid(),
            task.gettid(),
            (&*task) as *const _ as usize
        );
    } else {
        error!("[kernel][panic] no current task on this hart");
    }

    panic!(
        "a trap {:?} from kernel! sepc={:#x}, stval={:#x}, hart={}.",
        cause, sepc_v, stval_v, hart_id
    );
}

pub use context::TrapContext;
