//! The main module and entrypoint
//!
//! Various facilities of the kernels are implemented as submodules. The most
//! important ones are:
//!
//! - [`trap`]: Handles all cases of switching from userspace to the kernel
//! - [`task`]: Task management
//! - [`syscall`]: System call handling and implementation
//! - [`mm`]: Address map using SV39
//! - [`sync`]: Wrap a static data structure inside it so that we are able to access it without any `unsafe`.
//! - [`fs`]: Separate user from file system with some structures
//!
//! The operating system also starts in this module. Kernel code starts
//! executing from `entry.asm`, after which [`rust_main()`] is called to
//! initialize various pieces of functionality. (See its source code for
//! details.)
//!
//! We then call [`task::run_tasks()`] and for the first time go to
//! userspace.

#![deny(warnings)]
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![allow(unused)]
#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate log;

extern crate alloc;

use core::arch::asm;
use core::sync::atomic::Ordering;

#[macro_use]
mod console;
pub mod arch;
pub mod drivers;
pub mod ext4fs;
pub mod fs;
pub mod lang_items;
pub mod logging;
//#[cfg(target_arch = "riscv64")]
pub mod net;
pub mod mm;
pub mod sync;
pub mod syscall;
pub mod process;
pub mod auth;
pub mod timer;
pub mod ipc;

pub mod init;

pub use arch::config::*;
pub use process::task;

#[allow(unused)]
use crate::arch::sbi::*;
use crate::arch::trap;
use core::arch::global_asm;
#[cfg(target_arch = "loongarch64")]
#[allow(unused)]
use crate::arch::la;

pub use arch::timer::*;

#[cfg(board = "virt")]
use crate::arch::drivers::NET_DEVICE;

use core::sync::atomic::{AtomicBool, AtomicUsize};
use lazy_static::*;
use spin::Mutex;

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("arch/riscv/entry.asm"));
#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("arch/la/entry.asm"));


#[link_section = ".data"]
pub static MAIN_HART_INITED: AtomicBool = AtomicBool::new(false);

#[link_section = ".data"]
pub static MAIN_HART_ID: AtomicUsize = AtomicUsize::new(0);


/// clear BSS segment (excluding boot stack)
/// boot stack must not be part of [sbss, ebss), otherwise we would zero the live stack.
#[inline(always)]
fn clear_bss() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    unsafe {
        core::slice::from_raw_parts_mut(sbss as *const () as usize as *mut u8, ebss as *const () as usize - sbss as *const () as usize)
            .fill(0);
    }
}


extern "C" {
    fn _start();
}

macro_rules! check_eq {
    ($got:expr, $expect:expr) => {{
        let g = $got;
        let e = $expect;
        if g != e {
            panic!("got=0x{:x}, expect=0x{:x}", g as usize as u64, e as usize as u64);
        }
    }};
    ($got:expr, $expect:expr, $($arg:tt)*) => {{
        let g = $got;
        let e = $expect;
        if g != e {
            panic!("{}: got=0x{:x}, expect=0x{:x}", format_args!($($arg)*),
                  g as usize as u64, e as usize as u64);
        }
    }};
}


#[no_mangle]
pub fn rust_main(hart_id: usize) -> ! {
    clear_bss();
    logging::init();
    mm::init();
    trap::init();
    println!("[kernel] Hello, world! hart_id={}", hart_id);

    // 读取 crmd / misc
    unsafe {
        let mut crmd: usize;
        asm!("csrrd {}, 0x0", out(reg) crmd);
        println!("[kernel] crmd=0x{:x}", crmd);
        let mut misc: usize;
        asm!("csrrd {}, 0x3", out(reg) misc);
        println!("[kernel] misc=0x{:x}", misc);
    }

    unaligned_test_si12();
    unaligned_test_rk();
    unaligned_test_cross();

    // ─── 人工可读验证：不依赖 read_unaligned，直接字节级算期望 ───
    verify_manual();

    let final_count = crate::arch::la::trap::K_TRAP_COUNT.load(core::sync::atomic::Ordering::SeqCst);
    println!("[kernel] All unaligned tests passed! total k_trap_count={}", final_count);
    panic!("main end");
}

