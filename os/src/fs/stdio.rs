//!Stdin & Stdout
use super::File;
use crate::mm::UserBuffer;
use crate::arch::sbi::console_getchar;
use crate::task::suspend_current_and_run_next;
use lazy_static::*;
use crate::sync::MPSafeCell;
use crate::auth::{PermStat, FileMode};
use core::any::Any;

lazy_static! {
    pub static ref STDOUT_LOCK: MPSafeCell<()> = MPSafeCell::new(());
    static ref STDIN_BUFFERED_CHAR: MPSafeCell<Option<u8>> = MPSafeCell::new(None);
}

fn normalize_console_char(c: usize) -> Option<u8> {
    if c == 0 || c == usize::MAX {
        return None;
    }
    let normalized = if c == 13 || c == '\r' as usize { 10 } else { c };
    Some(normalized as u8)
}

/// stdin file for getting chars from console
pub struct Stdin;

/// stdout file for putting chars to console
pub struct Stdout;
/// stderr file for putting chars to console
pub struct Stderr;
impl File for Stdin {
    fn readable(&self) -> bool {
        true
    }

    fn ready_to_read(&self) -> bool {
        if STDIN_BUFFERED_CHAR.exclusive_access().is_some() {
            return true;
        }
        if let Some(ch) = normalize_console_char(console_getchar()) {
            *STDIN_BUFFERED_CHAR.exclusive_access() = Some(ch);
            return true;
        }
        false
    }

    fn writable(&self) -> bool {
        false
    }
    fn read(&self, user_buf: UserBuffer) -> usize {
        // assert_eq!(user_buf.len(), 1);
        // busy loop
        let ch = loop {
            let task = crate::task::current_task().unwrap();
            let task_inner = task.inner_exclusive_access();
            let pending = task_inner.signals.bits() & !task_inner.signal_mask.bits();
            let unmaskable = task_inner.signals.bits() & ((1 << 8) | (1 << 18));
            drop(task_inner);

            if pending != 0 || unmaskable != 0 {
                return 0; 
            }
            
            // 2. 只读取一次字符，避免吞掉输入
            let mut c = console_getchar();
            
            // 转换回车键
            if c == 13 || c == '\r' as usize {
                c = 10;
            }
            if let Some(ch) = normalize_console_char(c) {
                break ch;
            }
            
            suspend_current_and_run_next();
        }; // 注意这里的最后要加分号

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

    fn get_perm(&self) -> crate::auth::PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode);
        PermStat { mode, uid, gid }
    }

    
    fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdin: getdents called on stdin, returning -1");
        -1
    }

    fn as_any(&self) -> &dyn Any { self }
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
        //let _lock = STDOUT_LOCK.exclusive_access();
        for buffer in user_buf.buffers.iter() {
            print!("{}", core::str::from_utf8(*buffer).unwrap());
        }
        //drop(_lock);
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
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stdout: getdents called on stdout, returning -1");
        -1
    }
    fn as_any(&self) -> &dyn Any { self }
}

impl File for Stderr {
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot read from stderr!");
    }
    fn write(&self, user_buf: UserBuffer) -> usize {
        for buffer in user_buf.buffers.iter() {
            println!("{}", core::str::from_utf8(*buffer).unwrap());
        }
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
    fn get_perm(&self) -> PermStat {
        let stat = self.get_stat();
        let (mode, uid, gid) = (stat.mode, stat.uid, stat.gid);
        let mode = FileMode::from_bits_truncate(mode);
        PermStat { mode, uid, gid }
    }
        fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Stderr: getdents called on stderr, returning -1");
        -1
    }
    fn as_any(&self) -> &dyn Any { self }
}