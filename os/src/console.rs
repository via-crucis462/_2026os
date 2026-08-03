//! SBI console driver, for text output
use crate::arch::sbi::console_putchar;
use crate::sync::MPSafeCell;
use core::fmt::{self, Write};

use lazy_static::*;

lazy_static! {
    pub static ref CONSOLE_LOCK: MPSafeCell<()> = MPSafeCell::new(());
}

struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            console_putchar(c as usize);
        }
        Ok(())
    }
}

pub fn print(args: fmt::Arguments) {
    // 一次格式化输出必须在多核间保持原子性，否则不同 hart 会按字符交错，
    // 不仅无法还原事件顺序，还可能把 PID、系统调用号等字段拼成错误值。
    let _lock = CONSOLE_LOCK.exclusive_access();
    Stdout.write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! print {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!($fmt $(, $($arg)+)?))
    }
}

#[macro_export]
macro_rules! println {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        #[allow(unreachable_code)]
        #[cfg(any(board = "virt", board = "visionfive2"))]
        $crate::console::print(format_args!(concat!($fmt, "\n") $(, $($arg)+)?));
        #[cfg(board = "2k1000")]
        $crate::console::print(format_args!(concat!($fmt, "\r\n") $(, $($arg)+)?));
    }
}
