pub use crate::arch::timer::*;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

/// 当前实现接受的 `Timex.modes` 位定义。
pub const ADJ_OFFSET: u32 = 0x0001;
pub const ADJ_FREQUENCY: u32 = 0x0002;
pub const ADJ_MAXERROR: u32 = 0x0004;
pub const ADJ_ESTERROR: u32 = 0x0008;
pub const ADJ_STATUS: u32 = 0x0010;
pub const ADJ_TIMECONST: u32 = 0x0020;
pub const ADJ_TAI: u32 = 0x0080;
pub const ADJ_SETOFFSET: u32 = 0x0100;
pub const ADJ_MICRO: u32 = 0x1000;
pub const ADJ_NANO: u32 = 0x2000;
pub const ADJ_TICK: u32 = 0x4000;
pub const ADJ_OFFSET_SINGLESHOT: u32 = 0x8001;
pub const ADJ_OFFSET_SS_READ: u32 = 0xa001;

/// `Timex.status` 中对用户态开放的状态位定义。
pub const STA_PLL: i32 = 0x0001;
pub const STA_PPSFREQ: i32 = 0x0002;
pub const STA_PPSTIME: i32 = 0x0004;
pub const STA_FLL: i32 = 0x0008;
pub const STA_INS: i32 = 0x0010;
pub const STA_DEL: i32 = 0x0020;
pub const STA_UNSYNC: i32 = 0x0040;
pub const STA_FREQHOLD: i32 = 0x0080;
pub const STA_PPSSIGNAL: i32 = 0x0100;
pub const STA_PPSJITTER: i32 = 0x0200;
pub const STA_PPSWANDER: i32 = 0x0400;
pub const STA_PPSERROR: i32 = 0x0800;
pub const STA_CLOCKERR: i32 = 0x1000;
pub const STA_NANO: i32 = 0x2000;
pub const STA_MODE: i32 = 0x4000;
pub const STA_CLK: i32 = 0x8000;

/// `clock_adjtime` 成功且时钟被视为已同步时返回的状态码。
pub const TIME_OK: isize = 0;
/// `clock_adjtime` 成功但时钟仍处于未同步状态时返回的状态码。
pub const TIME_ERROR: isize = 5;

/// 当前内核允许通过 `Timex.modes` 提交的 mode 集合。
pub const CLOCK_ADJ_ALLOWED_MODES: u32 = 
    ADJ_OFFSET
    | ADJ_FREQUENCY
    | ADJ_MAXERROR
    | ADJ_ESTERROR
    | ADJ_STATUS
    | ADJ_TIMECONST
    | ADJ_TAI
    | ADJ_SETOFFSET
    | ADJ_MICRO
    | ADJ_NANO
    | ADJ_TICK;

/// 用户态通过 `ADJ_STATUS` 允许写入的状态位集合。
pub const CLOCK_ADJ_RW_STATUS: i32 = 
    STA_PLL
    | STA_PPSFREQ
    | STA_PPSTIME
    | STA_FLL
    | STA_INS
    | STA_DEL
    | STA_UNSYNC
    | STA_FREQHOLD
    | STA_MODE;

/// 当前实现能够识别的全部状态位集合。
pub const CLOCK_ADJ_VALID_STATUS: i32 = 
    CLOCK_ADJ_RW_STATUS
    | STA_PPSSIGNAL
    | STA_PPSJITTER
    | STA_PPSWANDER
    | STA_PPSERROR
    | STA_CLOCKERR
    | STA_NANO
    | STA_CLK;

lazy_static! {
    pub static ref TIMER_MANAGER: Mutex<TimerManager> = Mutex::new(TimerManager::new());
}

lazy_static! {
    /// 软件时钟校准层共享使用的 `clock_adjtime` 状态。
    pub static ref CLOCK_ADJ_STATE: Mutex<ClockAdjState> = Mutex::new(ClockAdjState::default());
    /// 叠加在平台 realtime 时钟之上的软件偏移量，单位为纳秒。
    pub static ref CLOCK_REALTIME_OFFSET_NS: Mutex<i64> = Mutex::new(0);
}

