//! POSIX timer object definitions and global registry.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicI32, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::arch::timer::get_time_us;
use crate::process::registry::{get_process, tid2task, TID2TCB};
use crate::process::signal::SignalFlags;
use crate::process::task::TaskStatus;
use crate::timer::{TimeSpec, CLOCK_REALTIME_OFFSET_NS};

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

/// `timer_settime`使用的POSIX定时器时间ABI。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ITimerSpec {
    /// 定时器到期后的重复周期；为零表示一次性定时器。
    pub it_interval: TimeSpec,
    /// 首次到期时间；为零表示解除定时器。
    pub it_value: TimeSpec,
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
    /// 下一次到期的绝对时间，单位为纳秒；`None`表示尚未启用。
    pub expires_ns: Option<u128>,
    /// 重复定时器的周期，单位为纳秒；零表示只触发一次。
    pub interval_ns: u128,
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

fn timespec_to_ns(value: TimeSpec) -> Option<u128> {
    if value.tv_nsec >= 1_000_000_000 {
        return None;
    }
    Some((value.tv_sec as u128) * 1_000_000_000 + value.tv_nsec as u128)
}

fn ns_to_timespec(value: u128) -> TimeSpec {
    TimeSpec {
        tv_sec: (value / 1_000_000_000).min(usize::MAX as u128) as usize,
        tv_nsec: (value % 1_000_000_000) as usize,
    }
}

fn realtime_ns() -> u128 {
    let base = crate::get_real_time_ns() as i128;
    let adjusted = base.saturating_add(*CLOCK_REALTIME_OFFSET_NS.lock() as i128);
    adjusted.max(0) as u128
}

fn clock_now_ns(clock_id: i32) -> u128 {
    if clock_id == 0 {
        realtime_ns()
    } else {
        (get_time_us() as u128) * 1_000
    }
}

/// 查询定时器当前剩余时间和重复周期，仅允许所属进程访问。
pub fn get_posix_timer_spec(id: i32, owner_pid: usize) -> Option<ITimerSpec> {
    let timers = POSIX_TIMERS.lock();
    let timer = timers.get(&id)?;
    if timer.owner_pid != owner_pid {
        return None;
    }
    let remaining_ns = timer
        .expires_ns
        .map(|expires| expires.saturating_sub(clock_now_ns(timer.clock_id)))
        .unwrap_or(0);
    Some(ITimerSpec {
        it_interval: ns_to_timespec(timer.interval_ns),
        it_value: ns_to_timespec(remaining_ns),
    })
}

/// 设置定时器。`absolute`为真时，`it_value`解释为所选时钟上的绝对时间。
pub fn set_posix_timer(
    id: i32,
    owner_pid: usize,
    new_value: ITimerSpec,
    absolute: bool,
) -> Option<()> {
    let interval_ns = timespec_to_ns(new_value.it_interval)?;
    let value_ns = timespec_to_ns(new_value.it_value)?;
    let mut timers = POSIX_TIMERS.lock();
    let timer = timers.get_mut(&id)?;
    if timer.owner_pid != owner_pid {
        return None;
    }
    let now_ns = clock_now_ns(timer.clock_id);
    timer.interval_ns = interval_ns;
    timer.expires_ns = if value_ns == 0 {
        None
    } else if absolute {
        Some(value_ns)
    } else {
        Some(now_ns.saturating_add(value_ns))
    };
    Some(())
}

/// 删除属于指定进程的定时器。
pub fn delete_posix_timer(id: i32, owner_pid: usize) -> bool {
    let mut timers = POSIX_TIMERS.lock();
    if timers.get(&id).map(|timer| timer.owner_pid) != Some(owner_pid) {
        return false;
    }
    timers.remove(&id);
    true
}

/// 检查所有POSIX定时器，并递送本次到期产生的信号。
pub fn check_posix_timers() {
    const SIGEV_NONE: i32 = 1;
    const SIGEV_THREAD_ID: i32 = 4;

    let expired = {
        let mut timers = POSIX_TIMERS.lock();
        let mut expired = alloc::vec::Vec::new();
        for timer in timers.values_mut() {
            let Some(deadline) = timer.expires_ns else {
                continue;
            };
            let now_ns = clock_now_ns(timer.clock_id);
            if deadline > now_ns {
                continue;
            }
            expired.push(*timer);
            if timer.interval_ns == 0 {
                timer.expires_ns = None;
            } else {
                let elapsed_periods = now_ns.saturating_sub(deadline) / timer.interval_ns + 1;
                timer.expires_ns = Some(
                    deadline.saturating_add(elapsed_periods.saturating_mul(timer.interval_ns)),
                );
            }
        }
        expired
    };

    for timer in expired {
        if timer.notify == SIGEV_NONE {
            continue;
        }
        let Some(signal) = SignalFlags::from_bits(1u64 << (timer.signo - 1)) else {
            continue;
        };
        let tasks = if timer.notify == SIGEV_THREAD_ID {
            tid2task(timer.target_tid).into_iter().collect::<alloc::vec::Vec<_>>()
        } else if let Some(process) = get_process(timer.owner_pid) {
            TID2TCB
                .exclusive_access()
                .values()
                .filter(|task| task.gettgid() == process.gettgid())
                .cloned()
                .collect::<alloc::vec::Vec<_>>()
        } else {
            alloc::vec::Vec::new()
        };

        for task in tasks {
            let mut inner = task.inner_exclusive_access();
            inner.pending.insert(signal);
            if inner.state == TaskStatus::Blocked {
                inner.signal_interrupted = true;
                inner.state = TaskStatus::Ready;
                drop(inner);
                crate::process::add_task(task);
            }
            if timer.notify == SIGEV_THREAD_ID {
                break;
            }
        }
    }
}
