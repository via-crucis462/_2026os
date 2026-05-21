use crate::lazy_static;
use spin::Mutex;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TimexTimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timex {
    pub modes: u32,
    pub _pad0: u32,
    pub offset: i64,
    pub freq: i64,
    pub maxerror: i64,
    pub esterror: i64,
    pub status: i32,
    pub _pad1: u32,
    pub constant: i64,
    pub precision: i64,
    pub tolerance: i64,
    pub time: TimexTimeVal,
    pub tick: i64,
    pub ppsfreq: i64,
    pub jitter: i64,
    pub shift: i32,
    pub _pad2: u32,
    pub stabil: i64,
    pub jitcnt: i64,
    pub calcnt: i64,
    pub errcnt: i64,
    pub stbcnt: i64,
    pub tai: i32,
    pub _pad3: [i32; 11],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ITimerVal {
    pub it_interval: TimeVal,
    pub it_value: TimeVal,
}

#[repr(C)]
pub struct RtcTime {
    pub tm_sec: i32,
    pub tm_min: i32,
    pub tm_hour: i32,
    pub tm_mday: i32,
    pub tm_mon: i32,
    pub tm_year: i32,
    pub tm_wday: i32,
    pub tm_yday: i32,
    pub tm_isdst: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ClockAdjState {
    pub offset: i64,
    pub freq: i64,
    pub maxerror: i64,
    pub esterror: i64,
    pub status: i32,
    pub constant: i64,
    pub tick: i64,
    pub tai: i32,
    pub is_nano: bool,
    pub pending_single_shot: i64,
}

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

pub const TIME_OK: isize = 0;
pub const TIME_ERROR: isize = 5;
pub const CLOCK_ADJ_ALLOWED_MODES: u32 = ADJ_OFFSET
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
pub const CLOCK_ADJ_RW_STATUS: i32 = STA_PLL
    | STA_PPSFREQ
    | STA_PPSTIME
    | STA_FLL
    | STA_INS
    | STA_DEL
    | STA_UNSYNC
    | STA_FREQHOLD
    | STA_MODE;
pub const CLOCK_ADJ_VALID_STATUS: i32 = CLOCK_ADJ_RW_STATUS
    | STA_PPSSIGNAL
    | STA_PPSJITTER
    | STA_PPSWANDER
    | STA_PPSERROR
    | STA_CLOCKERR
    | STA_NANO
    | STA_CLK;

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

lazy_static! {
    pub static ref CLOCK_ADJ_STATE: Mutex<ClockAdjState> = Mutex::new(ClockAdjState::default());
    pub static ref CLOCK_REALTIME_OFFSET_NS: Mutex<i64> = Mutex::new(0);
}
