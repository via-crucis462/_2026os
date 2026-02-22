// 正在为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html
#![allow(unused)]
mod context;

use crate::arch::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};

use crate::debug_csr_info;
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
            //会自动截断高位，转成低半地址空间的地址
            trap_handler = in(reg) VirtAddr::from(TRAMPOLINE).0,
        );
    }
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    crate::arch::timer::init_board_freq();
    unsafe {
        asm!("csrwr {}, 0x44", in(reg) 1);// 清除定时器中断
        let tcfg: usize = 0x100000 | 0b11;// 循环模式并开启中断，周期0x100000
        asm!("csrwr {}, 0x41", in(reg) tcfg);
        let mut ecfg: usize;
        asm!("csrrd {}, 0x4", out(reg) ecfg);
        asm!("csrwr {}, 0x4", in(reg) ecfg | (1 << 11)); // 使能定时器中断
        let mut crmd: usize;
        asm!("csrrd {}, 0x0", out(reg) crmd);
        asm!("csrwr {}, 0x0", in(reg) crmd | (1 << 2)); // 使能中断
    }
    debug_csr_info();
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
    println!("[kernel] called trap_handler");
    set_kernel_trap_entry();
    let estat = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x5", out(reg) t);
        t
    };
    // 出错虚地址
    let badv = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x7", out(reg) t);
        t
    };
    let cause = if ((estat >> 11) & 1)  != 0 {
        Cause::TimeInterrupt
    } else if ((estat >> 16) & 0x3fff) == 0xB {
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
                cx.r[4] = result as usize;
            }
            Cause::TimeInterrupt => {
                unsafe {
                    asm!("csrwr {}, 0x44", in(reg) 1);// 清除定时器中断
                }
                suspend_current_and_run_next();
            }
            _ => {
                error!("[kernel] trap_handler: {:?} in PID {}, bad addr = {:#x}, bad instruction = {:#x}",
                    cause,
                    crate::task::current_task().unwrap().pid.0,
                    badv,
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
/// 参考riscv的实现，小幅度修改
pub fn trap_return() -> ! {
    set_user_trap_entry();
    let trap_cx_ptr = TRAP_CONTEXT_BASE;
    let user_satp = current_user_token();
 
    // println!("[kernel] trap_return: to user mode");
    info!("[kernel] trap_return: to user mode, satp = {:#x}", user_satp);
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    // 相对地址+跳板基址
    let restore_va =
        __restore as *const () as usize - 
        __alltraps as *const () as usize + 
        TRAMPOLINE;
    // trace!("[kernel] trap_return: ..before return");
    // 初始化内存空间到用户态
    crate::arch::mm::la_app_init_mem(user_satp);
    unsafe {
        asm!(
            "dbar 0",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("$a0") trap_cx_ptr,
            in("$a1") user_satp,
            options(noreturn)
        );
    }
}

#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
/// 在我们现在的实现中，io设备不需要使用这个函数，但尊重rcore原作先不删除
pub fn trap_from_kernel() -> ! {
    panic!("a trap from kernel!");
}

pub use context::TrapContext;