pub struct TimerManager {
    // 正向索引：到期时间(ms) -> 挂在该时间点的进程 PID 列表
    events: BTreeMap<usize, Vec<usize>>,
    // 反向索引：进程 PID -> 它当前的闹钟到期时间(ms)
    pid_map: BTreeMap<usize, usize>,
}

impl TimerManager {
    pub fn new() -> Self {
        Self {
            events: BTreeMap::new(),
            pid_map: BTreeMap::new(),
        }
    }

    /// 取消闹钟的核心逻辑，O(log N) 复杂度
    pub fn cancel_alarm(&mut self, pid: usize) -> usize {
        // 1. O(log N) 极速找到进程对应的闹钟时间
        if let Some(expire_ms) = self.pid_map.remove(&pid) {
            // 2. 去事件树里把它删掉
            if let Some(pids) = self.events.get_mut(&expire_ms) {
                // retain 保留不等于该 pid 的元素
                pids.retain(|&x| x != pid); 
                // 如果这个时间点没有其他闹钟了，把空节点也干掉，防止内存泄露
                if pids.is_empty() {
                    self.events.remove(&expire_ms);
                }
            }
            return expire_ms;
        }
        0 // 没有旧闹钟
    }

    /// 设置闹钟
    pub fn set_alarm(&mut self, pid: usize, current_ms: usize, delay_ms: usize) -> usize {
        // 先无脑注销旧闹钟
        let old_expire_ms = self.cancel_alarm(pid);

        // 如果传参不是 0，说明要设新闹钟
        if delay_ms > 0 {
            let new_expire = current_ms + delay_ms;
            self.events.entry(new_expire).or_default().push(pid);
            self.pid_map.insert(pid, new_expire);
        }

        // 返回旧闹钟剩余的秒数
        if old_expire_ms > current_ms {
            old_expire_ms - current_ms
        } else {
            0
        }
    }

    /// 时钟中断调用：只收集数据，绝不碰 PCB！
    pub fn tick(&mut self, current_ms: usize) -> Vec<usize> {
        let mut expired_pids = Vec::new();
        
        while let Some((&expire_ms, _)) = self.events.first_key_value() {
            if expire_ms <= current_ms {
                // 弹出整批到期的 PID
                let pids = self.events.pop_first().unwrap().1;
                for pid in &pids {
                    // 同步清理反向索引
                    self.pid_map.remove(pid);
                }
                expired_pids.extend(pids);
            } else {
                break;
            }
        }
        
        // 返回出去让外层慢慢发信号，彻底解耦全局锁！
        expired_pids
    }
}

/// clock/time 相关系统调用使用的绝对时间 ABI 结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimeSpec {
    /// 秒部分，通常表示自某个时间原点以来的整秒数。
    pub tv_sec: usize,
    /// 纳秒部分，必须落在 [0, 1_000_000_000) 范围内。
    pub tv_nsec: usize,
}

/// timeval 风格的用户态 ABI 结构。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TimeVal {
    /// 秒部分。
    pub sec: usize,
    /// 微秒部分。
    pub usec: usize,
}

/// `Timex` 内嵌的时间载荷。
///
/// 第二个字段为了兼容 Linux ABI，仍保留 `tv_usec` 这个历史名字。
/// 当时钟处于纳秒模式时，这个槽位里存放的是纳秒，而不是微秒。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TimexTimeVal {
    /// 调整时间或设置偏移时使用的秒部分。
    pub tv_sec: i64,
    /// 微秒模式下表示微秒，纳秒模式下表示纳秒。
    pub tv_usec: i64,
}