// 从 16 字节数组 offset 处读取小端序 u16
fn expected_u16(buf: &[u8; 16], off: usize) -> u16 {
    buf[off] as u16 | ((buf[off + 1] as u16) << 8)
}
fn expected_u32(buf: &[u8; 16], off: usize) -> u32 {
    buf[off] as u32 | ((buf[off+1] as u32) << 8) | ((buf[off+2] as u32) << 16) | ((buf[off+3] as u32) << 24)
}
fn expected_u64(buf: &[u8; 16], off: usize) -> u64 {
    expected_u32(buf, off) as u64 | ((expected_u32(buf, off + 4) as u64) << 32)
}

fn verify_manual() {
    use core::sync::atomic::Ordering;
    let base = 0x9000_0000_0020_0000usize;

    // 写入 16 字节小端序 pattern，手动指定每个字节
    // pattern: bytes 0..15 = 00 11 22 33 44 55 66 77  88 99 AA BB CC DD EE FF
    let bytes: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
    ];
    unsafe {
        (base as *mut u64).write_volatile(u64::from_le_bytes(bytes[0..8].try_into().unwrap()));
        ((base + 8) as *mut u64).write_volatile(u64::from_le_bytes(bytes[8..16].try_into().unwrap()));
    }
    println!("[verify] wrote 16-byte pattern: 00 11 22 33 44 55 66 77  88 99 AA BB CC DD EE FF");

    macro_rules! verify_one {
        ($name:expr, $off:expr, $asm:expr, out($outreg:ident) $outvar:ident, $expect_fn:ident) => {
            let $outvar: usize;
            unsafe { ::core::arch::asm!($asm, out(reg) $outvar, in(reg) base); }
            let expect = $expect_fn(&bytes, $off);
            let got = $outvar as usize as u64;
            println!("[verify] {:>12} off={} expect=0x{:016x} got=0x{:016x} {}",
                $name, $off, expect as u64, got,
                if got == expect as u64 { "OK" } else { "FAIL" });
        };
    }

    // si12 reads（verify_one! 宏硬编码了 base，偏移在立即数中）
    verify_one!("ld.h  1", 1, "ld.h {}, {}, 1", out(r) r, expected_u16);
    verify_one!("ld.hu 3", 3, "ld.hu {}, {}, 3", out(r) r, expected_u16);
    verify_one!("ld.w  2", 2, "ld.w {}, {}, 2", out(r) r, expected_u32);
    verify_one!("ld.wu 6", 6, "ld.wu {}, {}, 6", out(r) r, expected_u32);
    verify_one!("ld.d  5", 5, "ld.d {}, {}, 5", out(r) r, expected_u64);

    // rk reads（手动构造 off_ptr，不能用 verify_one!）
    let off_ptr_1 = unsafe { base + 1 };
    let off_ptr_4 = unsafe { base + 4 };
    { let r: usize; unsafe { asm!("ldx.h {}, {}, $zero", out(reg) r, in(reg) off_ptr_1); }
      let e = expected_u16(&bytes, 1); let g = r as u64;
      println!("[verify] {:>12} off=1 expect=0x{:016x} got=0x{:016x} {}",
               "ldx.h +1", e as u64, g, if g==e as u64 {"OK"} else {"FAIL"}); }
    { let r: usize; unsafe { asm!("ldx.w {}, {}, $zero", out(reg) r, in(reg) (base+2)); }
      let e = expected_u32(&bytes, 2); let g = r as u64;
      println!("[verify] {:>12} off=2 expect=0x{:016x} got=0x{:016x} {}",
               "ldx.w +2", e as u64, g, if g==e as u64 {"OK"} else {"FAIL"}); }
    { let r: usize; unsafe { asm!("ldx.d {}, {}, $zero", out(reg) r, in(reg) off_ptr_4); }
      let e = expected_u64(&bytes, 4); let g = r as u64;
      println!("[verify] {:>12} off=4 expect=0x{:016x} got=0x{:016x} {}",
               "ldx.d +4", e, g, if g==e {"OK"} else {"FAIL"}); }

    // si12 stores (先重置 → 写入 → 对齐读回验证)
    unsafe {
        // st.h +7
        (base as *mut u64).write_volatile(0xFFFFFFFF_FFFFFFFFu64);
        ((base + 8) as *mut u64).write_volatile(0xFFFFFFFF_FFFFFFFFu64);
        asm!("st.h {}, {}, 7", in(reg) 0xABCDu16, in(reg) base);
        let got = u16::from_le_bytes([
            *((base + 7) as *const u8),
            *((base + 8) as *const u8),
        ]);
        println!("[verify] {:>12} off=7 val=0xABCD expect=0xABCD got=0x{:04x} {}",
            "st.h", got, if got == 0xABCD { "OK" } else { "FAIL" });

        // stx.w +3
        (base as *mut u64).write_volatile(0xFFFFFFFF_FFFFFFFFu64);
        asm!("stx.w {}, {}, $zero", in(reg) 0x12345678u32, in(reg) (base + 3));
        let got = u32::from_le_bytes([
            *((base + 3) as *const u8),
            *((base + 4) as *const u8),
            *((base + 5) as *const u8),
            *((base + 6) as *const u8),
        ]);
        println!("[verify] {:>12} off=3 val=0x12345678 expect=0x12345678 got=0x{:08x} {}",
            "stx.w", got, if got == 0x12345678 { "OK" } else { "FAIL" });
    }
}

