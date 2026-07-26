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
pub use arch::config;

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
    mem_test();
    arch::trap::init();
    #[cfg(target_arch = "loongarch64")]
    {
        use crate::ext4fs::block_device_test;

        println!("searching pci...");
        // 枚举pci设备
        drivers::search_pci();
        println!("done drivers");
        // 打印ahci控制器信息
        drivers::board::la2k1000::print_ahci_info();
        #[cfg(false)]
        unsafe {
            // 测试block device，会破坏磁盘数据，仅供测试
            block_device_test();
        }
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
    // init_other_hart(hart_id);

    // 块设备读取测试：从 0 号块逐块读取到 1024 号块
    {
        use crate::drivers::block::block_dev::BlockDevice;
        use crate::arch::drivers::BLOCK_DEVICE;
        let mut buf = [0u8; crate::ext4fs::BLOCK_SZ];
        let total = 1025;
        for block_id in 0..total {
            BLOCK_DEVICE.read_block(block_id, &mut buf);
            if block_id % 128 == 0 {
                println!("block read test: {}/{} blocks done", block_id, total);
            }
        }
        println!("block read test: all {} blocks read successfully", total);
    }

    println!("main_init done, run tasks...");
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