/// `clock_adjtime`/`adjtimex` 使用的 Linux 兼容 `timex` ABI。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timex {
    /// 位掩码，指定本次调用要把哪些字段当作输入来处理。
    pub modes: u32,
    pub _pad0: u32,
    /// 请求设置或内核回填的时钟偏移值。
    pub offset: i64,
    /// 请求设置或内核回填的频率调整值。
    pub freq: i64,
    /// 内核维护的最大误差估计。
    pub maxerror: i64,
    /// 当前误差估计值。
    pub esterror: i64,
    /// 时钟状态位，例如同步状态、分辨率模式等。
    pub status: i32,
    pub _pad1: u32,
    /// PLL/FLL 的时间常数。
    pub constant: i64,
    /// 时钟精度，按当前模式以纳秒或微秒表示。
    pub precision: i64,
    /// 时钟允许的最大频率容差。
    pub tolerance: i64,
    /// 用于 `ADJ_SETOFFSET` 等操作的辅助时间参数。
    pub time: TimexTimeVal,
    /// tick 长度参数，单位为微秒。
    pub tick: i64,
    /// PPS 相关的频率结果。
    pub ppsfreq: i64,
    /// PPS 抖动估计值。
    pub jitter: i64,
    /// PPS 间隔移位参数。
    pub shift: i32,
    pub _pad2: u32,
    /// PPS 稳定性估计值。
    pub stabil: i64,
    /// PPS 抖动事件计数。
    pub jitcnt: i64,
    /// PPS 校准区间计数。
    pub calcnt: i64,
    /// PPS 错误计数。
    pub errcnt: i64,
    /// PPS 稳定性事件计数。
    pub stbcnt: i64,
    /// 当前的 TAI-UTC 偏移。
    pub tai: i32,
    pub _pad3: [i32; 11],
}

/// `setitimer` 一类系统调用使用的区间定时器 ABI。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ITimerVal {
    /// 定时器触发后的重装周期。
    pub it_interval: TimeVal,
    /// 距离下一次触发还剩余的时间。
    pub it_value: TimeVal,
}

/// RTC 相关 ioctl 使用的日历时间表示。
#[repr(C)]
pub struct RtcTime {
    /// 当前分钟内的秒，范围 [0, 59]。
    pub tm_sec: i32,
    /// 当前小时内的分，范围 [0, 59]。
    pub tm_min: i32,
    /// 当天中的小时，范围 [0, 23]。
    pub tm_hour: i32,
    /// 一个月中的第几天，范围 [1, 31]。
    pub tm_mday: i32,
    /// 月份，从一月开始计数，范围 [0, 11]。
    pub tm_mon: i32,
    /// 自 1900 年以来经过的年数。
    pub tm_year: i32,
    /// 星期几，从周日开始计数，范围 [0, 6]。
    pub tm_wday: i32,
    /// 一年中的第几天，从 1 月 1 日开始计数，范围 [0, 365]。
    pub tm_yday: i32,
    /// 夏令时标志。
    pub tm_isdst: i32,
}

/// 当前 `clock_adjtime` 实现依赖的内核侧状态。
#[derive(Debug, Clone, Copy)]
pub struct ClockAdjState {
    /// 保存的相位偏移状态。
    pub offset: i64,
    /// 保存的频率修正状态。
    pub freq: i64,
    /// 保存的最大误差估计。
    pub maxerror: i64,
    /// 保存的估计误差。
    pub esterror: i64,
    /// 通过 `Timex.status` 对外暴露的当前状态位。
    pub status: i32,
    /// 保存的 PLL/FLL 时间常数。
    pub constant: i64,
    /// 保存的 tick 参数。
    pub tick: i64,
    /// 保存的 TAI-UTC 偏移。
    pub tai: i32,
    /// 当前接口是否处于纳秒模式。
    pub is_nano: bool,
    /// 通过 `ADJ_OFFSET_SINGLESHOT` 记录的待处理单次偏移。
    pub pending_single_shot: i64,
}

impl Default for ClockAdjState {
    fn default() -> Self {
        Self {
            offset: 0,
            freq: 0,
            maxerror: 0,
            esterror: 0,
            status: STA_UNSYNC,
            constant: 0,
            tick: 10_000,
            tai: 0,
            is_nano: false,
            pending_single_shot: 0,
        }
    }
}

/// 获取用户态可见的墙钟时间（秒），计入 CLOCK_REALTIME_OFFSET_NS 偏移
pub fn current_wallclock_sec() -> usize {
    let ns = crate::get_real_time_ns() as i128 + *CLOCK_REALTIME_OFFSET_NS.lock() as i128;
    if ns <= 0 {
        0
    } else {
        (ns / 1_000_000_000) as usize
    }
}