// ─── 测试辅助 ───
const TEST_BASE: usize = 0x9000_0000_0010_0000;

fn k_trap_snapshot() -> usize {
    crate::arch::la::trap::K_TRAP_COUNT.load(core::sync::atomic::Ordering::SeqCst)
}

// ─── si12 类型: ld/st 指令测试 ───
fn unaligned_test_si12() {
    let start = k_trap_snapshot();
    let base = TEST_BASE as *mut u64;

    // ld.d +7 会读到第 15 字节，因此完整初始化 16 字节。
    let pattern: u64 = 0x8877665544332211;
    unsafe {
        base.write_volatile(pattern);
        base.add(1).write_volatile(0xFFEEDDCCBBAA9988);
    }

    // ld.h  — 2 字节符号扩展 (offset: 1,3,5,7)
    macro_rules! test_ld_h {
        ($off:expr) => {
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let expect = unsafe { (ptr as *const i16).read_unaligned() as i64 as usize };
            let got: usize;
            unsafe { asm!(concat!("ld.h {}, {}, ", $off), out(reg) got, in(reg) base); }
            check_eq!(got, expect, "ld.h off={}", $off);
        };
    }
    test_ld_h!(1); test_ld_h!(3); test_ld_h!(5); test_ld_h!(7);
    println!("  si12 ld.h   OK");

    // ld.hu — 2 字节零扩展
    macro_rules! test_ld_hu {
        ($off:expr) => {
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let expect = unsafe { (ptr as *const u16).read_unaligned() as usize };
            let got: usize;
            unsafe { asm!(concat!("ld.hu {}, {}, ", $off), out(reg) got, in(reg) base); }
            check_eq!(got, expect, "ld.hu off={}", $off);
        };
    }
    test_ld_hu!(1); test_ld_hu!(3); test_ld_hu!(5); test_ld_hu!(7);
    println!("  si12 ld.hu  OK");

    // ld.w  — 4 字节符号扩展
    macro_rules! test_ld_w {
        ($off:expr) => {
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let expect = unsafe { (ptr as *const i32).read_unaligned() as i64 as usize };
            let got: usize;
            unsafe { asm!(concat!("ld.w {}, {}, ", $off), out(reg) got, in(reg) base); }
            check_eq!(got, expect, "ld.w off={}", $off);
        };
    }
    test_ld_w!(1); test_ld_w!(2); test_ld_w!(3);
    println!("  si12 ld.w   OK");

    // ld.wu — 4 字节零扩展
    macro_rules! test_ld_wu {
        ($off:expr) => {
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let expect = unsafe { (ptr as *const u32).read_unaligned() as usize };
            let got: usize;
            unsafe { asm!(concat!("ld.wu {}, {}, ", $off), out(reg) got, in(reg) base); }
            check_eq!(got, expect, "ld.wu off={}", $off);
        };
    }
    test_ld_wu!(1); test_ld_wu!(2); test_ld_wu!(3);
    println!("  si12 ld.wu  OK");

    // ld.d  — 8 字节
    macro_rules! test_ld_d {
        ($off:expr) => {
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let expect = unsafe { (ptr as *const u64).read_unaligned() };
            let got: usize;
            unsafe { asm!(concat!("ld.d {}, {}, ", $off), out(reg) got, in(reg) base); }
            check_eq!(got as u64, expect, "ld.d off={}", $off);
        };
    }
    test_ld_d!(1); test_ld_d!(2); test_ld_d!(3); test_ld_d!(4);
    test_ld_d!(5); test_ld_d!(6); test_ld_d!(7);
    println!("  si12 ld.d   OK");

    // st.h — 2 字节
    let marker: u64 = 0xDEADBEEF_CAFEBABE;
    macro_rules! test_st_h {
        ($off:expr, $val:expr) => {
            unsafe { base.write_volatile(marker); }
            unsafe { asm!(concat!("st.h {}, {}, ", $off), in(reg) $val, in(reg) base); }
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let got = unsafe { (ptr as *const u16).read_unaligned() };
            check_eq!(got, $val, "st.h off={}", $off);
        };
    }
    test_st_h!(1, 0x5678u16);
    test_st_h!(3, 0x9ABC);
    test_st_h!(5, 0xDEF0);
    test_st_h!(7, 0x1234);
    println!("  si12 st.h   OK");

    // st.w — 4 字节
    macro_rules! test_st_w {
        ($off:expr, $val:expr) => {
            unsafe { base.write_volatile(marker); }
            unsafe { asm!(concat!("st.w {}, {}, ", $off), in(reg) $val, in(reg) base); }
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let got = unsafe { (ptr as *const u32).read_unaligned() };
            check_eq!(got, $val, "st.w off={}", $off);
        };
    }
    test_st_w!(1, 0x12345678u32);
    test_st_w!(2, 0xAABBCCDDu32);
    test_st_w!(3, 0xDEADBEEFu32);
    println!("  si12 st.w   OK");

    // st.d — 8 字节
    macro_rules! test_st_d {
        ($off:expr, $val:expr) => {
            unsafe { asm!(concat!("st.d {}, {}, ", $off), in(reg) $val, in(reg) base); }
            let ptr = unsafe { (TEST_BASE as *const u8).add($off) };
            let got = unsafe { (ptr as *const u64).read_unaligned() };
            check_eq!(got, $val, "st.d off={}", $off);
        };
    }
    test_st_d!(1, 0xFEDCBA9876543210u64);
    test_st_d!(2, 0x11111111_22222222u64);
    test_st_d!(3, 0x33333333_44444444u64);
    test_st_d!(4, 0x55555555_66666666u64);
    test_st_d!(5, 0x77777777_88888888u64);
    test_st_d!(6, 0x99999999_AAAAAAAAu64);
    test_st_d!(7, 0xBBBBBBBB_CCCCCCCCu64);
    println!("  si12 st.d   OK");

    let count = k_trap_snapshot() - start;
    println!("si12 tests done, traps={}", count);
}

