//! SBI call wrappers

#![allow(unused)]

use core::arch::asm;
use crate::sync::MPSafeCell;
use lazy_static::*;

// 如果用qemu8，下列需要修改
// const SBI_SET_TIMER: usize = 0;//qemu7
const SBI_SET_TIMER: usize = 0x54494D45;//qemu8，采用ascii码
const SBI_CONSOLE_PUTCHAR: usize = 1;
const SBI_CONSOLE_GETCHAR: usize = 2;
// const SBI_SHUTDOWN: usize = 8;//qemu7
const SBI_SHUTDOWN: usize = 0x53525354;//qemu8

const SBI_IPI_SEND: usize = 0x735049;// sPI

// HSM (Hart State Management) 扩展 ID, "HSM"
const SBI_HSM: usize = 0x48534D;

struct SBICaller{}

impl SBICaller {
    #[allow(unused)]
    pub fn call(&mut self, which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
        let mut ret;
        unsafe {
            asm!(
                "ecall",
                inlateout("x10") arg0 => ret,
                in("x11") arg1,
                in("x12") arg2,
                in("x16") 0,
                in("x17") which,
            );
        }
        ret
    }
}

lazy_static! {
    static ref SBI_CALLER: MPSafeCell<SBICaller> = MPSafeCell::new(SBICaller{});
}

/// general sbi call
#[inline(always)]
pub fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    SBI_CALLER.exclusive_access().call(which, arg0, arg1, arg2)
}


/// use sbi call to set timer
pub fn set_timer(timer: usize) {
    sbi_call(SBI_SET_TIMER, timer, 0, 0);
}

/// use sbi call to putchar in console (qemu uart handler)
pub fn console_putchar(c: usize) {
    sbi_call(SBI_CONSOLE_PUTCHAR, c, 0, 0);
}

/// use sbi call to getchar from console (qemu uart handler)
pub fn console_getchar() -> usize {
    sbi_call(SBI_CONSOLE_GETCHAR, 0, 0, 0)
}

/// use sbi call to shutdown the kernel
pub fn shutdown() -> ! {
    sbi_call(SBI_SHUTDOWN, 0, 0, 0);
    panic!("It should shutdown!");
}

pub fn send_ipi(mask: usize) {
    sbi_call(SBI_IPI_SEND, mask, 0, 0);
}

pub fn start_hart(hart_id: usize, start_addr: usize, opaque: usize) {
    sbi_call(SBI_HSM, hart_id, start_addr, opaque);
}