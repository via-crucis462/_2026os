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
use core::arch::global_asm;
#[cfg(target_arch = "loongarch64")]
#[allow(unused)]
use crate::arch::la;

pub use arch::timer::*;

#[cfg(board = "virt")]
use crate::arch::drivers::NET_DEVICE;

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

#[no_mangle]
pub fn rust_main(hart_id: usize) -> ! {
    clear_bss();
    logging::init();
    println!("[kernel] Hello, world! hart_id={}", hart_id);

    println!("[kernel] test_broke num={:#x}", 42u8);
    println!("[kernel] test_broke num={:#x}", 42u16);
    println!("[kernel] test_broke num={:#x}", 42u32);
    println!("[kernel] test_broke num={:#x}", 42u64);

    /* 
    let aligned_adr = 0x9000_0000_0000_0000usize;
    let unaligned_adr = aligned_adr + 1;
    let aligned_ptr = aligned_adr as *mut u64;
    let unaligned_ptr = unaligned_adr as *mut u64;

    
    // 读取crmd
    unsafe {
        let mut crmd: usize;
        asm!("csrrd {}, 0x0", out(reg) crmd);
        println!("[kernel] crmd=0x{:x}", crmd);
    }

    // 读取misc
    unsafe {
        let mut misc: usize;
        asm!("csrrd {}, 0x3", out(reg) misc);
        println!("[kernel] misc=0x{:x}", misc);
    }

    let test_num = 0x34u8;
    // test ldx stx
    unsafe {
        asm!("stx.d $t0, {aligned}, $zero", aligned = in(reg) aligned_ptr, out("$t1") _);
        asm!("ldx.d $t0, {aligned}, $zero", aligned = in(reg) aligned_ptr, out("$t0") _);
     
    }
    println!("pass aligned ldx stx test");
    unsafe {
        asm!("ldx.d $t0, {unaligned}, $zero", unaligned = in(reg) unaligned_ptr, out("$t0") _);
        asm!("stx.d $t1, {unaligned}, $zero", unaligned = in(reg) unaligned_ptr, out("$t1") _);     
    }
    println!("pass unaligned ldx stx test");
    */


    
    panic!("main end");
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
use core::arch::{asm};
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
            assert_eq!(val, 0x12345678_9abcdeff);
        }
    }
    for addr in (aim1..aim2).step_by(8) {
        unsafe {
            let ptr = addr as *mut u64;
            ptr.write_volatile(0);
            let val = ptr.read_volatile();
            assert_eq!(val, 0);
        }
    }
    println!("mem_test passed!");
}

fn test_call() {
    println!("test_call: call test_func");
}