// ─── rk 类型: ldx/stx 指令测试 ───
fn unaligned_test_rk() {
    let start = k_trap_snapshot();

    // ldx.d / stx.d 已在前面简单测试通过，这里覆盖全部 8 种
    let base_ptr = TEST_BASE as *mut u64;
    let pattern: u64 = 0x0123456789ABCDEF;
    unsafe {
        base_ptr.write_volatile(pattern);
        base_ptr.add(1).write_volatile(0xFEDCBA9876543210);
    }

    // 使用 $zero 作为 rk，base + offset 作为 rj
    // ldx.h
    for off in [1usize, 3, 5, 7].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let expect = unsafe { ((TEST_BASE + off) as *const i16).read_unaligned() as i64 as usize };
        let got: usize;
        unsafe { asm!("ldx.h {}, {}, $zero", out(reg) got, in(reg) off_ptr); }
        check_eq!(got, expect, "ldx.h off={}", off);
    }
    println!("  rk ldx.h   OK");

    // ldx.hu
    for off in [1usize, 3, 5, 7].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let expect = unsafe { ((TEST_BASE + off) as *const u16).read_unaligned() as usize };
        let got: usize;
        unsafe { asm!("ldx.hu {}, {}, $zero", out(reg) got, in(reg) off_ptr); }
        check_eq!(got, expect, "ldx.hu off={}", off);
    }
    println!("  rk ldx.hu  OK");

    // ldx.w
    for off in [1usize, 2, 3].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let expect = unsafe { ((TEST_BASE + off) as *const i32).read_unaligned() as i64 as usize };
        let got: usize;
        unsafe { asm!("ldx.w {}, {}, $zero", out(reg) got, in(reg) off_ptr); }
        check_eq!(got, expect, "ldx.w off={}", off);
    }
    println!("  rk ldx.w   OK");

    // ldx.wu
    for off in [1usize, 2, 3].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let expect = unsafe { ((TEST_BASE + off) as *const u32).read_unaligned() as usize };
        let got: usize;
        unsafe { asm!("ldx.wu {}, {}, $zero", out(reg) got, in(reg) off_ptr); }
        check_eq!(got, expect, "ldx.wu off={}", off);
    }
    println!("  rk ldx.wu  OK");

    // ldx.d
    for off in [1usize, 2, 3, 4, 5, 6, 7].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let expect = unsafe { (off_ptr as *const u64).read_unaligned() };
        let got: usize;
        unsafe { asm!("ldx.d {}, {}, $zero", out(reg) got, in(reg) off_ptr); }
        check_eq!(got as u64, expect, "ldx.d off={}", off);
    }
    println!("  rk ldx.d   OK");

    // stx.h
    let marker: u64 = 0xAAAAAAAA_BBBBBBBB;
    for off in [1usize, 3, 5, 7].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let val = 0x2E2Eu16;
        unsafe { base_ptr.write_volatile(marker); }
        unsafe { asm!("stx.h {}, {}, $zero", in(reg) val, in(reg) off_ptr); }
        let got = unsafe { (off_ptr as *const u16).read_unaligned() };
        check_eq!(got, val, "stx.h off={}", off);
    }
    println!("  rk stx.h   OK");

    // stx.w
    for off in [1usize, 2, 3].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let val = 0x1CE1CEu32;
        unsafe { base_ptr.write_volatile(marker); }
        unsafe { asm!("stx.w {}, {}, $zero", in(reg) val, in(reg) off_ptr); }
        let got = unsafe { (off_ptr as *const u32).read_unaligned() };
        check_eq!(got, val, "stx.w off={}", off);
    }
    println!("  rk stx.w   OK");

    // stx.d
    for off in [1usize, 2, 3, 4, 5, 6, 7].iter() {
        let off_ptr = unsafe { TEST_BASE + off };
        let val = 0xDEAD_BEEF_CAFE_BABEu64;
        unsafe { asm!("stx.d {}, {}, $zero", in(reg) val, in(reg) off_ptr); }
        let got = unsafe { (off_ptr as *const u64).read_unaligned() };
        check_eq!(got, val, "stx.d off={}", off);
    }
    println!("  rk stx.d   OK");

    let count = k_trap_snapshot() - start;
    println!("rk tests done, traps={}", count);
}

