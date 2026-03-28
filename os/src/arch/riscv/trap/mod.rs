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
    KernelStack, SignalFlags, check_signals_error_of_current, current_add_signal, current_task, current_tid, current_trap_cx, current_user_token, exit_current_and_run_next, handle_signals, suspend_current_and_run_next
};
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

    /*log::debug!(
        "trap_handler: cause: {:?}, sepc: {:#x}, stval: {:#x}", 
        scause.cause(), 
        sepc, 
        stval
    );*/
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
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();
            cx.set_a0(result as usize);
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();
            net_poll();
            suspend_current_and_run_next();
        }
        Trap::Exception(Exception::StorePageFault) |
        Trap::Exception(Exception::LoadPageFault) |
        Trap::Exception(Exception::InstructionPageFault) => {
            let task = current_task().unwrap();
            let process = task.process(); 
            let mut process_inner = process.inner_exclusive_access();
            
            // 【修改 1】：获取当前的栈指针 SP
            let sp = current_trap_cx().x[2];
            
            // 【修改 2】：把 sp 传进去，支持动态扩栈
            if process_inner.memory_set.handle_page_fault(stval, sp) {
                // 修复成功！释放锁
                drop(process_inner);
                drop(process);
                drop(task);
            } else {
                drop(process_inner);
                drop(process);
                drop(task);
                
                println!(
                    "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}, sp={:#x}",
                    crate::task::current_task().unwrap().process().pid.0,
                    scause.cause(),
                    current_trap_cx().get_rt(),
                    stval,
                    sp
                );
                
                // 取消原来的 current_add_signal(SignalFlags::SIGSEGV);
                // 发信号压栈死循环。
                // 直接以 11 (SIGSEGV的默认信号值) 退出码击毙当前进程！
                crate::task::exit_current_and_run_next(11);
            }
        }
        _ => {
            println!(
                "[kernel] user_fault: pid={}, cause={:?}, pc={:#x}, badaddr={:#x}",
                crate::task::current_task().unwrap().process().pid.0,
                scause.cause(),
                current_trap_cx().get_rt(),
                stval
            );
            println!("[kernel] Trap! Source: User");
            println!("[kernel] Scause: {:?} (Code: {})", scause.cause(), scause.bits());
            println!("[kernel] Stval:  {:#x} (Bad Address)", stval);
            error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                scause.cause(),
                current_task().unwrap().process().pid.0,
                stval,
                current_trap_cx().get_rt(),
            );
            current_add_signal(SignalFlags::SIGSEGV);
        }
    }
    handle_signals();

    // check error signals (if error then exit)
    if let Some((errno, msg)) = check_signals_error_of_current() {
        trace!("[kernel] trap_handler: .. check signals {}", msg);
        exit_current_and_run_next(errno);
    }
    let killed = current_task().unwrap().inner_exclusive_access().killed;
    if killed {
        // -1 代表异常退出，如果是 SIGSEGV，也可以传它的信号值 (比如 11)
        exit_current_and_run_next(-1); 
    }
    let final_cx = current_trap_cx();
    if final_cx.get_rt() == 0xfffffffffffffffe {
        println!("\n[BINGO] sepc became -2 just before returning to user space!");
        println!("Syscall ID (a7) was: {}", final_cx.x[17]);
        println!("Return value (a0) is: {}", final_cx.x[10]);
        println!("Current PID: {}", current_task().unwrap().tid.0);
        loop {} // 冻结 CPU！
    }
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

    println!(
        "[kernel][panic] trap_from_kernel: hart={}, cause={:?}, sepc={:#x}, stval={:#x}, sstatus={:#x}, satp={:#x}",
        hart_id,
        cause,
        sepc_v,
        stval_v,
        sstatus_v,
        satp_v
    );

    if let Some(task) = crate::task::current_task() {
        println!(
            "[kernel][panic] current task snapshot: pid={}, tid={}, task_ptr={:#x}",
            task.getpid(),
            task.gettid(),
            (&*task) as *const _ as usize
        );
    } else {
        println!("[kernel][panic] no current task on this hart");
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
