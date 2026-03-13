//!Stdin & Stdout
use super::File;
use crate::mm::UserBuffer;
use crate::arch::sbi::console_getchar;
use crate::task::suspend_current_and_run_next;
use lazy_static::*;
use crate::sync::MPSafeCell;

lazy_static! {
    pub static ref STDOUT_LOCK: MPSafeCell<()> = MPSafeCell::new(());
}

/// stdin file for getting chars from console
pub struct Stdin;

/// stdout file for putting chars to console
pub struct Stdout;

impl File for Stdin {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }
    fn read(&self, user_buf: UserBuffer) -> usize {
        // assert_eq!(user_buf.len(), 1);
        // busy loop
        let mut c: usize;
        loop {
            c = console_getchar();
            if c == 13 || c == '\r' as usize {
                c = 10;
            }
            
            if c == 0 || c == 0xffffffffffffffff {
                suspend_current_and_run_next();
                continue;
            } else {
                break;
            }

        }
        let ch = c as u8;
        let mut count = 0;
        for byte_ref in user_buf.into_iter() {
            unsafe {
                *byte_ref = ch;
            }
            count += 1;
            break; // Currently we only read 1 byte to match the busy loop logic
        }
        count
    }

    fn read_at(&self, _offset: usize, user_buf: UserBuffer) -> usize {
        self.read(user_buf)
    }

    fn write(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot write to stdin!");
    }

    fn write_at(&self, _offset: usize, _user_buf: UserBuffer) -> usize {
        panic!("Cannot write to stdin!");
    }

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000,
            blksize: 4096,
            ..Default::default()
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdin: getdents called on stdin, returning -1");
        -1
    }
}

impl File for Stdout {
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot read from stdout!");
    }
    fn write(&self, user_buf: UserBuffer) -> usize {
        let _lock = STDOUT_LOCK.exclusive_access();
        for buffer in user_buf.buffers.iter() {
            print!("{}", core::str::from_utf8(*buffer).unwrap());
        }
        drop(_lock);
        user_buf.len()
    }
    fn read_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.read(buf)
    }
    fn write_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.write(buf)
    }
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000,
            blksize: 4096,
            ..Default::default()
        }
    }
    fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdout: getdents called on stdout, returning -1");
        -1
    }
}