// ─── 交叉验证: 每种 store 独立测试 ───
fn unaligned_test_cross() {
    let start = k_trap_snapshot();
    let base = TEST_BASE as *mut u64;

    // st.h +1
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("st.h {}, {}, 1", in(reg) 0x11u16, in(reg) base); }
    let got = unsafe { ((TEST_BASE + 1) as *const u16).read_unaligned() };
    check_eq!(got, 0x11u16, "cross st.h +1");

    // st.w +2
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("st.w {}, {}, 2", in(reg) 0x2222u32, in(reg) base); }
    let got = unsafe { ((TEST_BASE + 2) as *const u32).read_unaligned() };
    check_eq!(got, 0x2222u32, "cross st.w +2");

    // st.d +3
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("st.d {}, {}, 3", in(reg) 0x33333333u64, in(reg) base); }
    let got = unsafe { ((TEST_BASE + 3) as *const u64).read_unaligned() };
    check_eq!(got, 0x33333333u64, "cross st.d +3");

    // stx.h +4
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("stx.h {}, {}, $zero", in(reg) 0x44u16, in(reg) (TEST_BASE + 4)); }
    let got = unsafe { ((TEST_BASE + 4) as *const u16).read_unaligned() };
    check_eq!(got, 0x44u16, "cross stx.h +4");

    // stx.w +5
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("stx.w {}, {}, $zero", in(reg) 0x5555u32, in(reg) (TEST_BASE + 5)); }
    let got = unsafe { ((TEST_BASE + 5) as *const u32).read_unaligned() };
    check_eq!(got, 0x5555u32, "cross stx.w +5");

    // stx.d +6
    unsafe { base.write_volatile(0xFFFFFFFF_FFFFFFFFu64); }
    unsafe { asm!("stx.d {}, {}, $zero", in(reg) 0x66666666u64, in(reg) (TEST_BASE + 6)); }
    let got = unsafe { ((TEST_BASE + 6) as *const u64).read_unaligned() };
    check_eq!(got, 0x66666666u64, "cross stx.d +6");

    println!("cross tests done, traps={}", k_trap_snapshot() - start);
}

