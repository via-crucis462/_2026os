// 正在为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html
#![allow(unused)]
mod context;

use crate::arch::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};

use crate::syscall::syscall;
use crate::task::{
    check_signals_error_of_current, current_add_signal, current_trap_cx, current_user_token,
    exit_current_and_run_next, handle_signals, suspend_current_and_run_next, SignalFlags,
};
use crate::mm::VirtAddr;
use crate::arch::timer::set_next_trigger;
use crate::arch::mm::tlb_refill_handler;

use core::arch::{asm, global_asm};

global_asm!(include_str!("trap.S"));

/// Initialize trap handling
pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    unsafe {
        asm!(
            "csrwr {trap_handler},0xe",// 设置EENTRY
            trap_handler = in(reg) trap_handler as *const() as usize,
        );
    }
}

/// 插入跳板即可
fn set_user_trap_entry() {
    unsafe {
        asm!(
            "csrwr {trap_handler},0xe",
            //需要截断高位，转成低半地址空间的地址
            trap_handler = in(reg) VirtAddr::from(TRAMPOLINE).0,
        );
    }
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    unsafe {
        let mut ecfg: usize;
        asm!("csrrd {}, 0x41", out(reg) ecfg);
        asm!("csrwr {}, 0x41", in(reg) ecfg | (1 << 11),);
    }
}


#[derive(Debug)]
enum Cause {
    Syscall,
    TimeInterrupt,
    Other,
}

/// trap handler
/// 初始化EENTRY要指向这里
/// 从riscv的trap handler改写
/// 参考https://www.loongson.cn/uploads/images/2023041918133323805.%E9%BE%99%E8%8A%AF%E6%9E%B6%E6%9E%84%E5%8F%82%E8%80%83%E6%89%8B%E5%86%8C%E5%8D%B7%E4%B8%80_r1p03.pdf
/// 的111页和97页
#[no_mangle]
pub fn trap_handler() -> ! {

    set_kernel_trap_entry();
    let estat = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x5", out(reg) t);
        t
    };
    let stval = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x43", out(reg) t);
        t
    };
    let cause = if ((estat >> 11) & 1)  != 0 {
        Cause::TimeInterrupt
    } else if ((estat >> 16) & 0xB) != 0 {
        Cause::Syscall
    } else {
        Cause::Other
    };
    
        match cause {
            Cause::Syscall => {
                let mut cx = current_trap_cx();
                cx.set_rt(cx.get_rt() + 4);
                // get system call return value
                let result = syscall(
                    cx.r[11], 
                    [cx.r[4], cx.r[5], cx.r[6], cx.r[7], cx.r[8], cx.r[9]]
                );
                // cx is changed during sys_exec, so we have to call it again
                cx = current_trap_cx();
                cx.r[10] = result as *const () as usize;
            }
            Cause::TimeInterrupt => {
                set_next_trigger();
                suspend_current_and_run_next();
            }
            _ => {
                error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                    cause,
                    crate::task::current_task().unwrap().pid.0,
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
    trap_return();
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
    trace!("stval = {:#r}, sepc = {:#r}", stval::read(), sepc::read());
    panic!("a trap {:?} from kernel!", scause::read().cause());
    */
}

pub use context::TrapContext;
