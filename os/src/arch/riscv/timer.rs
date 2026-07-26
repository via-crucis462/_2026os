//! RISC-V timer-related functionality

use crate::arch::config::CLOCK_FREQ;
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
const GOLDFISH_RTC_BASE: usize = 0x10_1000;

/// 获取当前的真实时间 (返回自 1970-01-01 以来的纳秒数)
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

/// 获取当前的真实时间 (秒)
pub fn get_real_time_sec() -> u64 {
    get_real_time_ns() / 1_000_000_000
}
pub fn get_timer_ticks() -> usize {
    time::read()
}

/// Get the current time in ticks
pub fn get_time() -> usize {
    time::read()
}

/// get current time in milliseconds
pub fn get_time_ms() -> usize {
    time::read() * MSEC_PER_SEC / CLOCK_FREQ
}

/// get current time in microseconds
pub fn get_time_us() -> usize {
    time::read() * MICRO_PER_SEC / CLOCK_FREQ
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