/*
#[no_mangle]
/// the rust entry-point of os
pub fn rust_main(hart_id: usize) -> ! {
    let is_main_hart = MAIN_HART_INITED.compare_exchange(
        false, 
        true, 
        Ordering::Acquire, 
        Ordering::Relaxed
    ).is_ok();

    if is_main_hart {
        #[cfg(target_arch = "loongarch64")]
        la::mm::la_kernel_init_mem();// 设置映射窗口
        clear_bss();
        logging::init();
        info!("[kernel] Hello, world!");
        main_init(hart_id);
        panic!("Unreachable in rust_main!");
    } else {
        loop{}
        info!("[kernel] Hello from hart {}!", hart_id);
        other_init();
        panic!("Unreachable in rust_main!");
    }
    
}

/* la的main，单核版本，已弃用
#[cfg(target_arch = "loongarch64")]
#[no_mangle]
pub fn rust_main() -> ! {
    la::mm::la_kernel_init_mem();// 设置映射窗口
    clear_bss();
    logging::init();
    info!("[kernel] Hello, world!");
    mm::init();
    // mm::remap_test(); // 内核态取消了页表映射，因此跳过测试
    arch::trap::init();
    info!("drivers::search_pci"); drivers::search_pci(); info!("done drivers");
    fs::mount_procfs();
    fs::mount_devfs();
    fs::setup_oscomp_env(); 
    fs::list_apps();
    task::add_initproc();
    arch::trap::enable_timer_interrupt();
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}
 */ 

