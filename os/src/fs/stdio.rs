//!Stdin & Stdout
use super::File;
use crate::mm::UserBuffer;
use crate::console::{console_peek_char, console_read_char, echo_if_enabled};
use crate::task::suspend_current_and_run_next;
use lazy_static::*;
use crate::sync::{MPSafeCell, WaitQueue};
use alloc::sync::Arc;
use crate::auth::{PermStat, FileMode};
use core::any::Any;

lazy_static! {
    pub static ref STDOUT_LOCK: MPSafeCell<()> = MPSafeCell::new(());
    static ref STDIN_BUFFERED_CHAR: MPSafeCell<Option<u8>> = MPSafeCell::new(None);
}

/// stdin file for getting chars from console
pub struct Stdin;

/// stdout file for putting chars to console
pub struct Stdout;
/// stderr file for putting chars to console
pub struct Stderr;
impl File for Stdin {
    fn info_type(&self) {
        println!("stdin");
    }
    fn readable(&self) -> bool {
        true
    }

    fn ready_to_read(&self) -> bool {
        if STDIN_BUFFERED_CHAR.exclusive_access().is_some() {
            return true;
        }
        // 探测时不回显，回显统一放在 read() 里，保证每个字符只回显一次
        if let Some(ch) = console_peek_char() {
            *STDIN_BUFFERED_CHAR.exclusive_access() = Some(ch);
            return true;
        }
        false
    }

    fn poll_wait_queue(&self) -> Option<Arc<MPSafeCell<WaitQueue>>> {
        Some(crate::console::console_input_wait_queue())
    }

    fn writable(&self) -> bool {
        false
    }
    fn read(&self, user_buf: UserBuffer) -> usize {
        let ch = loop {
            crate::console::note_stdin_reader();
            // Ctrl-C is process-directed and therefore lives in the shared
            // pending set. Check both shared and thread-local pending signals.
            if crate::process::check_pending_signal() {
                return 0;
            }

            // 优先使用 ready_to_read() 时已缓冲的字符（此时需要补回显）
            if let Some(ch) = STDIN_BUFFERED_CHAR.exclusive_access().take() {
                if crate::console::process_line_discipline(ch) {
                    continue; // Ctrl-C 等控制字符已被消费并发信号，继续等待输入
                }
                echo_if_enabled(ch);
                break ch;
            }

            // 读取并回显单个字符（ICRNL 转换 + ECHO 回显在 console 层完成）
            if let Some(ch) = console_read_char() {
                break ch;
            }
            suspend_current_and_run_next();
        };

        let mut count = 0;
        for byte_ref in user_buf.into_iter() {
            unsafe {
                *byte_ref = ch as u8;
            }
            count += 1;
            break; // 目前只读取 1 byte 以匹配忙等待逻辑
        }
        count
    
    }

    fn raw_read_at(&self, _offset: usize, user_buf: UserBuffer) -> usize {
        self.read(user_buf)
    }

    fn write(&self, _user_buf: UserBuffer) -> usize {
        warn!("write to stdin is not allowed");
        0
    }

    fn raw_write_at(&self, _offset: usize, _user_buf: UserBuffer) -> usize {
        warn!("write_at to stdin is not allowed");
        0
    }

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000,
            blksize: 4096,
            ..Default::default()
        }
    }

    fn get_perm(&self) -> crate::auth::PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }

    
    fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdin: getdents called on stdin, returning -1");
        -1
    }

    fn as_any(&self) -> &dyn Any { self }
}

impl File for Stdout {
    fn info_type(&self) {
        println!("stdout");
    }
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> usize {
        warn!("read from stdout is not allowed");
        0
    }
    fn write(&self, user_buf: UserBuffer) -> usize {
        // 按字节直接转发，不解释为 UTF-8
        for buffer in user_buf.buffers.iter() {
            for &b in buffer.iter() {
                crate::arch::sbi::console_putchar(b as usize);
            }
        }
        user_buf.len()
    }
    fn raw_read_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.read(buf)
    }
    fn raw_write_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.write(buf)
    }
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000,
            blksize: 4096,
            ..Default::default()
        }
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdout: getdents called on stdout, returning -1");
        -1
    }
    fn as_any(&self) -> &dyn Any { self }
}

impl File for Stderr {
    fn info_type(&self) {
        println!("stderr");
    }
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> usize {
        warn!("read from stderr is not allowed");
        0
    }
    fn write(&self, user_buf: UserBuffer) -> usize {
        for buffer in user_buf.buffers.iter() {
            print!("{}", core::str::from_utf8(&*buffer).unwrap());
        }
        user_buf.len()
    }
    fn raw_read_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.read(buf)
    }
    fn raw_write_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.write(buf)
    }
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000,
            blksize: 4096,
            ..Default::default()
        }
    }
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode as u16);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stderr: getdents called on stderr, returning -1");
        -1
    }
    fn as_any(&self) -> &dyn Any { self }
}
