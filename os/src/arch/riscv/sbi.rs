//! SBI call wrappers

#![allow(unused)]

use core::arch::asm;

// 如果用qemu8，下列需要修改
// const SBI_SET_TIMER: usize = 0;//qemu7
const SBI_SET_TIMER: usize = 0x54494D45;//qemu8，采用ascii码
const SBI_CONSOLE_PUTCHAR: usize = 1;
const SBI_CONSOLE_GETCHAR: usize = 2;
// const SBI_SHUTDOWN: usize = 8;//qemu7
const SBI_SHUTDOWN: usize = 0x53525354;//qemu8

const SBI_IPI_SEND: usize = 0x735049;// sPI

// 跨核 TLB 刷新
const SBI_EXT_RFENCE: usize = 0x52464E43; // "RFNC"
const SBI_EXT_RFENCE_REMOTE_SFENCE_VMA: usize = 1;
const SBI_EXT_RFENCE_REMOTE_SFENCE_VMA_ASID: usize = 2;

// HSM (Hart State Management) 扩展 ID, "HSM"
const SBI_HSM: usize = 0x48534D;

const SBI_EXT_IPI: usize = 0x735049;
const SBI_IPI_SEND_IPI: usize = 0;

/// general sbi call
#[inline(always)]
pub fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x16") 0,
            in("x17") which,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn sbi_call_ext(which: usize, ext: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x16") ext,
            in("x17") which,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn sbi_call_ext5(
    which: usize,
    ext: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> usize {
    let mut ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x13") arg3,
            in("x14") arg4,
            in("x16") ext,
            in("x17") which,
            options(nostack),
        );
    }
    ret
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
pub fn shutdown() {
    sbi_call(SBI_SHUTDOWN, 0, 0, 0);
    println!("It should shutdown!");
}

pub fn send_ipi(mask: usize) {
    sbi_call(SBI_IPI_SEND, mask, 0, 0);
}

pub fn start_hart(hart_id: usize, start_addr: usize, opaque: usize) {
    let ret = sbi_call(SBI_HSM, hart_id, start_addr, opaque);
    println!("[sbi-debug] start_hart({}) addr={:#x} ret={:#x}", hart_id, start_addr, ret);
}

pub fn sbi_wakeup_hart(hart_id: usize) {
    let hart_mask = 1usize << hart_id;
    let hart_mask_base = 0;
    sbi_call(SBI_EXT_IPI, SBI_IPI_SEND_IPI, hart_mask, hart_mask_base);
}

pub fn sbi_wakeup_harts(hart_mask: usize) {
    let hart_mask_base = 0;
    sbi_call(SBI_EXT_IPI, SBI_IPI_SEND_IPI, hart_mask, hart_mask_base);
}

/// 让 mask 指定的核刷新 [start, start+size) 区间。
///
/// 全部完成后返回
pub fn remote_sfence_vma(hart_mask: usize, start: usize, size: usize) {
    let error = sbi_call_ext5(
        SBI_EXT_RFENCE,
        SBI_EXT_RFENCE_REMOTE_SFENCE_VMA,
        hart_mask,
        0,
        start,
        size,
        0,
    );
    assert_eq!(error, 0, "SBI remote_sfence_vma failed: {:#x}", error);
}

/// Synchronously invalidate every translation tagged with `asid` on `hart_mask`.
pub fn remote_sfence_vma_asid(hart_mask: usize, asid: usize) {
    let error = sbi_call_ext5(
        SBI_EXT_RFENCE,
        SBI_EXT_RFENCE_REMOTE_SFENCE_VMA_ASID,
        hart_mask,
        0,
        0,
        0,
        asid,
    );
    assert_eq!(
        error,
        0,
        "SBI remote_sfence_vma_asid failed: {:#x}",
        error
    );
}