fn main_init(hart_id: usize) {
    println!("[kernel] main_init hart_id={}", hart_id);

    mm::init();
    #[cfg(target_arch = "riscv64")]
    mm::remap_test();
    #[cfg(target_arch = "loongarch64")]
    // mem_test();
    // arch::trap::init();
    #[cfg(target_arch = "loongarch64")]
    {
        // --- CSR.MISC 诊断：确认 ALCL 是否可写 ---
        {
            let before = crate::arch::la::mm::MISC_BEFORE_WRITE.load(core::sync::atomic::Ordering::Relaxed);
            let after = crate::arch::la::mm::MISC_AFTER_WRITE.load(core::sync::atomic::Ordering::Relaxed);
            let misc_before = before;
            let misc_after = after;
            // 只用 Display ({}) 打印小整数，不走 LowerHex (避免触发 ALE)
            println!("[MISC diag] ALCL before={}, after={}", misc_before, misc_after);
            if misc_after != 0 {
                println!("[MISC diag] ALCL is READ-ONLY! HW does NOT support unaligned access.");
            } else {
                println!("[MISC diag] ALCL cleared OK, unaligned access now allowed.");
            }
        }
        // --- LDX/STX vs LD/ST 非对齐访存验证 ---
        {
            use core::arch::asm;
            // 栈上 8 字节对齐 buffer，取 buf[1] 保证：物理可访存 + 非 8 字节对齐
            #[repr(align(8))]
            struct Buf([u8; 16]);
            let buf = Buf([0u8; 16]);
            let aligned_ptr = buf.0.as_ptr() as usize;
            let unaligned_ptr = aligned_ptr + 1;
            let write_val: u64 = 0xDEADBEEF_CAFEBABE;
            println!(
                "[LDX/STX] aligned=0x{:x}, unaligned=0x{:x}, write_val=0x{:x}",
                aligned_ptr, unaligned_ptr, write_val
            );

            // Step 1: STX.D + LDX.D（带 x，ALCL=0）
            let readback: u64;
            unsafe {
                asm!(
                    "or $t1, {addr}, $zero",   // t1 = unaligned_ptr
                    "or $t2, {val}, $zero",    // t2 = write_val
                    "stx.d $t2, $t1, $zero",   // Mem[t1+0] = t2  (非对齐 store)
                    "ldx.d $t3, $t1, $zero",   // t3 = Mem[t1+0]  (非对齐 load)
                    "or {rb}, $t3, $zero",     // readback = t3
                    addr = in(reg) unaligned_ptr as u64,
                    val = in(reg) write_val,
                    rb = out(reg) readback,
                    out("$t1") _,
                    out("$t2") _,
                    out("$t3") _,
                );
            }
            if readback == write_val {
                println!("[LDX/STX] STX.D+LDX.D OK, readback=0x{:x}", readback);
            } else {
                println!("[LDX/STX] STX.D+LDX.D MISMATCH! readback=0x{:x}", readback);
            }

            // Step 2: ST.D + LD.D（不带 x，非对齐预期 ALE）
            println!("[LD/ST] Now testing regular ST.D (expect ALE on unaligned addr)...");
            unsafe {
                asm!(
                    "or $t1, {addr}, $zero",
                    "or $t2, {val}, $zero",
                    "st.d $t2, $t1, 0",        // ← 非对齐，必报 ALE
                    "ld.d $t3, $t1, 0",
                    addr = in(reg) unaligned_ptr as u64,
                    val = in(reg) write_val,
                    out("$t1") _,
                    out("$t2") _,
                    out("$t3") _,
                );
            }
            println!("[LD/ST] ST.D+LD.D OK (should NOT reach here if ALE fires)");
        }
        // --- 堆分配器诊断 ---
        {
            use alloc::boxed::Box;
            use alloc::vec::Vec;
            println!("[HEAP diag] alloc test start...");
            let _b = Box::new(42u64);
            println!("[HEAP diag] Box<u64> OK");
            let mut v = Vec::<u8>::new();
            v.push(0xaa);
            println!("[HEAP diag] Vec<u8> OK, len={}", v.len());
            let _v2: Vec<u64> = (0..16).collect();
            println!("[HEAP diag] Vec<u64>[16] OK");
            // 重点测试小尺寸 layout (接近 fmt 内部可能触发的分配)
            let _b2 = Box::new(0u8);
            println!("[HEAP diag] Box<u8> OK");
            println!("[HEAP diag] all alloc tests passed!");
        }
        // --- fmt 诊断：逐步隔离 0x{:x} panic ---
        // Step 1: 纯文本（已验证 OK）
        println!("[T0] baseline plain text");
        // Step 2: {:x} 不带 # —— 应该全过
        println!("[u8  :x] {:x}", 42u8);
        println!("[u16 :x] {:x}", 42u16);
        println!("[u32 :x] {:x}", 42u32);
        // Step 3: 手动 "0x" 前缀 —— 绕过 # flag
        println!("[u8  man] 0x{:x}", 42u8);
        // Step 4: as u64 绕过 —— 走 64-bit 安全路径
        println!("[u8  u64] 0x{:x}", 42u8 as u64);
        println!("[u32 u64] 0x{:x}", 42u32 as u64);
        // Step 5: u64 原生 0x{:x} —— 已知安全
        println!("[u64 #x] 0x{:#x}", 42u64);
        // Step 6（最后）: u8/u32 原生 0x{:x} —— 预期 panic
        println!("[u8  #x] 0x{:#x}", 42u8);
        println!("[u32 #x] 0x{:#x}", 42u32);
        println!(" === All fmt tests done ===");
        info!("searching pci...");
        // drivers::search_pci();
        info!("done drivers");

    }
    //#[cfg(target_arch = "riscv64")]
    #[cfg(board = "virt")]
    {
        lazy_static::initialize(&NET_DEVICE);
        lazy_static::initialize(&crate::net::NET_IFACE);
    }
    fs::init_test_env(); 
    fs::mount_procfs();
    fs::setup_oscomp_env(); 
    fs::list_apps();
    task::add_initproc();
    arch::trap::enable_timer_interrupt();
    arch::timer::set_next_trigger(task::manager::SCHED_OTHER);
    init_other_hart(hart_id);
    println!("main_init done, run tasks...");
    task::run_tasks();
}
*/
#[cfg(target_arch = "riscv64")]
fn init_other_hart(hart_id: usize) {
    /*unsafe {
         asm!(
            "wfi",
        );
    }*/
    MAIN_HART_ID.store(hart_id, Ordering::Release);
    for i in 0..hart_id  {
        start_hart(i, _start as *const() as usize, 0);
    }
    for i in hart_id+1..CPU_CORE_NUM {
        start_hart(i, _start as *const() as usize, 0);
    }
}

