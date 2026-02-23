// 正在为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html
#![allow(unused)]
mod context;


use crate::arch::trap;
use crate::debug_csr_info;
use crate::syscall::syscall;
use crate::task::{
    check_signals_error_of_current, current_add_signal, current_trap_cx, current_user_token,
    exit_current_and_run_next, handle_signals, suspend_current_and_run_next, SignalFlags,
};
use crate::mm::{VirtAddr, translated_byte_buffer};
use crate::arch::timer::set_next_trigger;
use crate::arch::mm::tlb_refill_handler;

use core::arch::{asm, global_asm};

global_asm!(include_str!("trap.S"));

extern "C" {
    fn __alltraps();
}

#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
/// 在我们现在的实现中，io设备不需要使用这个函数，先不删除
/// 改为调试信息打印
pub fn trap_from_kernel() -> ! {
    let estat: usize;
    let era: usize;
    let badv: usize;
    let badi: usize;
    unsafe {
        asm!("csrrd {estat}, 0x5", estat = out(reg) estat);
        asm!("csrrd {era}, 0x6", era = out(reg) era);
        asm!("csrrd {badv}, 0x7", badv = out(reg) badv);
        asm!("csrrd {badi}, 0x8", badi = out(reg) badi);
    }
    error!(
        "[kernel] trap_from_kernel: ESTAT={:#x}, ERA={:#x}, BADV={:#x}, BADI={:#x}",
        estat,
        era,
        badv,
        badi
    );
    loop {
        // 死循环
    }
}

/// Initialize trap handling
pub fn init() {
    set_kernel_trap_entry();
}

// 从内核trap时
fn set_kernel_trap_entry() {
    let target = trap_from_kernel as *const () as usize;
    let mut trap_handler: usize = target;
    unsafe {
        asm!(
            "csrwr {trap_handler},0xc",
            trap_handler = inout(reg) trap_handler,
        );
    }
    let mut eentry: usize;
    unsafe {
        asm!("csrrd {eentry}, 0xc", eentry = out(reg) eentry);
    }
    info!(
        "[kernel] set_kernel_trap_entry: target={:#x}, eentry={:#x}",
        target,
        eentry
    );
}

/// 插入__all_trap的地址
/// 当发生trap时，硬件会切换权限级，这时窗口映射生效
/// 直接访问0x9开始物理地址即可
fn set_user_trap_entry() {
    let target = __alltraps as *const () as usize;
    let mut trap: usize = target;
    unsafe {
        asm!(
            "csrwr {trap},0xc",
            trap = inout(reg) trap,
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
/// 从riscv的trap handler改写
/// 参考https://www.loongson.cn/uploads/images/2023041918133323805.%E9%BE%99%E8%8A%AF%E6%9E%B6%E6%9E%84%E5%8F%82%E8%80%83%E6%89%8B%E5%86%8C%E5%8D%B7%E4%B8%80_r1p03.pdf
/// 的111页和97页
#[no_mangle]
pub fn trap_handler() -> ! {
    println!("[kernel] called trap_handler");
    let estat = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x5", out(reg) t);
        t
    };
    // 具体需要查表，位于手册111页表格
    println!("[kernel] trap_handler: ESTAT={:#x}", estat);//11_0000_0000_0000_0000
    // 出错虚地址
    let badv = unsafe {
        let t: usize;
        asm!("csrrd {}, 0x7", out(reg) t);
        t
    };


    let cause = if ((estat >> 11) & 1)  != 0 {
        Cause::TimeInterrupt
    } else if ((estat >> 16) & 0x3fff) == 0xb {
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
/// 参考riscv的实现，修改内存相关
pub fn trap_return() -> ! {
    //set_user_trap_entry();
    // 直接用物理地址
    let trap_cx_ptr = current_trap_cx() as *mut TrapContext;
    let user_satp = current_user_token();
    
//    crate::arch::mm::la_app_init_mem(user_satp); //改为在restore中设置
    info!("[kernel] trap_return: going to user mode, satp = {:#x}", user_satp);
    
    
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    // la64因为是先切换特权级再跳，切换特权级时会自动关闭内存窗口可用性，不需要用跳板，restore直接跳转即可
    let restore = __restore as *const() as usize;

    let debug_buff = translated_byte_buffer(user_satp, 0x20_000 as *const u8, 128);
    println!("[kernel] trap_return: debug_buff = {:?}", &debug_buff[..]);
    // 1_001000_0000_0000_0000_0000 访存指令地址错例外，这表明页表并没有正确映射好
    println!("[kernel] calling __restore, address: {:#x}", restore);

    

    unsafe {
        asm!(
            "dbar 0", // 相当于sfence.vma
            "jr {restore}",
            restore = in(reg) restore,
            in("$a0") trap_cx_ptr,
            in("$a1") user_satp,
            options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn debug_print(){
    println!("[kernel] debug_print called");
}

#[no_mangle]
pub unsafe extern "C" fn csr_info(){
    let csr: usize;
    asm!("csrrd {}, 0x19", out(reg) csr);
    println!("[kernel] csr_info: CSR = {:#x}", csr );
}

pub use context::TrapContext;
