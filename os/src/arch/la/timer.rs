// 为LA64部分重写，尚未完善

use crate::arch::config::UNCHACHED_KERNEL_BASE;
use core::arch::asm;

/// The number of ticks per second
const TICKS_PER_SEC: usize = 100;
/// The number of milliseconds per second
const MSEC_PER_SEC: usize = 1000;
/// The number of microseconds per second
const MICRO_PER_SEC: usize = 1_000_000;
const NSEC_PER_SEC: u64 = 1_000_000_000;

/// QEMU loongarch virt 平台上的 LS7A RTC 物理基地址。
const LS7A_RTC_REG_BASE_PHYS: usize = 0x100D_0100;
/// LoongArch 内核通过 uncached 直映窗口访问 MMIO。
const LS7A_RTC_REG_BASE: usize = UNCHACHED_KERNEL_BASE | LS7A_RTC_REG_BASE_PHYS;

const SYS_TOYREAD0: usize = 0x2C;
const SYS_TOYREAD1: usize = 0x30;
const SYS_RTCCTRL: usize = 0x40;

const RTC_CTRL_EO: u32 = 1 << 8;
const RTC_CTRL_TOYEN: u32 = 1 << 11;
// 全局只初始化一次，所以unsafe是安全的
static mut TIMER_FREQUENCY: usize = 0;

/// Get the current time in ticks
pub fn get_time() -> usize {
    let mut time: usize;
    unsafe {
        asm!("rdtime.d {}, $zero", out(reg) time);
    }
    time
}

pub fn get_timer_ticks() -> usize {
    get_time()
}

/// 读取板载时钟频率，单位Hz
pub fn init_board_freq() {
    let freq;
    unsafe {
        asm!("cpucfg {}, {}", out(reg) freq, in(reg) 0x4);
        TIMER_FREQUENCY = freq;
    }
}

/// get current time in milliseconds
pub fn get_time_ms() -> usize {
    let time = get_time();
    time * MSEC_PER_SEC / unsafe { TIMER_FREQUENCY }
}

/// get current time in microseconds
pub fn get_time_us() -> usize {
    let time = get_time();
    time * MICRO_PER_SEC / unsafe { TIMER_FREQUENCY }
}

fn rtc_read_u32(offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((LS7A_RTC_REG_BASE + offset) as *const u32) }
}

fn rtc_write_u32(offset: usize, value: u32) {
    unsafe { core::ptr::write_volatile((LS7A_RTC_REG_BASE + offset) as *mut u32, value) }
}

fn ensure_toy_enabled() {
    let required = RTC_CTRL_EO | RTC_CTRL_TOYEN;
    let ctrl = rtc_read_u32(SYS_RTCCTRL);
    if ctrl & required != required {
        rtc_write_u32(SYS_RTCCTRL, ctrl | required);
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = month as i64;
    let day = day as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn get_real_time_ns() -> u64 {
    ensure_toy_enabled();

    let toy0 = rtc_read_u32(SYS_TOYREAD0);
    let toy1 = rtc_read_u32(SYS_TOYREAD1);

    let sec = ((toy0 >> 4) & 0x3F) as u64;
    let min = ((toy0 >> 10) & 0x3F) as u64;
    let hour = ((toy0 >> 16) & 0x1F) as u64;
    let day = ((toy0 >> 21) & 0x1F) as u32;
    let month = ((toy0 >> 26) & 0x3F) as u32;
    let year = toy1 as i64 + 1900;

    let days = days_from_civil(year, month, day);
    let seconds = days as u64 * 86_400 + hour * 3_600 + min * 60 + sec;
    seconds * NSEC_PER_SEC
}
/// Set the next timer interrupt
/// la64计时器中断带循环，无需每次设置，弃用该函数
#[allow(unused)]
pub fn set_next_trigger() {
    // 10ms后触发
    // set_timer(get_time() + unsafe { TIMER_FREQUENCY } / TICKS_PER_SEC);
}
