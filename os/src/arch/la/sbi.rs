//! LA64通常没有SBI，需要用uart模拟一些简单的SBI调用

#![allow(unused)]
use lazy_static::lazy_static;

use crate::sync::UPSafeCell;
use super::config::*;
use core::arch::asm;
// 使用uart模拟sbi
struct UartSbi {
    base_addr: usize,
}

// qemu la64 uart基址
const UART_BASE: usize = UNCHACHED_KERNEL_BASE | 0x1fe001e0;

lazy_static! {
    static ref UART_SBI: UPSafeCell<UartSbi> = 
    unsafe { 
        UPSafeCell::new(UartSbi { base_addr: UART_BASE })
    };
}

impl UartSbi {
    // 使用rcorela手册的实现
    // 摘自手册：在打印字符函数中，需要偏移地址为0x5的线路状态寄存器，
    // 检查FIFO空标志，非空时等待，否则就写入。在读取函数中则相反。
    fn put(&mut self, c: u8) {
        let mut ptr = self.base_addr as *mut u8;
        loop {
            unsafe {
                let c = ptr.add(5).read_volatile();
                if c & (1<<5)!=0{
                    break;
                }
            }
        }
        ptr = self.base_addr as *mut u8;
        unsafe {
            ptr.add(0).write_volatile(c);
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
    // 关机
    fn shutdown(&mut self) {

    }
}



#[allow(dead_code)]
pub fn console_putchar(c: usize) {
    UART_SBI.exclusive_access().put(c as u8);
}

#[allow(dead_code)]
pub fn console_getchar() -> usize {
    UART_SBI.exclusive_access().get().unwrap_or(0) as *const () as usize
}

#[allow(dead_code)]
//、 ai写的设置定时器函数，暂未检查
pub fn set_timer(timer: usize) {
    // LoongArch 使用 CSR TCFG (0x41) 配置定时器
    // TCFG 格式: [InitVal (bits 63:2)] | [Periodic (bit 1)] | [En (bit 0)]
    // 定时器是倒计数的。我们需要计算 delta 并设置为单次触发模式。
    
    let current_time: usize;
    unsafe { asm!("rdtime.d {}, $zero", out(reg) current_time); }
    
    let delta = if timer > current_time { timer - current_time } else { 2000 };
    
    // 设置初始值并使能 (Enable=1, Periodic=0 for One-shot)
    let tcfg = (delta << 2) | 1;
    
    unsafe {
        asm!("csrwr {}, 0x41", in(reg) tcfg);
    }
}

#[allow(dead_code)]
#[allow(unreachable_code)]
pub fn shutdown() -> ! {
    // 暂未实现，死循环代替
    loop {}
}
