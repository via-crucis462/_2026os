//! SBI console driver, for text output
use crate::arch::sbi::{console_getchar, console_putchar};
use crate::sync::{MPSafeCell, WaitQueue};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicUsize, Ordering};

use lazy_static::*;

lazy_static! {
    pub static ref CONSOLE_LOCK: MPSafeCell<()> = MPSafeCell::new(());
    /// 串口输入缓冲：内核控制台 worker 主动轮询填入，
    /// read() 从这里取字符（避免输入只在有人 read 时才被消费）。
    static ref INPUT_BUFFER: MPSafeCell<VecDeque<u8>> = MPSafeCell::new(VecDeque::new());
    /// 最近一次读取 stdin 的用户进程组（Ctrl-C 无 TIOCSPGRP 时的回退目标）
    static ref LAST_STDIN_READER_PGRP: AtomicUsize = AtomicUsize::new(0);
    /// 等待终端输入队列，阻塞在终端读和 poll 的任务在此等待
    static ref INPUT_WAITERS: Arc<MPSafeCell<WaitQueue>> =
        Arc::new(MPSafeCell::new(WaitQueue::new()));
}

pub fn console_input_wait_queue() -> Arc<MPSafeCell<WaitQueue>> {
    INPUT_WAITERS.clone()
}

// ---------- 终端行规程（tty line discipline）----------
// 标志位与 Linux asm-generic/termbits.h 保持一致
pub const ICRNL: u32 = 0o000400; // c_iflag：把回车(CR)转换为换行(NL)
pub const ISIG: u32 = 0o000001; // c_lflag：允许终端产生信号(如 Ctrl-C)
pub const ICANON: u32 = 0o000002; // c_lflag：规范模式（行缓冲）
pub const ECHO: u32 = 0o000010; // c_lflag：回显输入字符

#[derive(Clone, Copy)]
pub struct ConsoleTermios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

lazy_static! {
    /// 全局终端属性，TCGETS/TCSETS 读写它，输入读取时据此回显。
    static ref CONSOLE_TERMIOS: MPSafeCell<ConsoleTermios> = MPSafeCell::new(ConsoleTermios {
        c_iflag: 0o012402,
        c_oflag: 0o000005,
        c_cflag: 0o002277,
        c_lflag: 0o0105011, // 默认包含 ISIG|ICANON|ECHO 等
        c_line: 0,
        c_cc: {
            let mut cc = [0u8; 19];
            cc[0] = 3;    // VINTR  = Ctrl-C
            cc[1] = 28;   // VQUIT  = Ctrl-\
            cc[2] = 127;  // VERASE = DEL
            cc[4] = 4;    // VEOF   = Ctrl-D
            cc
        },
    });
}

pub fn get_termios() -> ConsoleTermios {
    *CONSOLE_TERMIOS.exclusive_access()
}

pub fn set_termios(t: ConsoleTermios) {
    *CONSOLE_TERMIOS.exclusive_access() = t;
}

/// 向 TTY 前台进程组发送中断信号（Ctrl-C / Ctrl-\）。
/// 优先使用 ioctl(TIOCSPGRP) 设置的前台进程组；未设置时退化为当前读终端进程的进程组。
pub fn tty_send_sigint() {
    use crate::process::registry::TID2TCB;
    use crate::process::{wake_up_task, SignalFlags, TaskControlBlock};
    use alloc::collections::BTreeMap;
    use alloc::sync::Arc;

    let fg = crate::syscall::process::tty_foreground_pgrp();
    let fallback_pgid = LAST_STDIN_READER_PGRP.load(Ordering::Relaxed);
    let target_pgid = if fg == 0 { fallback_pgid } else { fg as usize };
    if target_pgid == 0 {
        return;
    }

    let tasks: alloc::vec::Vec<Arc<TaskControlBlock>> =
        TID2TCB.exclusive_access().values().cloned().collect();
    let mut targets = BTreeMap::<usize, Arc<TaskControlBlock>>::new();
    for task in &tasks {
        if task.inner_exclusive_access().pgid == target_pgid {
            targets
                .entry(task.gettgid())
                .or_insert_with(|| task.clone());
        }
    }

    for (tgid, proc) in targets {
        let sig = proc.inner_exclusive_access().signal.clone();
        sig.exclusive_access().insert_pending(SignalFlags::SIGINT);
        for task in tasks.iter().filter(|task| task.gettgid() == tgid) {
            wake_up_task(task.clone());
        }
    }
}

