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

pub use arch::config::*;
use crate::drivers::block::NET_DEVICE;

pub use process::task;
#[allow(unused)]
use crate::arch::sbi::*;
use core::arch::global_asm;
#[cfg(target_arch = "loongarch64")]
#[allow(unused)]
use crate::arch::la;

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


/// clear BSS segment
/// 两种架构应该是统一的
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
    mm::init();
    #[cfg(target_arch = "riscv64")]
    mm::remap_test();
    arch::trap::init();
    #[cfg(target_arch = "loongarch64")]
    {
        info!("drivers::search_pci");
        drivers::search_pci();
        info!("done drivers");
    }
    #[cfg(target_arch = "riscv64")]
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
    arch::timer::set_next_trigger();
    init_other_hart(hart_id);
    task::run_tasks();
}

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
        arch::la::ipi::csr_mail_send(start_addr as u64, i, 0);
        arch::la::ipi::send_ipi_single(i, 1);
        info!("[kernel][la] wakeup hart {} with start={:#x}", i, start_addr);
    }
}

use mm::KERNEL_SPACE;
fn other_init() {
    // 调试用，先把其他核关了
    /*
    unsafe {
         asm!(
            "wfi",
        );
    } */
    #[cfg(target_arch = "riscv64")]
    KERNEL_SPACE.exclusive_access().activate();
    #[cfg(target_arch = "loongarch64")]
    la::mm::la_kernel_init_mem();// 设置映射窗口
    arch::trap::init();
    arch::trap::enable_timer_interrupt();
    arch::timer::set_next_trigger();
    task::run_tasks();
}

/// 获取当前核心的hart id
#[cfg(target_arch = "loongarch64")]
pub fn get_hart_id() -> usize {
    let hart_id: usize;
    unsafe {
         asm!(
            "move {}, $tp",
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
    debug!("pgdl: {:#x}, crmd: {:#b}", pgdl, crmd);
}