//! RISC-V timer-related functionality

use crate::arch::config::{CLOCK_FREQ, UNCACHED_KERNEL_BASE};
use crate::arch::sbi::set_timer;
use crate::process::scheduler::runqueue::{SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_RR};

use riscv::register::time;

const DEFAULT_TIME_SLICE_MS: usize = 10;
const FIFO_TIME_SLICE_MS: usize = 50;
const RR_TIME_SLICE_MS: usize = 1;
const IDLE_TIME_SLICE_MS: usize = 20;
/// The number of milliseconds per second
const MSEC_PER_SEC: usize = 1000;
/// The number of microseconds per second
const MICRO_PER_SEC: usize = 1_000_000;
/// QEMU virt 平台上 Goldfish RTC 的 MMIO 基地址
#[cfg(board = "virt")]
const GOLDFISH_RTC_BASE: usize = 0x10_1000 | UNCACHED_KERNEL_BASE;

/// 获取当前的真实时间 (返回自 1970-01-01 以来的纳秒数)
#[cfg(board = "virt")]
pub fn get_real_time_ns() -> u64 {
    // 寄存器偏移：
    // 0x00: TIME_LOW  (时间的低 32 位)
    // 0x04: TIME_HIGH (时间的高 32 位)
    let timer_low = (GOLDFISH_RTC_BASE + 0x00) as *const u32;
    let timer_high = (GOLDFISH_RTC_BASE + 0x04) as *const u32;

    unsafe {
        // 核心机制：根据 Goldfish RTC 的硬件手册，
        // 读取 TIME_LOW 时，硬件会自动把对应的高 32 位锁存到 TIME_HIGH 内部寄存器中，
        // 以防止在读取两次寄存器期间时间发生进位。因此必须先读 Low，再读 High。
        let low = core::ptr::read_volatile(timer_low);
        let high = core::ptr::read_volatile(timer_high);
        
        ((high as u64) << 32) | (low as u64)
    }
}

/// 读取 JH7110 RTC，并转换为 Unix 时间戳。
#[cfg(board = "visionfive2")]
pub fn get_real_time_ns() -> u64 {
    use core::ptr::read_volatile;

    const RTC_CFG: usize = 0x00;
    const RTC_IRQ_STATUS: usize = 0x18;
    const RTC_TIME: usize = 0x3c;
    const RTC_DATE: usize = 0x40;
    const RTC_ENABLED: u32 = 1;
    const RTC_IRQ_1SEC: u32 = 1 << 3;

    #[inline]
    fn read_reg(offset: usize) -> u32 {
        unsafe { read_volatile((crate::arch::config::RTC_BASE + offset) as *const u32) }
    }

    #[inline]
    fn bcd_to_bin(value: u32) -> u32 {
        (value & 0x0f) + ((value >> 4) & 0x0f) * 10
    }

    fn is_leap_year(year: u32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    fn days_before_year(year: u32) -> u64 {
        let year = (year - 1) as u64;
        year * 365 + year / 4 - year / 100 + year / 400
    }

    fn unix_seconds(date: u32, time: u32) -> Option<u64> {
        let second = bcd_to_bin(time & 0x7f);
        let minute = bcd_to_bin((time >> 7) & 0x7f);
        let hour = bcd_to_bin((time >> 14) & 0x7f);
        let day = bcd_to_bin(date & 0x3f);
        let month = bcd_to_bin((date >> 6) & 0x1f);
        let year = 2000 + bcd_to_bin((date >> 11) & 0xff);

        if second > 59 || minute > 59 || hour > 23 || month == 0 || month > 12 || day == 0 {
            return None;
        }

        const DAYS_BEFORE_MONTH: [u16; 12] =
            [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        const DAYS_IN_MONTH: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

        let month_days = if month == 2 && is_leap_year(year) {
            29
        } else {
            DAYS_IN_MONTH[(month - 1) as usize]
        };
        if day > month_days {
            return None;
        }

        let mut days = days_before_year(year) - days_before_year(1970);
        days += DAYS_BEFORE_MONTH[(month - 1) as usize] as u64;
        if month > 2 && is_leap_year(year) {
            days += 1;
        }
        days += (day - 1) as u64;

        Some(days * 86_400 + hour as u64 * 3_600 + minute as u64 * 60 + second as u64)
    }

    if read_reg(RTC_CFG) & RTC_ENABLED != 0 {
        let mut second_state = read_reg(RTC_IRQ_STATUS) & RTC_IRQ_1SEC;
        for _ in 0..2 {
            let time = read_reg(RTC_TIME);
            let date = read_reg(RTC_DATE);
            let next_state = read_reg(RTC_IRQ_STATUS) & RTC_IRQ_1SEC;
            if second_state != 0 || next_state == 0 {
                if let Some(seconds) = unix_seconds(date, time) {
                    return seconds * 1_000_000_000;
                }
                break;
            }
            second_state = next_state;
        }
    }

    // RTC 未启用或内容无效时，退化为单调时间，避免 realtime 调用卡死。
    get_time_us() as u64 * 1_000
}

/// 获取当前的真实时间 (秒)
pub fn get_real_time_sec() -> u64 {
    get_real_time_ns() / 1_000_000_000
}
pub fn get_timer_ticks() -> usize {
    get_time()
}

/// Get the current time in ticks
pub fn get_time() -> usize {
    time::read()
}

/// get current time in milliseconds
pub fn get_time_ms() -> usize {
    get_time() * MSEC_PER_SEC / CLOCK_FREQ
}

/// get current time in microseconds
pub fn get_time_us() -> usize {
    get_time() * MICRO_PER_SEC / CLOCK_FREQ
}

fn time_slice_ms_for_policy(policy: isize) -> usize {
    match policy {
        SCHED_FIFO => FIFO_TIME_SLICE_MS,
        SCHED_RR => RR_TIME_SLICE_MS,
        SCHED_IDLE => IDLE_TIME_SLICE_MS,
        SCHED_BATCH => DEFAULT_TIME_SLICE_MS,
        _ => DEFAULT_TIME_SLICE_MS,
    }
}

/// Set the next timer interrupt according to the task scheduling policy.
pub fn set_next_trigger(policy: isize) {
    let time_slice_ms = time_slice_ms_for_policy(policy);
    set_timer(get_time() + CLOCK_FREQ * time_slice_ms / MSEC_PER_SEC);
}
