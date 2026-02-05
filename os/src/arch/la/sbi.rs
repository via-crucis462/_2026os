//! LA64通常没有SBI，需要用uart模拟一些简单的SBI调用
use lazy_static::lazy_static;

use crate::sync::UPSafeCell;

// 使用uart模拟sbi
struct UartSbi {
    base_addr: usize,
}

// qemu la64 uart基址
const UART_BASE: usize = 0x1fe001e0;

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
    UART_SBI.exclusive_access().get().unwrap_or(0) as usize
}

#[allow(dead_code)]
pub fn set_timer(_timer: usize) {
    // TODO
}

#[allow(dead_code)]
pub fn shutdown() -> ! {
    panic!("Forced shutdown!");
    // 不应运行到这里
}
