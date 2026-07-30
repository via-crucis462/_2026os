//! POSIX timer object definitions and global registry.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicI32, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

/// Linux内核使用的sigevent布局；只保留timer_create需要读取的字段。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KernelSigEvent {
    /// `sigev_value`：定时器到期时随信号传递给用户态的应用自定义值。
    pub value: usize,
    /// `sigev_signo`：`SIGEV_SIGNAL`或`SIGEV_THREAD_ID`使用的信号编号。
    pub signo: i32,
    /// `sigev_notify`：通知方式，如发送信号、不通知或通知指定线程。
    pub notify: i32,
    /// `sigev_notify_thread_id`：`SIGEV_THREAD_ID`模式下接收信号的线程ID。
    pub tid: i32,
}

/// POSIX定时器的创建信息。定时值由后续timer_settime设置。
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct PosixTimer {
    /// 创建该定时器的进程ID，用于权限检查和进程退出时清理资源。
    pub owner_pid: usize,
    /// 计时所依据的时钟ID，目前支持`CLOCK_REALTIME`和`CLOCK_MONOTONIC`。
    pub clock_id: i32,
    /// 到期通知方式，对应用户传入的`sigev_notify`。
    pub notify: i32,
    /// 到期时发送的信号编号；`SIGEV_NONE`模式下不使用。
    pub signo: i32,
    /// 到期信号携带的用户自定义值，对应`sigev_value`。
    pub value: usize,
    /// 定向通知的线程ID；仅`SIGEV_THREAD_ID`模式有效，其他模式为0。
    pub target_tid: usize,
}

static NEXT_POSIX_TIMER_ID: AtomicI32 = AtomicI32::new(0);

lazy_static! {
    static ref POSIX_TIMERS: Mutex<BTreeMap<i32, PosixTimer>> = Mutex::new(BTreeMap::new());
}

/// 分配一个非负timer_t并将定时器加入全局注册表。
pub fn add_posix_timer(timer: PosixTimer) -> i32 {
    let id = loop {
        let candidate = NEXT_POSIX_TIMER_ID.fetch_add(1, Ordering::Relaxed);
        if candidate >= 0 && !POSIX_TIMERS.lock().contains_key(&candidate) {
            break candidate;
        }
    };
    POSIX_TIMERS.lock().insert(id, timer);
    id
}

/// 从全局注册表删除指定POSIX定时器。
pub fn remove_posix_timer(id: i32) {
    POSIX_TIMERS.lock().remove(&id);
}