#[cfg(target_arch = "loongarch64")]
fn init_other_hart(hart_id: usize) {
    let current_hart = get_hart_id();
    if current_hart != hart_id {
        warn!(
            "[kernel][la] init_other_hart: arg_hart_id={} != tp_hart_id={}",
            hart_id,
            current_hart
        );
    }
    MAIN_HART_ID.store(current_hart, Ordering::Release);
    let start_addr = _start as *const () as usize;
    for i in 0..CPU_CORE_NUM {
        if i == current_hart {
            continue;
        }
        //参考2025年RocketOS的实现，先把启动地址写入目标核的csr_mail，然后发ipi唤醒
        arch::la::ipi::csr_mail_send(start_addr as u64, i, 0);
        arch::la::ipi::send_ipi_single(i, 1);
        info!("[kernel][la] wakeup hart {} with start=0x{:x}", i, start_addr as u64);
    }
}

use mm::KERNEL_SPACE;
fn other_init() {
    // 调试用，先把其他核关了
    /*
    unsafe {
         asm!(/
            "wfi",
        );
    } */
    #[cfg(target_arch = "riscv64")]
    KERNEL_SPACE.exclusive_access().activate();
    #[cfg(target_arch = "loongarch64")]
    la::mm::la_kernel_init_mem();// 设置映射窗口
    arch::trap::init();
    arch::trap::enable_timer_interrupt();
    arch::timer::set_next_trigger(task::manager::SCHED_OTHER);
    task::run_tasks();
}

/// 获取当前核心的hart id
#[cfg(target_arch = "loongarch64")]
pub fn get_hart_id() -> usize {
    let hart_id: usize;
    unsafe {
         asm!(
            "csrrd {}, 0x20",
            out(reg) hart_id
        );
    }
    hart_id
}
#[cfg(target_arch = "riscv64")]
pub fn get_hart_id() -> usize {
    let hart_id: usize;
    unsafe {
         asm!(
            "mv {}, tp",
            out(reg) hart_id
        );
    }
    hart_id
}

#[allow(unused)]
#[cfg(target_arch = "loongarch64")]
#[no_mangle]
pub fn debug_csr_info() {
    let mut pgdl:usize= 0;
    let mut crmd:usize= 0;
    unsafe{
        asm!("csrrd {}, 0x19", out(reg) pgdl);
        asm!("csrrd {}, 0x0", out(reg) crmd);
    }
    debug!("pgdl: 0x{:x}, crmd: 0b{:b}", pgdl, crmd);
}

#[cfg(target_arch = "loongarch64")]
pub fn mem_test() {
    let aim1 = LOWRAM_BASE;
    let aim2 = LOWRAM_END;
    for addr in (aim1..aim2).step_by(8) {
        unsafe {
            let ptr = addr as *mut u64;
            ptr.write_volatile(0x12345678_9abcdeff);
            let val = ptr.read_volatile();
            check_eq!(val, 0x12345678_9abcdeff);
        }
    }
    for addr in (aim1..aim2).step_by(8) {
        unsafe {
            let ptr = addr as *mut u64;
            ptr.write_volatile(0);
            let val = ptr.read_volatile();
            check_eq!(val, 0);
        }
    }
    println!("mem_test passed!");
}

fn test_call() {
    println!("test_call: call test_func");
}
