// 为LA64部分重写，尚未完善

/// The number of ticks per second
const TICKS_PER_SEC: usize = 100;
/// The number of milliseconds per second
const MSEC_PER_SEC: usize = 1000;
/// The number of microseconds per second
const MICRO_PER_SEC: usize = 1_000_000;

use core::arch::asm;
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

/// Set the next timer interrupt
/// la64计时器中断带循环，无需每次设置，弃用该函数
#[allow(unused)]
pub fn set_next_trigger() {
    // 10ms后触发
    // set_timer(get_time() + unsafe { TIMER_FREQUENCY } / TICKS_PER_SEC);
}
