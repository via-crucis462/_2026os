//! LA64通常没有SBI，需要用uart模拟一些简单的SBI调用

#![allow(unused)]
use lazy_static::lazy_static;

use crate::sync::MPSafeCell;
use super::config::*;
use core::{arch::asm, panic};
// 使用uart模拟sbi
struct UartSbi {
    base_addr: usize,
}

pub const UART_BASE: usize = UNCHACHED_KERNEL_BASE | super::config::UART_PHYS;

/// UART data-register address used by the stackless TLB-refill diagnostic.
#[no_mangle]
pub static tlb_refill_debug_uart: usize = UART_BASE;

// ACPI GED寄存器基址，用于电源管理
pub const ACPI_GED_BASE: usize = UNCHACHED_KERNEL_BASE | 0x100e001c;

lazy_static! {
    static ref UART_SBI: MPSafeCell<UartSbi> = 
    unsafe { 
        MPSafeCell::new(UartSbi { base_addr: UART_BASE })
    };
}

impl UartSbi {
    // 使用rcorela手册的实现
    // 摘自手册：在打印字符函数中，需要偏移地址为0x5的线路状态寄存器，
    // 检查FIFO空标志，非空时等待，否则就写入。在读取函数中则相反。
    fn put(&mut self, c: u8) {
        let ptr = self.base_addr as *mut u8;
        loop {
            unsafe {
                let lsr = ptr.add(5).read_volatile();
                if lsr & (1 << 5) != 0 {
                    break;
                }
            }
        }
        unsafe {
            (self.base_addr as *mut u8).write_volatile(c);
            asm!("dbar 0");  // ← 诊断：确保写入对后续读取可见
        }
    }
    // 使用rcore的实现
    fn get(&mut self) -> Option<u8> {
        let ptr = self.base_addr as *mut u8;
        unsafe {
            if ptr.add(5).read_volatile() & 1 == 0 {
                // The DR bit is 0, meaning no data
                None
            } else {
                // The DR bit is 1, meaning data!
                Some(ptr.add(0).read_volatile())
            }
        }
    }

    /// 初始化 16550 兼容 UART
    /// 波特率 = UART_CLK / (16 * divisor)
    /// 2K1000 APB 时钟 50MHz，divisor=27 → 约 115200 bps
    pub fn init(&mut self, baud_divisor: u16) {
        let ptr = self.base_addr as *mut u8;
        unsafe {
            // 1. 禁中断
            ptr.add(1).write_volatile(0x00);   // IER = 0

            // 2. 使能 DLAB 以访问波特率分频器
            ptr.add(3).write_volatile(0x80);   // LCR = 0x80 (DLAB=1)

            // 3. 设置波特率分频器 (先低后高)
            ptr.add(0).write_volatile((baud_divisor & 0xFF) as u8);       // DLL
            ptr.add(1).write_volatile(((baud_divisor >> 8) & 0xFF) as u8); // DLH

            // 4. 设置线路控制: 8N1 (8数据位, 无校验, 1停止位)
            ptr.add(3).write_volatile(0x03);   // LCR = 0x03 (8N1, DLAB=0)

            // 5. 使能并清空 FIFO
            ptr.add(2).write_volatile(0x07);   // FCR = 0x07 (使能FIFO, 清空收发FIFO)

            // 6. 可选：设置 Modem 控制 (RTS=1, DTR=1)
            ptr.add(4).write_volatile(0x03);   // MCR = 0x03
        }
    }

    // 关机
}


// 无锁版 UART 直接写入（仅用于诊断，绕过 UART_SBI 的 Mutex）
#[cfg(no_console_lock)]
fn uart_put_raw(c: u8) {
    let ptr = UART_BASE as *mut u8;
    loop {
        unsafe {
            if ptr.add(5).read_volatile() & (1 << 5) != 0 {
                break;
            }
        }
    }
    unsafe {
        (UART_BASE as *mut u8).write_volatile(c);
        asm!("dbar 0");  // ← 诊断：确保写入对后续读取可见
    }
}

#[allow(dead_code)]
pub fn console_putchar(c: usize) {
    #[cfg(not(no_console_lock))]
    {
        UART_SBI.exclusive_access().put(c as u8);
    }
    #[cfg(no_console_lock)]
    {
        uart_put_raw(c as u8);
    }
}

#[allow(dead_code)]
pub fn console_getchar() -> usize {
    UART_SBI.exclusive_access().get().unwrap_or(0) as *const () as usize
}

/// 内核启动时调用，初始化 UART 为 115200 8N1
pub fn uart_init() {
    UART_SBI.exclusive_access().init(27);
}

#[allow(dead_code)]
#[allow(unreachable_code)]
pub fn shutdown() {
    unsafe {
        // qemu la64 virt machine ACPI GED powerdown
        (ACPI_GED_BASE as *mut u8).write_volatile(0x34);
        
        asm!("idle 0");
    }
}
