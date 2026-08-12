//! SBI call wrappers

#![allow(unused)]

use core::arch::asm;
use core::sync::atomic::{AtomicU8, Ordering};

// 如果用qemu8，下列需要修改
// const SBI_SET_TIMER: usize = 0;//qemu7
const SBI_SET_TIMER: usize = 0x54494D45;//qemu8，采用ascii码
const SBI_CONSOLE_PUTCHAR: usize = 1;
const SBI_CONSOLE_GETCHAR: usize = 2;
// const SBI_SHUTDOWN: usize = 8;//qemu7
const SBI_SHUTDOWN: usize = 0x53525354;//qemu8

// 跨核 TLB 刷新
const SBI_EXT_RFENCE: usize = 0x52464E43; // "RFNC"
const SBI_EXT_RFENCE_REMOTE_SFENCE_VMA: usize = 1;
const SBI_EXT_RFENCE_REMOTE_SFENCE_VMA_ASID: usize = 2;

// HSM (Hart State Management) 扩展 ID, "HSM"
const SBI_HSM: usize = 0x48534D;

const SBI_EXT_IPI: usize = 0x735049;
const SBI_IPI_SEND_IPI: usize = 0;

const SBI_EXT_BASE: usize = 0x10;
const SBI_BASE_PROBE_EXTENSION: usize = 3;

const SBI_SUCCESS: isize = 0;
const SBI_IPI_UNKNOWN: u8 = 0;
const SBI_IPI_SUPPORTED: u8 = 1;
const SBI_IPI_UNSUPPORTED: u8 = 2;

/// SBI v0.2+ returns an error in `a0` and an optional value in `a1`.
#[derive(Clone, Copy, Debug)]
pub struct SbiRet {
    pub error: isize,
    pub value: usize,
}

/// Cached result of the SBI IPI extension probe.
///
/// The scheduler may issue a kick from a hot wakeup path, so extension probing
/// belongs to per-hart initialization rather than to that path.
static SBI_IPI_STATE: AtomicU8 = AtomicU8::new(SBI_IPI_UNKNOWN);

/// general sbi call
#[inline(always)]
pub fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            // SBI is allowed to return a value in a1.  Even legacy callers
            // ignore it, so it must still be declared clobbered to the Rust
            // compiler.
            inlateout("x11") arg1 => _,
            in("x12") arg2,
            in("x16") 0,
            in("x17") which,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
fn sbi_call_ext_ret(
    which: usize,
    ext: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
) -> SbiRet {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => error,
            inlateout("x11") arg1 => value,
            in("x12") arg2,
            in("x16") ext,
            in("x17") which,
            options(nostack),
        );
    }
    SbiRet {
        error: error as isize,
        value,
    }
}

#[inline(always)]
pub fn sbi_call_ext(which: usize, ext: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    sbi_call_ext_ret(which, ext, arg0, arg1, arg2).error as usize
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
            inlateout("x11") arg1 => _,
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

pub fn start_hart(hart_id: usize, start_addr: usize, opaque: usize) {
    let ret = sbi_call(SBI_HSM, hart_id, start_addr, opaque);
    println!("[sbi-debug] start_hart({}) addr={:#x} ret={:#x}", hart_id, start_addr, ret);
}

/// Probe the SBI IPI extension once before scheduler wakeups begin.
///
/// This kernel already relies on SBI v0.2 extensions for timer, HSM and
/// remote-fence services.  If firmware lacks IPI support, wakeup placement
/// remains correct but cannot provide the immediate remote-reschedule bound.
pub fn init_scheduler_ipi() -> bool {
    let cached = SBI_IPI_STATE.load(Ordering::Acquire);
    if cached != SBI_IPI_UNKNOWN {
        return cached == SBI_IPI_SUPPORTED;
    }

    let result = sbi_call_ext_ret(
        SBI_EXT_BASE,
        SBI_BASE_PROBE_EXTENSION,
        SBI_EXT_IPI,
        0,
        0,
    );
    let state = if result.error == SBI_SUCCESS && result.value != 0 {
        SBI_IPI_SUPPORTED
    } else {
        SBI_IPI_UNSUPPORTED
    };
    match SBI_IPI_STATE.compare_exchange(
        SBI_IPI_UNKNOWN,
        state,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => state == SBI_IPI_SUPPORTED,
        Err(existing) => existing == SBI_IPI_SUPPORTED,
    }
}

/// Send an SBI scheduler IPI to a hardware hart.
///
/// `hart_mask = 1` and `hart_mask_base = hart_id` are the SBI-native form
/// used by Linux.  Unlike `1 << hart_id` with base zero, it remains valid for
/// sparse hart IDs and for IDs greater than or equal to XLEN.
pub fn sbi_wakeup_hart(hart_id: usize) -> bool {
    if SBI_IPI_STATE.load(Ordering::Acquire) == SBI_IPI_UNSUPPORTED {
        return false;
    }
    let result = sbi_call_ext_ret(
        SBI_EXT_IPI,
        SBI_IPI_SEND_IPI,
        1,
        hart_id,
        0,
    );
    result.error == SBI_SUCCESS
}

pub fn sbi_wakeup_harts(hart_mask: usize) -> bool {
    if SBI_IPI_STATE.load(Ordering::Acquire) == SBI_IPI_UNSUPPORTED {
        return false;
    }
    let hart_mask_base = 0;
    let result = sbi_call_ext_ret(
        SBI_EXT_IPI,
        SBI_IPI_SEND_IPI,
        hart_mask,
        hart_mask_base,
        0,
    );
    result.error == SBI_SUCCESS
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
