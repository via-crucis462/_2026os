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
    // 诊断用：通过 NO_CONSOLE_LOCK=1 环境变量可跳过此锁
    // #[cfg(not(no_console_lock))]
    // let _lock = CONSOLE_LOCK.exclusive_access();
    // 这行会把fmt全部输出完
    Stdout.write_fmt(args).unwrap();
}

/// Print! to the host console using the format string and arguments.
#[macro_export]
macro_rules! print {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!($fmt $(, $($arg)+)?))
    }
}

/// Println! to the host console using the format string and arguments.
#[macro_export]
macro_rules! println {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        #[allow(unreachable_code)]
        #[cfg(board = "virt")]
        $crate::console::print(format_args!(concat!($fmt, "\n") $(, $($arg)+)?));
        #[cfg(board = "2k1000")]
        $crate::console::print(format_args!(concat!($fmt, "\r\n") $(, $($arg)+)?));
    }
}
