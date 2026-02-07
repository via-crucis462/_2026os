// 为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html

mod context;
/* 
use crate::arch::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};

use crate::syscall::syscall;
use crate::task::{
    check_signals_error_of_current, current_add_signal, current_trap_cx, current_user_token,
    exit_current_and_run_next, handle_signals, suspend_current_and_run_next, SignalFlags,
};
use crate::arch::timer::set_next_trigger;

use core::arch::{asm, global_asm};

global_asm!(include_str!("trap.S"));
*/
/// Initialize trap handling
pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    // TODO
}

fn set_user_trap_entry() {
    // TODO
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    // TODO
}

/// trap handler
/// 初始化EENTRY要指向这里
#[no_mangle]
pub fn trap_handler() -> ! {
    loop {
        
    }
    /* 
    set_kernel_trap_entry();
    let scause = scause::read();
    let stval = stval::read();
    
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            let mut cx = current_trap_cx();
            cx.sepc += 4;
            // get system call return value
            let result = syscall(
                cx.x[17], 
                [cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]]
            );
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();
            cx.x[10] = result as *const () as usize;
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();
            suspend_current_and_run_next();
        }
        _ => {
            error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                scause.cause(),
                crate::task::current_task().unwrap().pid.0,
                stval,
                current_trap_cx().sepc,
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
    trap_return();
    */
}

#[no_mangle]
/// return to user space
pub fn trap_return() -> ! {
    /*
    set_user_trap_entry();
    let _trap_cx_ptr = TRAP_CONTEXT_BASE;
    // let _user_satp = current_user_token();
    // println!("[kernel] trap_return: to user mode");
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    let restore_va = __restore as *const () as usize - __alltraps as *const () as usize + TRAMPOLINE;
    // trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            //in("a0") trap_cx_ptr,
            //in("a1") user_satp,
            options(noreturn)
        );
    }
    */
    loop {
        
    }
}

#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
pub fn trap_from_kernel() -> ! {
    loop {
        
    }
    /* 
    use riscv::register::sepc;
    trace!("stval = {:#x}, sepc = {:#x}", stval::read(), sepc::read());
    panic!("a trap {:?} from kernel!", scause::read().cause());
    */
}

pub use context::TrapContext;