/// 记录当前正在读终端的用户进程组，供 Ctrl-C 回退使用
pub fn note_stdin_reader() {
    if let Some(task) = crate::process::current_task() {
        let pgid = task.inner_exclusive_access().pgid;
        LAST_STDIN_READER_PGRP.store(pgid, Ordering::Relaxed);
    }
}

/// 把单个字符回显到控制台
fn echo_char(ch: u8) {
    match ch {
        b'\r' | b'\n' => print(format_args!("\r\n")),
        0x7f | 0x08 => print(format_args!("\x08 \x08")), // 退格：光标左移、清空、再左移
        c if (0x20..=0x7e).contains(&c) => print(format_args!("{}", c as char)),
        _ => {} // 其他控制字符不回显
    }
}

/// 行规程处理：识别 ISIG 控制字符（Ctrl-C 等）。
/// 返回 true 表示字符已被行规程消费，不应再返回给用户。
pub fn process_line_discipline(ch: u8) -> bool {
    let (isig, vintr) = {
        let t = CONSOLE_TERMIOS.exclusive_access();
        ((t.c_lflag & ISIG) != 0, t.c_cc[0])
    };
    if isig && ch == vintr {
        tty_send_sigint();
        return true;
    }
    false
}

/// 若行规程开启了 ECHO，则把字符回显到控制台。
/// 用于处理 poll/select 时已缓冲、随后才被 read 取走的字符。
pub fn echo_if_enabled(ch: u8) {
    if (CONSOLE_TERMIOS.exclusive_access().c_lflag & ECHO) != 0 {
        echo_char(ch);
    }
}

/// 从控制台非阻塞地读一个字符（无输入时返回 None）。
/// 已做 ICRNL 回车转换，并按 ECHO 标志回显（只回显一次）。
pub fn console_read_char() -> Option<u8> {
    // 优先消费控制台 worker 轮询到的字符
    if let Some(ch) = INPUT_BUFFER.exclusive_access().pop_front() {
        echo_if_enabled(ch);
        return Some(ch);
    }
    let raw = console_getchar();
    if raw == 0 || raw == usize::MAX {
        return None;
    }
    let mut ch = raw as u8;
    let (icrnl, echo) = {
        let t = CONSOLE_TERMIOS.exclusive_access();
        ((t.c_iflag & ICRNL) != 0, (t.c_lflag & ECHO) != 0)
    };
    if icrnl && ch == b'\r' {
        ch = b'\n';
    }
    if process_line_discipline(ch) {
        return None;
    }
    if echo {
        echo_char(ch);
    }
    Some(ch)
}

/// 从控制台非阻塞地读一个字符，但不回显（用于 poll/select 探测）。
pub fn console_peek_char() -> Option<u8> {
    // 与 read 路径共用缓冲：取出后由 stdio 的 STDIN_BUFFERED_CHAR 暂存
    if let Some(ch) = INPUT_BUFFER.exclusive_access().pop_front() {
        return Some(ch);
    }
    let raw = console_getchar();
    if raw == 0 || raw == usize::MAX {
        return None;
    }
    let mut ch = raw as u8;
    if (CONSOLE_TERMIOS.exclusive_access().c_iflag & ICRNL) != 0 && ch == b'\r' {
        ch = b'\n';
    }
    Some(ch)
}

/// 控制台内核 worker：周期性轮询串口，处理行规程控制字符（Ctrl-C 等）
/// 并把普通字符放入输入缓冲，供后续 read() 使用。
pub fn console_poll_input() {
    let mut received = false;
    loop {
        let raw = console_getchar();
        if raw == 0 || raw == usize::MAX {
            break;
        }
        let mut ch = raw as u8;
        let icrnl = (CONSOLE_TERMIOS.exclusive_access().c_iflag & ICRNL) != 0;
        if icrnl && ch == b'\r' {
            ch = b'\n';
        }
        if process_line_discipline(ch) {
            continue; // ^C 等已被消费并发信号
        }
        INPUT_BUFFER.exclusive_access().push_back(ch);
        received = true;
    }
    if received {
        crate::process::scheduler::wait::wake_up_all_mp(&INPUT_WAITERS);
    }
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
