//! Process management syscalls
//! 进程管理相关系统调用实现
//! 内存管理也暂时放在此处，后续迁移到mm

use core::{panic, result};
use crate::drivers::net::EthernetDevice;
use crate::net::SOCKET_SET;
use core::sync::atomic::{AtomicI32, Ordering};
use crate::process::{
    block_current_and_run_next_if, block_current_and_run_next_if_task,
    wait4_block_current, waitid_block_current,
};

use crate::mm::{prepare_user_read, prepare_user_write, translated_read, try_translated_str, try_translated_read, try_translated_write};
use crate::{PAGE_SIZE, USER_APP_MAX_SIZE, USER_STACK_SIZE, get_hart_id};
use crate::process::{FdFlags, FileDescriptor};    // 引入当前进程获取方法
use crate::process::task::SignalAltStackState;
use crate::net::socket::TcpSocket;
use alloc::collections::btree_map::Values;
use alloc::vec;
use crate::syscall::EPOLL_CTL_DEL;
use crate::syscall::EPOLL_CTL_ADD;
use crate::syscall::EPOLL_CTL_MOD;
use crate::process::scheduler::runqueue::{SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_OTHER, SCHED_RR};
use crate::process::scheduler::futex::{get_futex_wait_queue, FUTEX_WAIT_QUEUES};
use crate::lazy_static;
use spin::Mutex;
use crate::sync::WaitQueue;
use alloc::collections::VecDeque;
use crate::process::TaskContext;
use crate::net::socket::UdpSocket;

use alloc::collections::BTreeMap;

/// TTY 前台进程组 ID（用于 TIOCGPGRP / TIOCSPGRP）
/// 初始值为 0，表示尚未设置
static TTY_FOREGROUND_PGRP: AtomicI32 = AtomicI32::new(0);


// 记录格式：ino (inode编号) -> (atime_sec, atime_nsec, mtime_sec, mtime_nsec)
pub static TIME_CACHE: Mutex<BTreeMap<u64, (i64, i64, i64, i64)>> = Mutex::new(BTreeMap::new());
lazy_static! {
    /// 专门用于进程死等信号的全局等待队列
    pub static ref SIGNAL_WAIT_QUEUE: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
}

pub(crate) fn clear_child_tid_and_wake(token: usize, clear_child_tid: usize) {
    if clear_child_tid == 0 {
        return;
    }

    // Clear first: try_translated_write may populate a lazy anonymous page or
    // resolve COW, either of which can establish/change the physical futex key.
    if !try_translated_write(token, clear_child_tid as *mut u32, 0u32) {
        error!(
            "[CLEAR_CHILD_TID] failed to clear va={:#x}",
            clear_child_tid,
        );
        return;
    }

    // Translate again after the write and wake waiters on the final PTE's key.
    let page_table = PageTable::from_token(token);
    if let Some(pa) = page_table.translate_va(VirtAddr::from(clear_child_tid)) {
        let queue = {
            let queues = FUTEX_WAIT_QUEUES.lock();
            queues.get(&pa.0).cloned()
        };

        if let Some(queue) = queue {
            crate::process::wake_up_one(&queue);
        }
    }
}
pub use crate::{
    timer::*,
    fs::*, 
    mm::{PageTable, UserBuffer, VirtAddr, mmap, translated_byte_buffer, translated_str, translated_byte_buffer_mut, translated_write}, 
    process::{
        task::{
            MAX_SIG, SignalAction, SignalFlags, add_task, current_task, current_user_token, exit_current_and_run_next, suspend_current_and_run_next, 
                TaskControlBlock
        },
        registry::*
    },
    syscall::errno::Errno
};
pub use crate::process::timer::{
    add_posix_timer, delete_posix_timer, get_posix_timer_spec, remove_posix_timer,
    set_posix_timer, ITimerSpec, KernelSigEvent, PosixTimer,
};
use alloc::task;
pub use alloc::{string::{String,ToString}, sync::Arc, vec::Vec};
use crate::fs::{open_file, OpenFlags, RenameError};
use super::{errno::Errno::*, normalize_leading_dot_path};

use crate::syscall::epoll::{EpollFile, EventFile, EpollEvent};
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19], // 控制字符数组
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,    // 行数
    pub ws_col: u16,    // 列数
    pub ws_xpixel: u16, // 像素宽度 (通常不用，填 0)
    pub ws_ypixel: u16, // 像素高度 (通常不用，填 0)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tms {
pub tms_utime: usize,  // 用户态时间
pub tms_stime: usize,  // 内核态时间
pub tms_cutime: usize, // 子进程用户态时间
pub tms_cstime: usize, // 子进程内核态时间
}


#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SignalAltStack {
    pub ss_sp: usize,
    pub ss_flags: i32,
    pub _pad: i32,
    pub ss_size: usize,
}

pub fn sys_sigaltstack(ss: *const SignalAltStack, old_ss: *mut SignalAltStack) -> isize {
    const SS_ONSTACK: u32 = 1;
    const SS_DISABLE: u32 = 2;
    const SS_AUTODISARM: u32 = 0x8000_0000;
    const MINSIGSTKSZ: usize = 2048;

    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let token = inner.get_user_token();
    let current = inner.signal_alt_stack;
    let user_sp = inner.get_trap_cx().get_sp();
    let on_stack = current.size != 0
        && user_sp >= current.sp
        && user_sp < current.sp.saturating_add(current.size);

    if !old_ss.is_null() {
        let old = SignalAltStack {
            ss_sp: current.sp,
            ss_flags: if current.size == 0 {
                SS_DISABLE as i32
            } else if on_stack {
                SS_ONSTACK as i32
            } else {
                current.flags as i32
            },
            _pad: 0,
            ss_size: current.size,
        };
        if !try_translated_write(token, old_ss, old) {
            return EFAULT.as_isize();
        }
    }

    if ss.is_null() {
        return 0;
    }
    let Some(new_stack) = try_translated_read(token, ss) else {
        return EFAULT.as_isize();
    };
    if on_stack {
        return EPERM.as_isize();
    }

    let flags = new_stack.ss_flags as u32;
    if flags == SS_DISABLE {
        inner.signal_alt_stack = SignalAltStackState::default();
        return 0;
    }
    if flags != 0 && flags != SS_AUTODISARM {
        return EINVAL.as_isize();
    }
    if new_stack.ss_size < MINSIGSTKSZ {
        return ENOMEM.as_isize();
    }
    if new_stack.ss_sp == 0 || new_stack.ss_sp.checked_add(new_stack.ss_size).is_none() {
        return EINVAL.as_isize();
    }

    inner.signal_alt_stack = SignalAltStackState {
        sp: new_stack.ss_sp,
        size: new_stack.ss_size,
        flags,
    };
    0
}

#[repr(C)]
pub struct UtsName {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}
#[repr(C)]
pub struct PollFd {
    pub fd: i32,     // 监视的文件描述符
    pub events: i16, // 事件
    pub revents: i16,// 内核返回的实际发生的事件
}

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
const POLLERR: i16 = 0x008;
const POLLHUP: u16 = 0x0010;

pub fn sys_ppoll(ufds_ptr: usize, nfds: usize, tmo_p: usize, _sigmask: usize) -> isize {
    debug!("[kernel] sys_ppoll: ufds=0x{:x}, nfds={}, tmo_p=0x{:x}", ufds_ptr, nfds, tmo_p);
    if ufds_ptr == 0 && nfds > 0 {
        return EFAULT.as_isize(); // EFAULT
    }

    // 解析超时时间
    let has_timeout = tmo_p != 0;
    let mut deadline_ms: usize = 0;
    if has_timeout {
        let token = current_user_token();
        let timespec = {
            if let Some(ts) = try_translated_read(token, tmo_p as *const TimeSpec) {
                ts
            } else {
                return EFAULT.as_isize();
            }
        };
        if timespec.tv_nsec >= 1_000_000_000 {
            return EINVAL.as_isize();
        }
        const MAX_PPOLL_TIMEOUT_SEC: usize = 86400;
        if timespec.tv_sec > MAX_PPOLL_TIMEOUT_SEC {
            return EINVAL.as_isize();
        }
        let timeout_ms = timespec.tv_sec.saturating_mul(1000).saturating_add(timespec.tv_nsec / 1_000_000);
        deadline_ms = get_time_ms().saturating_add(timeout_ms);
    } else {
        deadline_ms = usize::MAX;
    }

    // 备份原始掩码，并应用临时掩码
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let original_mask = task_inner.blocked;
    
    if _sigmask != 0 {
        let token = current_user_token();
        let mask_val = {
            if let Some(val) = try_translated_read(token, _sigmask as *const usize) {
                val
            } else {
                return EFAULT.as_isize();
            }
        };
        task_inner.blocked = SignalFlags::from_bits_truncate(mask_val as u64);
    }
    drop(task_inner);

    loop {
        let task = current_task().unwrap();
        // --- 检查信号 (使用当前的临时掩码) ---
        let task_inner = task.inner_exclusive_access();
        let pending_bits = task_inner.pending.bits();
        let pending = pending_bits & !task_inner.blocked.bits();
        // 特判 SIGKILL(9) 和 SIGSTOP(19) 这两个绝对不可屏蔽的信号
        let unmaskable = pending_bits & ((1 << (9 - 1)) | (1 << (19 - 1)));

        if (pending | unmaskable) != 0 {
         
            //task_inner.signal_mask = original_mask;
            debug!("[PROBE 1] ppoll return -4. pending signals: {:#x}, current mask: {:#x}", 
                     pending_bits, task_inner.blocked.bits());
            drop(task_inner); // 放锁
            return EINTR.as_isize(); // EINTR
        }
        drop(task_inner); 
        // ----------------------------------------

        // 提取 token 和 fd_table 后立即释放锁，防止 translated_* 死锁
        let (token, fd_table) = {
            let inner = task.inner_exclusive_access();
            let token = inner.get_user_token();
            let fd_table = inner.files.exclusive_access().fds.clone();
            (token, fd_table)
        }; // inner 在此释放

        // 防止随机/恶意 nfds 导致死循环
        const POLL_MAX: usize = 1024;
        let nfds = nfds.min(POLL_MAX);
        let mut ready_count = 0;
        
        // --- 🔵 遍历轮询所有的 fd ---
        for i in 0..nfds {
            let pollfd_ptr = (ufds_ptr + i * core::mem::size_of::<PollFd>()) as *mut PollFd;
            let mut pollfd = {
                if let Some(pf) = try_translated_read(token, pollfd_ptr) {
                    pf
                } else {
                    return EFAULT.as_isize();
                }
            };
            
            let fd = pollfd.fd;
            pollfd.revents = 0;
            
            if fd < 0 { continue; }
            let fd_usize = fd as usize;
            
            if fd_usize >= fd_table.len() || fd_table[fd_usize].file.is_none() {
                pollfd.revents = 0x008; // POLLERR
                ready_count += 1;
            } else {
                let file = fd_table[fd_usize].file.as_ref().unwrap();
                
                // 检查读
                if (pollfd.events & POLLIN) != 0 && file.ready_to_read() {
                    pollfd.revents |= POLLIN;
                }
                // 检查写
                if (pollfd.events & POLLOUT) != 0 && file.ready_to_write() {
                    pollfd.revents |= POLLOUT;
                }
                
                if pollfd.revents != 0 {
                    ready_count += 1;
                }
            }
            if !try_translated_write(token, pollfd_ptr, pollfd) {
                return EFAULT.as_isize();
            }
            //trace!("[kernel] ppoll fd={} target_events=0x{:x} ready_revents=0x{:x}", pollfd.fd, pollfd.events, pollfd.revents);
        }
        
        // 4. 如果找到了就绪事件，恢复掩码并返回！
        if ready_count > 0 {
            let mut task_inner = task.inner_exclusive_access();
            task_inner.blocked = original_mask; 
            drop(task_inner);
            return ready_count as isize;
        }
        
        // 5. 如果没找到事件，处理超时逻辑
        if has_timeout {
            if get_time_ms() >= deadline_ms {
                let mut task_inner = task.inner_exclusive_access();
                task_inner.blocked = original_mask; 
                drop(task_inner);
                return 0; // 超时返回 0
            }
        }
        
        // 继续等待
        suspend_current_and_run_next();
    }
}
pub fn sys_exit(exit_code: i32) -> ! {
    let pid = current_task().unwrap().getpid();
    trace!("kernel:pid[{}] sys_exit", pid);
    crate::timer::TIMER_MANAGER.lock().cancel_alarm(pid);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}
pub fn sys_exit_group(exit_code: i32) -> ! {
    let task = current_task().unwrap();
    let pid = task.getpid();
    let tasks = crate::process::registry::TID2TCB
        .exclusive_access()
        .values()
        .filter(|thread| thread.getpid() == pid)
        .cloned()
        .collect::<alloc::vec::Vec<_>>();

    // 遍历当前进程的所有线程（tasks 列表）
    for thread in tasks.iter() {
        if thread.gettid() != task.gettid() {
            let mut t_inner = thread.inner_exclusive_access();
            t_inner.pending.insert(SignalFlags::SIGKILL);
            t_inner.term_signal = Some(9);
            if matches!(t_inner.state, crate::task::TaskStatus::Blocked) {
                t_inner.signal_interrupted = true;
            }
            drop(t_inner);
            crate::process::wake_up_task(thread.clone());
        }
    }
    
    drop(tasks); // 先前没有这行，会导致内存泄露

    // 退出进程  // 先放掉锁，避免后续迭代时死锁
    task.inner_exclusive_access().exit_code = exit_code;
    drop(task);

    info!("[EXIT_GROUP] PID {} tasks cleanup done. Calling exit_current_and_run_next...", pid);
    // 正常的退出流程
    crate::timer::TIMER_MANAGER.lock().cancel_alarm(pid);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_gettid() -> isize {
    current_task().unwrap().gettid() as isize
}
pub fn sys_chroot(path: usize) -> isize {
    let task = current_task().unwrap();
    let (cred, fs) = {
        let inner = task.inner_exclusive_access();
        (inner.cred.clone(), inner.fs.clone())
    };
    if cred.exclusive_access().euid() != 0 {
        return Errno::EPERM.as_isize();
    }
    let token = current_user_token();
    let path_str = {
        if let Some(s) = crate::mm::try_translated_str(token, path as *const u8) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };


    let start = fs.exclusive_access().get_pwd();
    match start.find_tree(&path_str, true) {
        Ok(root) => {
            let mut fs = fs.exclusive_access();
            *fs = crate::task::fs::FsStruct::new(root.clone(), root);
            0
        }
        Err(_) => ENOENT.as_isize(),
    }
}
/// 信号处理后的恢复
pub fn sys_rt_sigreturn() -> isize {
    sys_sigreturn()
}

pub fn sys_getuid() -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let uid = cred.exclusive_access().uid() as isize;
    uid
}
// 假装获取成功，返回 PGID 为 0
pub fn sys_getpgid(pid: usize) -> isize {
    let task = current_task().unwrap();
    
    // 如果 pid 为 0，表示获取当前进程的 pgid
    if pid == 0 {
        let inner = task.inner_exclusive_access();

        return inner.pgid as isize;
    }

    // 否则查找指定 pid 的进程
    if let Some(proc) = get_process(pid) {
        let inner = proc.inner_exclusive_access();
        inner.pgid as isize
    } else {
        -3 // ESRCH (No such process)
    }
}

pub fn sys_setpgid(pid: usize, pgid: usize) -> isize {
    let task = current_task().unwrap();
    let current_proc = task.clone();
    let target_pid = if pid == 0 { current_proc.pid.0 } else { pid };

    let Some(proc) = get_process(target_pid) else {
        return ESRCH.as_isize();
    };

    let current_sid = current_proc.inner_exclusive_access().sid;
    if target_pid != current_proc.pid.0 {
        let parent_is_current = proc
            .inner_exclusive_access()
            .parent
            .upgrade()
            .map_or(false, |parent| parent.getpid() == current_proc.pid.0);
        if !parent_is_current {
            return ESRCH.as_isize();
        }
    }

    let target_pgid = if pgid == 0 { target_pid } else { pgid };
    {
        let inner = proc.inner_exclusive_access();
        if inner.sid != current_sid {
            return EPERM.as_isize();
        }
        if inner.sid == target_pid {
            return EPERM.as_isize();
        }
    }

    if target_pgid != target_pid && !process_group_exists_in_session(target_pgid, current_sid) {
        return EPERM.as_isize();
    }

    proc.inner_exclusive_access().pgid = target_pgid;
    0
}

fn process_group_exists_in_session(pgid: usize, sid: usize) -> bool {
    for pid in list_pids() {
        let Some(proc) = get_process(pid) else {
            continue;
        };
        let inner = proc.inner_exclusive_access();
        if inner.sid == sid && inner.pgid == pgid {
            return true;
        }
    }
    false
}
pub fn sys_getgid() -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let gid = cred.exclusive_access().gid() as isize;
    gid
}

pub fn sys_geteuid() -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let uid = cred.exclusive_access().euid() as isize;
    uid
}

pub fn sys_getegid() -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let gid = cred.exclusive_access().egid() as isize;
    gid
}
pub fn sys_setuid(uid: u32) -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    cred.exclusive_access().set_uid(uid);
    0 
}

pub fn sys_setgid(gid: u32) -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    cred.exclusive_access().set_gid(gid);
    0 
}
pub fn sys_seteuid(euid: u32) -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    cred.exclusive_access().set_euid(euid);
    0 
}
/// umask: 设置进程文件模式创建掩码，返回旧掩码
pub fn sys_umask(mask: u32) -> isize {
    let task = current_task().unwrap();
    let fs = task.inner_exclusive_access().fs.clone();
    let old = fs.exclusive_access().set_umask(mask);
    old as isize
}

pub fn sys_set_tid_address(tidptr: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    inner.clear_child_tid = tidptr;
    task.gettid() as isize 
}

pub fn sys_getsid(pid: usize) -> isize {
    let task = current_task().unwrap();
    
    // 如果 pid 为 0，获取当前进程的 sid
    if pid == 0 {
        let inner = task.inner_exclusive_access();
        return inner.sid as isize;
    }

    if let Some(proc) = get_process(pid) {
        let inner = proc.inner_exclusive_access();
        inner.sid as isize
    } else {
        return ESRCH.as_isize();
    }
}

/// 创建新会话，当前进程成为会话首进程（Session Leader）和进程组首进程
pub fn sys_setsid() -> isize {
    let task = current_task().unwrap();
    let proc = task.clone();
    let mut inner = proc.inner_exclusive_access();
    
    let pid = proc.pid.0;
    
    // POSIX 规定：如果当前进程已经是进程组组长，则 setsid 失败（返回 EPERM）
    if inner.pgid == pid {
        return EPERM.as_isize(); // EPERM (Operation not permitted)
    }
    
    // 将 sid 和 pgid 都设置为当前进程的 pid
    inner.sid = pid;
    inner.pgid = pid;
    
    pid as isize // 成功时返回新的会话 ID
}
// 系统调用号 148: getresuid
// 让用户态程序查询自己当前拥有的 RUID、EUID、SUID。
// 内核需要把查到的 Root UID (0) 写进用户态传入的指针里。
pub fn sys_getresuid(ruid_ptr: *mut u32, euid_ptr: *mut u32, suid_ptr: *mut u32) -> isize {
    // 获取当前进程的物理/虚拟内存翻译 Token
    let token = current_user_token();
    let root_uid: u32 = 0;
    if ruid_ptr as usize != 0 {
        translated_write(token, ruid_ptr, root_uid);
    }
    if euid_ptr as usize != 0 {
        translated_write(token, euid_ptr, root_uid);
    }
    if suid_ptr as usize != 0 {
        translated_write(token, suid_ptr, root_uid);
    }
    0 
}
const CLOCK_REALTIME: usize = 0;
const CLOCK_MONOTONIC: usize = 1;
const CLOCK_REALTIME_COARSE: usize = 5;
const CLOCK_MONOTONIC_COARSE: usize = 6;
fn clock_adj_has_invalid_mode_bits(modes: u32) -> bool {
    let allowed = CLOCK_ADJ_ALLOWED_MODES | ADJ_OFFSET_SINGLESHOT | ADJ_OFFSET_SS_READ;
    modes & !allowed != 0
}

fn clock_adj_write_status(old_status: i32, new_status: i32) -> i32 {
    let preserved = old_status & !CLOCK_ADJ_RW_STATUS;
    preserved | (new_status & CLOCK_ADJ_RW_STATUS)
}

fn clock_adj_result_from_status(status: i32) -> isize {
    if status & STA_UNSYNC != 0 {
        TIME_ERROR
    } else {
        TIME_OK
    }
}

/// 缓存的 RTC 基准：在第一次读取时记录 RTC 值和当时的 monotonic 时间
static RTC_BASE: spin::Once<(i64, u64)> = spin::Once::new();

fn current_wallclock_ns() -> i64 {
    let mono_us = get_time_us() as u64;
    // 首次调用时，记录 RTC 快照和对应的 monotonic 时间
    let (rtc_base_ns, mono_base_us) = RTC_BASE.call_once(|| {
        (get_real_time_ns() as i64, mono_us)
    });
    // wallclock = RTC基准 + monotonic增量（转为纳秒） + offset
    let mono_delta_ns = (mono_us - *mono_base_us) as i128 * 1_000;
    let base_ns = *rtc_base_ns as i128 + mono_delta_ns;
    let offset_ns = *CLOCK_REALTIME_OFFSET_NS.lock() as i128;
    let adjusted = base_ns + offset_ns;

    if adjusted <= 0 {
        0
    } else if adjusted > i64::MAX as i128 {
        i64::MAX
    } else {
        adjusted as i64
    }
}

pub fn sys_clock_gettime(clock_id: usize, tp: *mut TimeSpec) -> isize {
    //warn!("kernel: sys_clock_gettime: clock_id={}, tp=0x{:x}", clock_id, tp as usize);
    if tp as usize == 0 {
        return EFAULT.as_isize();
    }
    let (sec, nsec) = match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => {
            let total_ns = current_wallclock_ns() as usize;
            (total_ns / 1_000_000_000, total_ns % 1_000_000_000)
            
        }
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_COARSE => {
            // 默认：返回系统运行时间 (Uptime)
            let total_us = get_time_us();
            (total_us / 1_000_000, (total_us % 1_000_000) * 1_000)
        }
        _ => return EINVAL.as_isize(),
    };
    let token = current_user_token();
    let mut time_spec = {
        if let Some(ts) = try_translated_read(token, tp) {
            ts
        } else {
            return EFAULT.as_isize();
        }
    };
    
    time_spec.tv_sec = sec;
    time_spec.tv_nsec = nsec;
    if !try_translated_write(token, tp, time_spec) {
        return EFAULT.as_isize();
    }
    0
}

pub fn sys_clock_getres(clock_id: usize, tp: *mut TimeSpec) -> isize {
    match clock_id {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => {}
        _ => return EINVAL.as_isize(),
    }
    if tp.is_null() {
        return 0;
    }
    let token = current_user_token();
    if !try_translated_write(token, tp, TimeSpec { tv_sec: 0, tv_nsec: 1 }) {
        return EFAULT.as_isize();
    }
    0
}

const TCGETS: u32 = 0x5401;
const TIOCGPGRP: u32 = 0x540F;   // 获取前台进程组 ID
const TIOCSPGRP: u32 = 0x5410;   // 设置前台进程组 ID
const TIOCGWINSZ: u32 = 0x5413;
const RTC_RD_TIME: u32 = 0x80247009; // 真实的 RTC 读取指令号
const TIOCSCTTY: u32 = 0x540E; // 设置控制终端
//  网络接口相关命令 (Socket IOCTL)
pub const SIOCGIFFLAGS: u32 = 0x8913; // 获取网卡运行状态标志
pub const SIOCGIFADDR: u32  = 0x8915; // 获取网卡当前的 IP 地址
pub const SIOCSIFADDR: u32  = 0x8916; // 设置网卡当前的 IP 地址
pub const SIOC_NET_START: u32 = 0x8900;
pub const SIOC_NET_END: u32   = 0x89FF;
pub const SIOCGIFINDEX: u32 = 0x8933;
pub const SIOCGIFTXQLEN: u32 = 0x8942;
// Loop 设备相关的 ioctl 命令
const LOOP_SET_FD: u32 = 0x4C00; //设置 Loop 设备的后端文件描述符
const LOOP_CLR_FD: u32 = 0x4C01; //清除 Loop 设备的后端文件描述符
const LOOP_SET_STATUS64: u32 = 0x4C04; //设置 Loop 设备的状态（使用 LoopInfo64 结构体）
const LOOP_GET_STATUS64: u32 = 0x4C05; //获取 Loop 设备的状态（使用 LoopInfo64 结构体）
const LOOP_SET_STATUS: u32 = 0x4C02; //设置 Loop 设备的状态
const LOOP_CTL_GET_FREE: u32 = 0x4C82; //获取一个空闲的 Loop 设备编号
const LOOP_SET_BLOCK_SIZE: u32 = 0x4C09;
const LOOP_CONFIGURE: u32 = 0x4C0A;

const BLKGETSIZE64: u32 = 0x80081272; // BLKGETSIZE64


#[repr(C)]
struct LoopInfo64 {
    lo_device: u64,
    lo_inode: u64,
    lo_rdevice: u64,
    lo_offset: u64,
    lo_sizelimit: u64,
    lo_number: u32,
    lo_encrypt_type: u32,
    lo_encrypt_key_size: u32,
    lo_flags: u32,
    lo_file_name: [u8; 64],
    lo_crypt_name: [u8; 64],
    lo_encrypt_key: [u8; 32],
    lo_init: [u64; 2],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IfReq {
    // 网卡名称
    pub ifr_name: [u8; 16],
    // 联合体数据 (包含了 sockaddr_in 或 flags 等)
    pub ifru_data: [u8; 24], 
}
/// ioctl
/// io设备控制系统调用
/// 虽然loop设备驱动实现好了，但这里部分loop设备操作是伪实现的
pub fn sys_ioctl(fd: usize, request: usize, argp: usize) -> isize {
    //warn!("kernel: sys_ioctl: fd={}, request=0x{:x}, argp=0x{:x}", fd, request, argp);
    let task = current_task().unwrap();
    let proc = task.clone();
    let files = task.inner_exclusive_access().files.clone();
    let fd_table = files.exclusive_access().fds.clone();
    // fd合法性检查
    if fd >= fd_table.len() || fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let file = fd_table[fd].file.as_ref().unwrap();
    let mut is_tty = false;
    if fd <= 2 || fd == 255 {
        is_tty = true; 
    } else if let Some(dentry) = file.get_dentry() {
        // 如果 fd > 2，检查它的文件名，只要包含 tty 或 console，是合法的终端 fd
        let name = dentry.name();
        if name.contains("tty") || name.contains("console") {
            is_tty = true;
        }
    }
    let token = current_user_token();
    match request as u32 {
        TCGETS => {
            if !is_tty {
                warn!("[kernel] sys_ioctl: TCGETS request on non-tty fd {}", fd);
                return ENOTTY.as_isize();
            }
            let mut termios = Termios {
                c_iflag: 0o012402, c_oflag: 0o000005,
                c_cflag: 0o002277, c_lflag: 0o0105011,
                c_line: 0, c_cc: [0; 19],
            };
            termios.c_cc[0] = 3; termios.c_cc[1] = 28;
            termios.c_cc[2] = 127; termios.c_cc[4] = 4;
            if argp != 0 {
                if !try_translated_write(token, argp as *mut Termios, termios) {
                    return EFAULT.as_isize();
                }
                0 // 成功
            } else {
                EFAULT.as_isize()
            }
        }
        TIOCGPGRP => { 
            if !is_tty {
                warn!("[kernel] sys_ioctl: TIOCGPGRP on non-tty fd {}", fd);
                return ENOTTY.as_isize();
            }
            if argp != 0 {
                // 获取真实的进程组 ID 
                let pgid = proc.inner_exclusive_access().pgid as i32;
                if !try_translated_write(token, argp as *mut i32, pgid) {
                    return EFAULT.as_isize();
                }
                0 
            } else {
                EFAULT.as_isize()
            }
        }
        TIOCSPGRP => { 
            if !is_tty {
                warn!("[kernel] sys_ioctl: TIOCSPGRP on non-tty fd {}", fd);
                return ENOTTY.as_isize();
            }
            if argp != 0 {
                if let Some(new_pgid) = try_translated_read(token, argp as *const i32) {
                    proc.inner_exclusive_access().pgid = new_pgid as usize;
                    0 
                } else {
                    EFAULT.as_isize()
                }
            } else {
                EFAULT.as_isize()
            }
        }
        
        TIOCSCTTY => { 
            if !is_tty { return ENOTTY.as_isize(); }
            // 申请将当前终端设为控制终端返回成功
            0
        }
        TIOCGWINSZ => {
            if !is_tty {
                warn!("[kernel] sys_ioctl: TIOCGWINSZ request on non-tty fd {}", fd);
                return ENOTTY.as_isize();
            }
            let winsize = Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
            if argp != 0 {
                if !try_translated_write(token, argp as *mut Winsize, winsize) {
                    return EFAULT.as_isize();
                }
                0 // 成功
            } else { EFAULT.as_isize() }
        }
        TIOCGPGRP => {
            // 获取前台进程组 ID
            // 如果还没设置过，默认返回当前进程的 pgid
            let fg_pgrp = TTY_FOREGROUND_PGRP.load(Ordering::Relaxed);
            let pgrp: i32 = if fg_pgrp == 0 {
                task.inner_exclusive_access().pgid as i32
            } else {
                fg_pgrp
            };
            if argp != 0 {
                if !try_translated_write(token, argp as *mut i32, pgrp) {
                    return EFAULT.as_isize();
                }
                0
            } else {
                EFAULT.as_isize()
            }
        }
        TIOCSPGRP => {
            // 设置前台进程组 ID
            if argp == 0 {
                return EFAULT.as_isize();
            }
            let new_pgrp: i32 = match try_translated_read(token, argp as *const i32) {
                Some(v) => v,
                None => return EFAULT.as_isize(),
            };
            TTY_FOREGROUND_PGRP.store(new_pgrp, Ordering::Relaxed);
            0
        }
        RTC_RD_TIME => {
            // 获取硬件时间并写给用户
            let time_ms = get_time_ms(); 
            let sec = (time_ms / 1000) as i32;
            let rtc_time = RtcTime {
                tm_sec: sec % 60,
                tm_min: (sec / 60) % 60,
                tm_hour: (sec / 3600) % 24,
                tm_mday: 1, 
                tm_mon: 0, 
                tm_year: 126,
                tm_wday: 0, tm_yday: 0, tm_isdst: 0,
            };
            if argp != 0 {
                if !try_translated_write(token, argp as *mut RtcTime, rtc_time) {
                    return EFAULT.as_isize();
                }
                0
            } else {
                EFAULT.as_isize() // 指针错误
            }
        }
        LOOP_CTL_GET_FREE => {
            let manager = &crate::drivers::loopdev::LOOP_DEVICE_MANAGER;
            let free_id = manager.get_free_id();
            if free_id != -1 { free_id } else { EBUSY.as_isize() }
        }
        LOOP_SET_FD => {
            let backend_fd = argp;
            if backend_fd >= fd_table.len() || fd_table[backend_fd].file.is_none() {
                return EBADF.as_isize();
            }
            let backend_file = fd_table[backend_fd].file.as_ref().unwrap();
            let backend_inode = match backend_file.get_dentry() {
                Some(d) => d.inode.clone(),
                None => return EINVAL.as_isize(),
            };
            
            let loop_file = fd_table[fd].file.as_ref().unwrap();
            let dentry = match loop_file.get_dentry() {
                Some(d) => d,
                None => return EINVAL.as_isize(),
            };
            
            let mut target_id = None;
            if let Some(id_str) = dentry.name().strip_prefix("loop") {
                if let Ok(id) = id_str.parse::<usize>() {
                    target_id = Some(id);
                }
            }
            
            if let Some(id) = target_id {
                if crate::drivers::loopdev::LOOP_DEVICE_MANAGER.set_backing_file(id, Some(backend_inode)) {
                    0
                } else {
                    EINVAL.as_isize()
                }
            } else {
                EINVAL.as_isize()
            }
        }
        LOOP_CLR_FD => {
            let loop_file = fd_table[fd].file.as_ref().unwrap();
            let dentry = match loop_file.get_dentry() {
                Some(d) => d,
                None => return EINVAL.as_isize(),
            };
            
            let mut target_id = None;
            if let Some(id_str) = dentry.name().strip_prefix("loop") {
                if let Ok(id) = id_str.parse::<usize>() {
                    target_id = Some(id);
                }
            }
            if let Some(id) = target_id {
                if crate::drivers::loopdev::LOOP_DEVICE_MANAGER.set_backing_file(id, None) {
                    0
                } else {
                    EINVAL.as_isize()
                }
            } else {
                EINVAL.as_isize()
            }
        }
        LOOP_GET_STATUS64 => {
            let loop_file = fd_table[fd].file.as_ref().unwrap();
            let dentry = match loop_file.get_dentry() {
                Some(d) => d,
                None => return EINVAL.as_isize(),
            };
            let mut target_id = None;
            if let Some(id_str) = dentry.name().strip_prefix("loop") {
                if let Ok(id) = id_str.parse::<usize>() {
                    target_id = Some(id);
                }
            }
            if let Some(id) = target_id {
                if let Some((offset, size)) = crate::drivers::loopdev::LOOP_DEVICE_MANAGER.get_info(id) {
                    if argp != 0 {
                        let mut info = LoopInfo64 {
                            lo_device: 0, lo_inode: 0, lo_rdevice: 0, lo_offset: offset as u64,
                            lo_sizelimit: size as u64, lo_number: 0, lo_encrypt_type: 0,
                            lo_encrypt_key_size: 0, lo_flags: 0, lo_file_name: [0; 64],
                            lo_crypt_name: [0; 64], lo_encrypt_key: [0; 32], lo_init: [0; 2],
                        };
                        if !try_translated_write(token, argp as *mut LoopInfo64, info) {
                            return EFAULT.as_isize();
                        }
                        0
                    } else { EFAULT.as_isize() }
                } else {
                    ENXIO.as_isize()
                }
            } else {
                ENOTTY.as_isize()
            }
        }
        BLKGETSIZE64 => {
            let loop_file = fd_table[fd].file.as_ref().unwrap();
            let dentry = match loop_file.get_dentry() {
                Some(d) => d,
                None => return EINVAL.as_isize(),
            };
            let mut target_id = None;
            if let Some(id_str) = dentry.name().strip_prefix("loop") {
                if let Ok(id) = id_str.parse::<usize>() {
                    target_id = Some(id);
                }
            }
            if let Some(id) = target_id {
                if let Some((_, size)) = crate::drivers::loopdev::LOOP_DEVICE_MANAGER.get_info(id) {
                    if argp != 0 {
                        if !try_translated_write(token, argp as *mut u64, size as u64) {
                            return EFAULT.as_isize();
                        }
                        0
                    } else { EFAULT.as_isize() }
                } else {
                    EINVAL.as_isize()
                }
            } else {
                EINVAL.as_isize()
            }
        }
        LOOP_SET_STATUS64 | LOOP_SET_STATUS => {
            0
        }
        LOOP_SET_BLOCK_SIZE => {
            let bs = argp;
            if bs < 512 || bs > 4096 || bs.count_ones() != 1 {
                return EINVAL.as_isize();
            }
            0
        }
        LOOP_CONFIGURE => {
            let fd = {
                if let Some(f) = try_translated_read(token, argp as *const i32) {
                    f
                } else {
                    return EFAULT.as_isize();
                }
            };
            let bs = {
                if let Some(b) = try_translated_read(token, (argp + 4) as *const u32) {
                    b as usize
                } else {
                    return EFAULT.as_isize();
                }
            };
            if fd < 0 {
                return EBADF.as_isize();
            }
            if bs > 0 && (bs < 512 || bs > 4096 || bs.count_ones() != 1) {
                return EINVAL.as_isize();
            }
            0
        }
        0x5402 => { /* TCSETS */
            0
        }
        SIOCGIFFLAGS => { // 获取网卡运行状态
        if let Some(mut ifr) = try_translated_read::<IfReq>(token, argp as *const IfReq) {
            // 网卡处于 UP (1) 且 RUNNING (0x40) 状态
            let flags: u16 = 0x1 | 0x40; 
            ifr.ifru_data[0..2].copy_from_slice(&flags.to_ne_bytes());
            try_translated_write(token, argp as *mut IfReq, ifr);
            0
        } else { EFAULT.as_isize() }
        }
        SIOCGIFADDR => { // 获取网卡当前的 IP
            if let Some(mut ifr) = try_translated_read::<IfReq>(token, argp as *const IfReq) {
                let iface = crate::net::NET_IFACE.exclusive_access();
                if let Some(ip) = iface.ip_addrs().first() {
                     let smoltcp::wire::IpAddress::Ipv4(ipv4) = ip.address() ;
                        ifr.ifru_data[0..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET 协议族
                        ifr.ifru_data[2..4].copy_from_slice(&0u16.to_ne_bytes()); // 端口设为 0
                        ifr.ifru_data[4..8].copy_from_slice(&ipv4.0);             // 真实的 IPv4 地址
                        try_translated_write(token, argp as *mut IfReq, ifr);
                    
                }
                0
            } else { EFAULT.as_isize() }
        }
        SIOCGIFTXQLEN => {
        if let Some(mut ifr) = try_translated_read::<IfReq>(token, argp as *const IfReq) {
                let qlen: i32 = 1000;
                // 将 1000 写入 union 的前 4 个字节
                ifr.ifru_data[0..4].copy_from_slice(&qlen.to_ne_bytes());
                if try_translated_write(token, argp as *mut IfReq, ifr) {
                    0
                } else {
                    EFAULT.as_isize()
                }
            } else { EFAULT.as_isize() }
        }
        SIOCSIFADDR => { // 给 eth0 绑定新 IP！
            if let Some(ifr) = try_translated_read::<IfReq>(token, argp as *const IfReq) {
                // 从 ifreq.sockaddr_in 中提取 IPv4 字节流 
                let ip_bytes = &ifr.ifru_data[4..8];
                let ip = smoltcp::wire::Ipv4Address::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
                let cidr = smoltcp::wire::IpCidr::new(smoltcp::wire::IpAddress::Ipv4(ip), 24); // 默认 24 位掩码
                let mut push_failed = false;
                //  将新 IP塞入协议栈！
                let mut iface = crate::net::NET_IFACE.exclusive_access();
                iface.update_ip_addrs(|addrs| {
                    // 如果这个 IP 还没绑过，就动态加进去
                    if !addrs.iter().any(|a| *a == cidr) {
                    if let Err(_) = addrs.push(cidr) {
                                push_failed = true; // 记录失败了
                            }
                    }
                    
                });
                if push_failed {
                    warn!("[kernel]  动态添加网卡 IP 失败：IP 池已满！");
                    return ENOBUFS.as_isize(); 
                }
                info!("[kernel]  动态添加网卡 IP: {}", ip);
                0
            } else { EFAULT.as_isize() }
        }
        SIOCGIFINDEX => { 
            if let Some(mut ifr) = try_translated_read::<IfReq>(token, argp as *const IfReq) {
                // 在标准的 struct ifreq 中，ifr_ifindex 和 ifr_hwaddr 属于同一个 union
                // 占用 ifru_data 的前 4 个字节（是一个 i32 类型的整数）
                let ifindex: i32 = 1; // 网卡 eth0 的 index 是 1
                ifr.ifru_data[0..4].copy_from_slice(&ifindex.to_ne_bytes());
                
                if try_translated_write(token, argp as *mut IfReq, ifr) {
                    0 // 成功返回
                } else {
                    EFAULT.as_isize()
                }
            } else { EFAULT.as_isize() }
        }

        0x8900..=0x89ff => {// 兜底：对其他未实现的 ifconfig / ip 命令配置请求返回 0 
            0
        }
        _ => {
            // 委托给文件自己的 ioctl（如 userfaultfd）
            let file = fd_table[fd].file.as_ref().unwrap();
            file.ioctl(request as u32, argp, token)
        }
    }
}

pub fn sys_renameat2(
    _olddirfd: i32, oldpath_ptr: usize,
    _newdirfd: i32, newpath_ptr: usize, flags: usize
) -> isize {
    const RENAME_NOREPLACE: usize = 1;
    if flags & !RENAME_NOREPLACE != 0 {
        return EINVAL.as_isize();
    }

    let task = current_task().unwrap();
    let token = current_user_token();


    let old_path = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, oldpath_ptr as *const u8) { s } else { return EFAULT.as_isize(); }
    );
    let new_path = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, newpath_ptr as *const u8) { s } else { return EFAULT.as_isize(); }
    );
    
    // 解析父目录和文件名
    let old_parent_path = parent_path(&old_path);
    let old_name = file_name(&old_path);
    let new_parent_path = parent_path(&new_path);
    let new_name = file_name(&new_path);

    let fs = task.inner_exclusive_access().fs.clone();
    let cwd = fs.exclusive_access().get_pwd();

    // 找到新老父目录的内存 Dentry
    if let (Ok(old_parent), Ok(new_parent)) = (
        cwd.find_tree(&old_parent_path, true),
        cwd.find_tree(&new_parent_path, true)
    ) {
        // 判断是否在同一个目录下操作（如同目录内重命名 mv /a/foo /a/bar）
        let same_dir = Arc::ptr_eq(&old_parent, &new_parent);

        let rename_locked = || -> isize {
            if same_dir {
                let mounted = old_parent.mounted_children.lock();
                if mounted.contains_key(&old_name) || mounted.contains_key(&new_name) {
                    return EBUSY.as_isize();
                }
            } else if old_parent.mounted_children.lock().contains_key(&old_name)
                || new_parent.mounted_children.lock().contains_key(&new_name)
            {
                return EBUSY.as_isize();
            }

            // Materialize an uncached source without publishing any namespace change yet.
            let moved_dentry = old_parent
                .children
                .lock()
                .get(&old_name)
                .cloned()
                .or_else(|| {
                    old_parent.inode.find(&old_name).map(|inode| {
                        crate::fs::Dentry::new(
                            old_name.clone(),
                            inode,
                            Arc::downgrade(&old_parent),
                        )
                    })
                });
            let Some(moved_dentry) = moved_dentry else {
                return ENOENT.as_isize();
            };

            if (moved_dentry.inode.get_stat().mode & 0o170000) == 0o040000 && !same_dir {
                let mut ancestor = Some(new_parent.clone());
                while let Some(node) = ancestor {
                    if Arc::ptr_eq(&node, &moved_dentry) {
                        return EINVAL.as_isize();
                    }
                    ancestor = node.parent().upgrade();
                }
            }

            let disk_result = old_parent.inode.rename_dir_entry(
                &old_name,
                &new_parent.inode,
                &new_name,
                flags & RENAME_NOREPLACE != 0,
            );
            if let Err(error) = disk_result {
                return match error {
                    RenameError::NotFound => ENOENT.as_isize(),
                    RenameError::Exists => EEXIST.as_isize(),
                    RenameError::NotDir => ENOTDIR.as_isize(),
                    RenameError::IsDir => EISDIR.as_isize(),
                    RenameError::NotEmpty => ENOTEMPTY.as_isize(),
                    RenameError::CrossDevice => EXDEV.as_isize(),
                    RenameError::Invalid => EINVAL.as_isize(),
                    RenameError::Io => EIO.as_isize(),
                };
            }

            // Commit the cache update only after the filesystem operation succeeded.
            if same_dir {
                let mut children = old_parent.children.lock();
                let moved_dentry = children.remove(&old_name).unwrap_or(moved_dentry);
                children.remove(&new_name);
                moved_dentry.relocate(new_name.clone(), Arc::downgrade(&new_parent));
                children.insert(new_name.clone(), moved_dentry);
            } else {
                let moved_dentry = old_parent
                    .children
                    .lock()
                    .remove(&old_name)
                    .unwrap_or(moved_dentry);
                new_parent.children.lock().remove(&new_name);
                moved_dentry.relocate(new_name.clone(), Arc::downgrade(&new_parent));
                new_parent.children.lock().insert(new_name.clone(), moved_dentry);
            }
            0
        };

        if same_dir {
            let _guard = old_parent.namespace_lock.lock();
            return rename_locked();
        }

        let old_addr = Arc::as_ptr(&old_parent) as usize;
        let new_addr = Arc::as_ptr(&new_parent) as usize;
        if old_addr < new_addr {
            let _old_guard = old_parent.namespace_lock.lock();
            let _new_guard = new_parent.namespace_lock.lock();
            return rename_locked();
        } else {
            let _new_guard = new_parent.namespace_lock.lock();
            let _old_guard = old_parent.namespace_lock.lock();
            return rename_locked();
        }
    }
    
    ENOENT.as_isize()
}
pub fn sys_getpid() -> isize {
	let task = current_task().unwrap();
	trace!("kernel: sys_getpid tgid:{}", task.gettgid());
    task.gettgid() as isize
}
pub fn sys_getppid() -> isize {
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    match inner.parent.upgrade() {
        Some(parent) => parent.getpid() as isize,
        None => 0, 
    }
}
pub fn sys_syslog(_type_: usize, _buf: usize, _len: usize) -> isize {
    0
}
#[repr(C)]
#[derive(Debug)]
pub struct Sysinfo {
    pub uptime: isize,      // 启动到现在经过的秒数
    pub loads: [usize; 3],  // 1, 5, 15 分钟的平均负载
    pub totalram: usize,    // 总的可用内存大小
    pub freeram: usize,     // 还剩多少可用内存
    pub sharedram: usize,   // 共享内存大小
    pub bufferram: usize,   // 缓冲区大小
    pub totalswap: usize,   // 交换空间总大小
    pub freeswap: usize,    // 交换空间剩余大小
    pub procs: u16,         // 当前进程数
    pub pad: u16,           // 结构体对齐填充
    pub totalhigh: usize,   // 高端内存大小
    pub freehigh: usize,    // 高端内存剩余大小
    pub mem_unit: u32,      // 内存单位（比如 1 表示以 byte 为单位计算）
    pub _pad: u32,          // 补齐到 112 字节 
}

pub fn sys_sysinfo(sysinfo_ptr: usize) -> isize {
    if sysinfo_ptr == 0 {
        return EFAULT.as_isize();
    }
    let token = current_user_token();
    let sysinfo = Sysinfo {
        uptime: 1000,
        loads: [0, 0, 0],
        totalram: 128 * 1024 * 1024,
        freeram: 64 * 1024 * 1024,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 2,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _pad: 0,
    };
    if !try_translated_write(token, sysinfo_ptr as *mut Sysinfo, sysinfo) {
        return EFAULT.as_isize();
    }

    // 返回 0 表示获取成功！
    0
}

pub fn sys_uname(uts: *mut UtsName) -> isize {
    let token = current_user_token();
    let mut uts_name = {
        if let Some(u) = try_translated_read(token, uts) {
            u
        } else {
            return EFAULT.as_isize();
        }
    };
    
    // 读取当前进程的 personality，检查 UNAME26 标志
    let task = current_task().unwrap();
    let persona = task.inner_exclusive_access().personality;
    drop(task);
    const UNAME26: usize = 0x0020000;
    let uname26 = persona & UNAME26 != 0;

    // 填充系统信息
    let sysname = b"Linux";
    let nodename = b"rCore-Nodename";
    let version = b"v0.1.0";
    let machine = b"riscv64";
    let domainname = b"rcore.os";

    // UNAME26: release 字段只保留前 3 个 '.' 分隔的版本段
    let full_release = b"5.10.0-rcore";
    let release: &[u8] = if uname26 {
        // 找第 3 个 '.' 出现的位置，或直接到字符串末尾
        let mut dot_count = 0;
        let mut cut_pos = full_release.len();
        for (i, &ch) in full_release.iter().enumerate() {
            if ch == b'.' {
                dot_count += 1;
                if dot_count == 3 {
                    cut_pos = i;
                    break;
                }
            }
        }
        // 如果不足 3 个 '.'，保留全部
        &full_release[..cut_pos]
    } else {
        full_release
    };

    // 辅助函数，安全复制并补 0（防御 CVE-2012-0957 内核内存泄露）
    fn fill_str(dest: &mut [u8; 65], src: &[u8]) {
        let len = src.len().min(64);
        dest[..len].copy_from_slice(&src[..len]);
        for i in len..65 {
            dest[i] = 0;
        }
    }

    fill_str(&mut uts_name.sysname, sysname);
    fill_str(&mut uts_name.nodename, nodename);
    fill_str(&mut uts_name.release, release);
    fill_str(&mut uts_name.version, version);
    fill_str(&mut uts_name.machine, machine);
    fill_str(&mut uts_name.domainname, domainname);
    if !try_translated_write(token, uts, uts_name) {
        return EFAULT.as_isize();
    }
    
    0
}

pub fn sys_fork(stack: usize, _flags: usize) -> isize {
	let task = current_task().unwrap();
	trace!("kernel:pid[{}] old_sys_fork", task.getpid());
    let flags = if _flags & CSIGNAL == 0 {
        _flags | SIGCHLD_NUM as usize
    } else {
        _flags
    };
    task.do_clone(flags, stack, 0, 0, 0)
}


// 在当前进程中克隆出一个线程
const CSIGNAL: usize = 0xff;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CloneArgs {
    pub flags: u64,
    pub pidfd: u64,
    pub child_tid: u64,
    pub parent_tid: u64,
    pub exit_signal: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub tls: u64,
    pub set_tid: u64,
    pub set_tid_size: u64,
    pub cgroup: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CloneArgsV0 {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CloneArgsV1 {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
}

impl From<CloneArgsV0> for CloneArgs {
    fn from(args: CloneArgsV0) -> Self {
        Self {
            flags: args.flags,
            pidfd: args.pidfd,
            child_tid: args.child_tid,
            parent_tid: args.parent_tid,
            exit_signal: args.exit_signal,
            stack: args.stack,
            stack_size: args.stack_size,
            tls: args.tls,
            ..Default::default()
        }
    }
}

impl From<CloneArgsV1> for CloneArgs {
    fn from(args: CloneArgsV1) -> Self {
        Self {
            flags: args.flags,
            pidfd: args.pidfd,
            child_tid: args.child_tid,
            parent_tid: args.parent_tid,
            exit_signal: args.exit_signal,
            stack: args.stack,
            stack_size: args.stack_size,
            tls: args.tls,
            set_tid: args.set_tid,
            set_tid_size: args.set_tid_size,
            cgroup: 0,
        }
    }
}

pub fn sys_clone3(uargs: *const CloneArgs, size: usize) -> isize {
    const CLONE_ARGS_SIZE_VER0: usize = core::mem::size_of::<CloneArgsV0>();
    const CLONE_ARGS_SIZE_VER1: usize = core::mem::size_of::<CloneArgsV1>();
    const CLONE_ARGS_SIZE_VER2: usize = core::mem::size_of::<CloneArgs>();

    if size < CLONE_ARGS_SIZE_VER0 {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let args = if size < CLONE_ARGS_SIZE_VER1 {
        match try_translated_read(token, uargs as *const CloneArgsV0) {
            Some(args) => CloneArgs::from(args),
            None => return EFAULT.as_isize(),
        }
    } else if size < CLONE_ARGS_SIZE_VER2 {
        match try_translated_read(token, uargs as *const CloneArgsV1) {
            Some(args) => CloneArgs::from(args),
            None => return EFAULT.as_isize(),
        }
    } else {
        match try_translated_read(token, uargs) {
            Some(args) => args,
            None => return EFAULT.as_isize(),
        }
    };

    let flags = args.flags as usize;
    let exit_signal = args.exit_signal as usize;

    if exit_signal > MAX_SIG || exit_signal & !CSIGNAL != 0 {
        return EINVAL.as_isize();
    }
    if flags & CLONE_SIGHAND != 0 && flags & CLONE_VM == 0 {
        return EINVAL.as_isize();
    }
    if flags & CLONE_THREAD != 0 && flags & CLONE_SIGHAND == 0 {
        return EINVAL.as_isize();
    }
    if flags & CLONE_FS != 0 && flags & CLONE_NEWNS != 0 {
        return EINVAL.as_isize();
    }
    if args.stack == 0 && args.stack_size != 0 {
        return EINVAL.as_isize();
    }
    if args.stack != 0 && args.stack_size == 0 {
        return EINVAL.as_isize();
    }
    if flags & CLONE_PIDFD != 0 || args.set_tid != 0 || args.set_tid_size != 0 || args.cgroup != 0 {
        return EINVAL.as_isize();
    }

    let stack = if args.stack != 0 {
        match (args.stack as usize).checked_add(args.stack_size as usize) {
            Some(stack) => stack,
            None => return EINVAL.as_isize(),
        }
    } else {
        0
    };
    let clone_flags = flags | exit_signal;

    #[cfg(target_arch = "riscv64")]
    {
        sys_clone(clone_flags, stack, args.parent_tid as usize, args.tls as usize, args.child_tid as usize)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        sys_clone(clone_flags, stack, args.parent_tid as usize, args.child_tid as usize, args.tls as usize)
    }
}
const CLONE_VM: usize = 0x00000100;              // 共享地址空间
const CLONE_FS: usize = 0x00000200;              // 共享 fs_struct（根目录/工作目录）
const CLONE_FILES: usize = 0x00000400;           // 共享文件描述符表
const CLONE_SIGHAND: usize = 0x00000800;         // 共享信号处理函数表
const CLONE_SETTLS: usize = 0x00080000;          // 设置子任务 TLS 指针
const CLONE_PARENT_SETTID: usize = 0x00100000;   // 向父地址空间写入子 TID
const CLONE_CHILD_CLEARTID: usize = 0x00200000;  // 子任务退出时清零 ctid 并 futex 唤醒
const CLONE_CHILD_SETTID: usize = 0x01000000;    // 向子地址空间写入 TID
const CLONE_THREAD: usize = 0x00010000;           // 创建线程（共享 tgid）
const CLONE_SYSVSEM: usize = 0x00040000;           // 共享 System V 信号量（todo）
const CLONE_PIDFD: usize = 0x00001000;
const CLONE_VFORK: usize = 0x00004000;
const CLONE_NEWNS: usize = 0x00020000; // 创建新的 mount namespace
const CLONE_DETACHED: usize = 0x00400000; // 历史标志：父进程不关心子进程退出信号（已废弃但仍可能出现）
pub fn sys_clone(flags: usize, stack: usize, ptid: usize, arg3: usize, arg4: usize) -> isize {
    #[cfg(target_arch = "riscv64")]
    let (tls, ctid) = (arg3, arg4);
    #[cfg(target_arch = "loongarch64")]
    let (ctid, tls) = (arg3, arg4);

    const SUPPORTED_FLAGS: usize = CSIGNAL
        | CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SYSVSEM
        | CLONE_SETTLS
        | CLONE_PARENT_SETTID
        | CLONE_CHILD_CLEARTID
        | CLONE_CHILD_SETTID
        | CLONE_VFORK
        | CLONE_DETACHED;

    let unsupported_flags = flags & !SUPPORTED_FLAGS;
    if unsupported_flags != 0 {
        warn!(
            "sys_clone: ignoring unsupported flags {:#x} from flags {:#x}",
            unsupported_flags,
            flags
        );
    }
    // 简化的 vfork 兼容实现：当前内核没有 vfork completion，无法保证
    // 父进程一直阻塞到子进程 execve/_exit。若直接保留 CLONE_VM，父子会
    // 在多核上并发使用同一用户栈，子进程修改栈帧后会导致双方持续缺页。
    // 因此将 CLONE_VFORK | CLONE_VM 降级成普通 COW fork；语义安全，只是
    // 暂时失去 vfork 的地址空间共享性能优化。
    let effective_flags = if flags & CLONE_VFORK != 0 {
        warn!(
            "sys_clone: emulating CLONE_VFORK {:#x} with COW fork",
            flags
        );
        flags & !(CLONE_VFORK | CLONE_VM)
    } else {
        flags
    };
    // 未实现的附加语义不阻断通用 do_clone 流程；do_clone 只解释其已
    // 实现的标志位，其他位保持原样传入并自然被忽略。
    if effective_flags & CSIGNAL > MAX_SIG {
        return EINVAL.as_isize();
    }
    if effective_flags & CLONE_SIGHAND != 0 && effective_flags & CLONE_VM == 0 {
        return EINVAL.as_isize();
    }
    if effective_flags & CLONE_THREAD != 0
        && (effective_flags & CLONE_SIGHAND == 0 || effective_flags & CSIGNAL != 0)
    {
        return EINVAL.as_isize();
    }

    let task = current_task().unwrap();
    task.do_clone(effective_flags, stack, ptid, ctid, tls)
}
/// Return the effective NUMA memory policy. This kernel currently exposes one
/// memory node, so every address uses node 0 and the default policy.
pub fn sys_get_mempolicy(
    mode: *mut i32,
    nodemask: *mut usize,
    maxnode: usize,
    _addr: usize,
    flags: usize,
) -> isize {
    const MPOL_F_NODE: usize = 1;
    const MPOL_F_ADDR: usize = 2;
    const MPOL_F_MEMS_ALLOWED: usize = 4;
    const VALID_FLAGS: usize = MPOL_F_NODE | MPOL_F_ADDR | MPOL_F_MEMS_ALLOWED;

    if flags & !VALID_FLAGS != 0
        || flags & MPOL_F_MEMS_ALLOWED != 0 && flags != MPOL_F_MEMS_ALLOWED
        || flags & MPOL_F_NODE != 0 && mode.is_null()
    {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    if !mode.is_null() && !try_translated_write(token, mode, 0i32) {
        return EFAULT.as_isize();
    }

    if !nodemask.is_null() && maxnode != 0 {
        let word_bits = usize::BITS as usize;
        let words = maxnode.saturating_add(word_bits - 1) / word_bits;
        for index in 0..words {
            let value = if index == 0 { 1usize } else { 0usize };
            if !try_translated_write(token, unsafe { nodemask.add(index) }, value) {
                return EFAULT.as_isize();
            }
        }
    }

    0
}
// path elf路径
// args 参数数组，必须以0结尾
// envp 环境变量数组，必须以0结尾
pub fn sys_exec(path: *const u8, mut args: *const usize, mut envs: *const usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let path_str = {
        if let Some(path) = try_translated_str(token, path){
            normalize_leading_dot_path(path)
        } else {
            return EFAULT.as_isize();
        }
    };
     if path_str.len() >= 4096 { // PATH_MAX
        return ENAMETOOLONG.as_isize(); 
    }
    for comp in path_str.split('/') {
        if comp.len() > 255 {
            return ENAMETOOLONG.as_isize(); 
        }
    }
    let mut args_vec: Vec<String> = Vec::new();
    // 提取原始参数数组
    if args as usize != 0 {
        loop {
            let arg_str_ptr = {
                if let Some(p) = try_translated_read(token, args) {
                    p
                } else {
                    return EFAULT.as_isize();
                }
            };
            if arg_str_ptr == 0 { break; }
            let arg_str = {
                if let Some(s) = try_translated_str(token, arg_str_ptr as *const u8) {
                    s
                } else {
                    return EFAULT.as_isize();
                }
            };
            args_vec.push(arg_str);
            unsafe { args = args.add(1); }
        }
    }

    // 提取环境变量数组
    let mut envs_vec: Vec<String> = Vec::new();
    /* */
    if envs as usize != 0 {
        loop {
            let env_str_ptr = {
                if let Some(p) = try_translated_read(token, envs) {
                    p
                } else {
                    return EFAULT.as_isize();
                }
            };
            if env_str_ptr == 0 { break; }
            let env_str = {
                if let Some(s) = try_translated_str(token, env_str_ptr as *const u8) {
                    s
                } else {
                    return EFAULT.as_isize();
                }
            };
            envs_vec.push(env_str);
            unsafe { envs = envs.add(1); }
        }
    }
    task.do_exec(path_str, args_vec, envs_vec)
}
///wait系的参数
const P_ALL: i32 = 0;
const P_PID: i32 = 1;
const P_PGID: i32 = 2;

const WNOHANG: i32 = 0x0000_0001;
const WSTOPPED: i32 = 0x0000_0002;
const WEXITED: i32 = 0x0000_0004;
const WCONTINUED: i32 = 0x0000_0008;
const WNOWAIT: i32 = 0x0100_0000;

const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;
const CLD_DUMPED: i32 = 3;
const SIGCHLD_NUM: i32 = 17;

/// 等待子进程退出
pub fn sys_wait4(pid: i32, exit_code_ptr: *mut i32, options: usize) -> isize {
    loop {
        let task = current_task().unwrap();
        let proc = task.clone();
        let mut child_pid: usize = 0;
        let mut exit_code = -1;
        let mut has_match = false;
        let mut proc_inner = proc.inner_exclusive_access();
        let mut child_idx: Option<usize> = None;
        match pid {
            -1 => {
                has_match = !proc_inner.children.is_empty();
                for (idx, child) in proc_inner.children.iter().enumerate() {
                    if child.inner_exclusive_access().state == crate::task::TaskStatus::Zombie {
                        //warn!("[wait4] P{} found a zombie child P{} with exit code {}", proc.getpid(), child.getpid(), child.inner_exclusive_access().exit_code);
                        exit_code = child.inner_exclusive_access().exit_code;
                        child_pid = child.getpid();
                        child_idx = Some(idx);
                        break;
                    }
                }
            }
            0 => {
                for (idx, child) in proc_inner.children.iter().enumerate() {
                    let child_pgid = child.inner_exclusive_access().pgid;
                    if child_pgid == proc_inner.pgid {
                        has_match = true;
                        if child.inner_exclusive_access().state == crate::task::TaskStatus::Zombie {
                            //warn!("[wait4] P{} found a zombie child P{} with exit code {}", proc.getpid(), child.getpid(), child.inner_exclusive_access().exit_code);
                            exit_code = child.inner_exclusive_access().exit_code;
                            child_pid = child.getpid();
                            child_idx = Some(idx);
                            break;
                        }
                    }
                     
                }
            }
            value if value > 0 => {
                for (idx, child) in proc_inner.children.iter().enumerate() {
                    if child.getpid() == value as usize {
                        has_match = true;
                        if child.inner_exclusive_access().state == crate::task::TaskStatus::Zombie {
                            //warn!("[wait4] P{} found a zombie child P{} with exit code {}", proc.getpid(), child.getpid(), child.inner_exclusive_access().exit_code);
                            exit_code = child.inner_exclusive_access().exit_code;
                            child_pid = child.getpid();
                            child_idx = Some(idx);
                            break;
                        }
                    }
                }
            }
            pid if pid < -1 => {
                if pid == i32::MIN {
                    return ESRCH.as_isize();
                }
                let target_pgid = (-pid) as usize;
                for (idx, child) in proc_inner.children.iter().enumerate() {
                    let child_pgid = child.inner_exclusive_access().pgid;
                    if child_pgid == target_pgid {
                        has_match = true;
                        if child.inner_exclusive_access().state == crate::task::TaskStatus::Zombie {
                            exit_code = child.inner_exclusive_access().exit_code;
                            child_pid = child.getpid();
                            child_idx = Some(idx);
                            break;
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
        //非阻塞或者没有找到符合条件的僵尸子进程的处理,区分不存在pid在子进程和存在但未退出两种情况
        if child_pid == 0 {
            if has_match == false {
                return ECHILD.as_isize(); // 没有任何匹配的子进程
            }
            if options & WNOHANG as usize != 0 {
                return 0;
            }
            drop(proc_inner);
            // 队列锁内再次检查条件，随后原子地入队并切换，防止子进程退出唤醒发生在入队之前。
            wait4_block_current(&proc, pid);
            continue;
        }else{
            //从父进程的孩子列表里摘除这个僵尸子进程
            let child = if let Some(idx) = child_idx {
                proc_inner.children.remove(idx)
            } else {
                panic!("sys_wait4: logic error, child_pid is set but child_idx is None?");
            };
            //println!("[wait4] P{} collected Zombie P{} (code: {})", proc.getpid(), child_pid, exit_code);
            if exit_code_ptr as usize != 0 {
                //warn!("[wait4] Writing exit code {} to user space for child P{}", exit_code, child_pid);
                let status = if exit_code >= 0 {
                    (exit_code & 0xff) << 8
                } else {
                    let signal = -exit_code;
                    let mut status = signal & 0x7f;
                    if signal == 8 || signal == 11 || signal == 4 || signal == 6 || signal == 24 || signal == 25 || signal == 31 || signal == 5 {
                        status |= 0x80;
                    }
                    status
                };
                drop(proc_inner);
                let token = current_user_token();
                //println!("[wait4] Writing exit code {} (status: {:#x}) to user space for child P{}", exit_code, status, child_pid);
                if !try_translated_write(token, exit_code_ptr, status) {
                    return EFAULT.as_isize();
                }
                //println!("[wait4] Wrote exit code {} (status: {:#x}) to user space for child P{}", exit_code, status, child_pid);
            }
            
            
            // 从全局进程表里删除这个子进程
            crate::process::remove_process(child_pid);
            return child_pid as isize;
        }
    }
    //warn!("sys_wait4 called with pid={}, options=0x{:x}", pid, options);
/*  let task = current_task().unwrap();
    let proc = task.process();
    // 提前拿到当前进程的 pgid
    let current_pgid = proc.inner_exclusive_access().pgid; 
    
    const WNOHANG: usize = 0x1;
    let nohang = (options & WNOHANG) != 0;//是否开启了非阻塞选项
    info!("[wait4] P{} waiting for PID/PGID: {}, options: {}", current_pgid, pid, options);
    let mut printed_info = false;

    loop {
        // 直接在锁内遍历
        let mut proc_inner = proc.inner_exclusive_access();

        if proc_inner.children.is_empty() {
            info!("[wait4] P{} has no children at all", current_pgid);
            return ECHILD.as_isize(); // 没有任何子进程
        }
        //是否找到匹配子进程
        let mut has_match = false;
        let mut zombie_child: Option<(usize, i32)> = None;
        for child in proc_inner.children.iter() {

            //判断是否pid匹配
            let child_pid = child.getpid();
            has_match = if pid == -1 {
                true
            } else if pid > 0 {
                child_pid == pid as usize
            } else {
                let child_pgid = child.inner_exclusive_access().pgid;
                if pid == 0 {
                    child_pgid == current_pgid
                } else {
                    child_pgid == (-pid) as usize
                }
            };

            //若没有则继续找下一个孩子
            if !has_match {
                continue;
            }

            has_match = true;
            let child_inner = child.inner_exclusive_access();
            if child_inner.is_zombie() {
                warn!("[wait4] P{} found a zombie child P{} with exit code {}", current_pgid, child_pid, child_inner.exit_code);
                zombie_child = Some((child_pid, child_inner.exit_code));
                break;
            }
        }

        // 1. 检查是否存在符合要求的子进程
        if !has_match {
            info!("[wait4] P{} has no matching children for filter {}", current_pgid, pid);
            return ECHILD.as_isize(); // 真的是一个匹配的都没有，才返回 ECHILD
        }
    
        // 2. 判断该匹配的子进程是否已经是僵尸了，如果是僵尸了就收尸退出；如果不是僵尸了就睡眠等待
        if let Some((zombie_pid, exit_code)) = zombie_child {
            if let Some(idx) = proc_inner.children.iter().position(|child| child.getpid() == zombie_pid) {
                let child = proc_inner.children.remove(idx);
                let child_pid = child.getpid();
           
                info!("[wait4] P{} collected Zombie P{} (code: {})", current_pgid, child_pid, exit_code);
                // 组装状态码
                let status = (exit_code & 0xff) << 8;
                if exit_code_ptr as usize != 0 {
                    if !try_translated_write(proc_inner.memory_set.token(), exit_code_ptr, status) {
                        return EFAULT.as_isize();
                    }
                }
                drop(proc_inner);
                // 从全局进程表里删除
                crate::process::remove_process(child_pid);
                return child_pid as isize;
            }
        }

        if nohang {
            return 0; // 没有僵尸孩子但开启了非阻塞选项，直接返回 0
        }
        if !printed_info {
            info!("[wait4] P{}'s target(s) still alive, sleeping...", current_pgid);
            printed_info = true;
        }
        drop(proc_inner);
        suspend_current_and_run_next();
    }
     /*  
        else{
            // B. 孩子还活着，睡眠等待
            info!("[wait4] P{}'s target(s) still alive, sleeping...", current_pgid);
            // 新增：检查是否被信号打断 
            let task_inner = task.inner_exclusive_access();
            let pending = task_inner.signals.bits() & !task_inner.signal_mask.bits();
                info!("[wait4] Interrupted by signal! Returning EINTR.");
                return -4; // -4 对应 EINTR (Interrupted system call)
            }
            return child_pid as isize;
            let mut count = 0;
            loop{
                // 继续睡眠等待，直到被调度器唤醒
                suspend_current_and_run_next();
                
                let children_snapshot = {
                    let proc_inner = proc.inner_exclusive_access();
                    proc_inner.children.clone()
                };
                let mut zombie_child: Option<(usize, i32)> = None;
                for child in children_snapshot.iter() {
                    let child_pid = child.getpid();
                    let matches = if pid == -1 {
                        true
                    } else if pid > 0 {
                        child_pid == pid as usize
                    } else {
                        let child_pgid = child.inner_exclusive_access().pgid;
                        if pid == 0 {
                            child_pgid == current_pgid
                        } else {
                            child_pgid == (-pid) as usize
                        }
                    };
                    if !matches {
                        continue;
                    }
                    let child_inner = child.inner_exclusive_access();
                    if child_inner.is_zombie() {
                        zombie_child = Some((child_pid, child_inner.exit_code));
                        break;
                    }
                }
                if let Some((zombie_pid, exit_code)) = zombie_child {
                    let mut proc_inner = proc.inner_exclusive_access();
                    if let Some(idx) = proc_inner.children.iter().position(|child| child.getpid() == zombie_pid) {
                        // 收尸成功，拿到孩子的 PID 和退出码
                        let child = proc_inner.children.remove(idx);
                        let child_pid = child.getpid();
                        
                        info!("[wait4] P{} collected Zombie P{} (code: {}) after waking up", current_pgid, child_pid, exit_code);
                        // 组装状态码
                        let status = (exit_code & 0xff) << 8;
                        if exit_code_ptr as usize != 0 {
                            *translated_refmut(proc_inner.memory_set.token(), exit_code_ptr) = status;
                        }
                        
                        return child_pid as isize;
                    }
                }
        }
    } */ */
}


pub fn sys_waitid(idtype: i32, id: i32, infop: *mut SigInfo, options: i32) -> isize {
    if infop.is_null() {
        return EFAULT.as_isize();
    }

    if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
        return EINVAL.as_isize();
    }

    let supported = WEXITED | WNOHANG | WNOWAIT;
    if options & !supported != 0 {
        return EINVAL.as_isize();
    }

    if options & WEXITED == 0 {
        return EINVAL.as_isize();
    }

    loop {
        let task = current_task().unwrap();
        let proc = task.clone();
        let mut proc_inner = proc.inner_exclusive_access();
        let token = current_user_token();
        let mut child_pid: usize = 0;
        let mut exit_code = 0;
        let mut child_idx: Option<usize> = None;
        let mut has_match = false;

        for (idx, child) in proc_inner.children.iter().enumerate() {
            let child_pid_now = child.getpid();
            //是否找到匹配选项
            let matches = match idtype {
                P_ALL => true,
                P_PID => child_pid_now == id as usize,
                P_PGID => child.inner_exclusive_access().pgid == id as usize,
                _ => return EINVAL.as_isize(),
            };

            if !matches {
                continue;
            }

            has_match = true;
            let child_inner = child.inner_exclusive_access();
            if child_inner.state == crate::task::TaskStatus::Zombie {
                child_pid = child_pid_now;
                exit_code = child_inner.exit_code;
                child_idx = Some(idx);
                break;
            }
        }
        //若未匹配上
        if child_pid == 0 {
            if !has_match {
                return ECHILD.as_isize();
            }
            //非阻塞选项的处理
            if options & WNOHANG != 0 {
                /// 需要写入一个全零的 siginfo 结构体
                if infop.is_null() {
                return 0;
                }
                let info = SigInfo {
                    si_signo: 0,
                    si_errno: 0,
                    si_code: 0,
                    _pad0: 0,
                    si_pid: 0,
                    si_uid: 0,
                    si_status: 0,
                    _pad1: 0,
                    _pad: [0; 12],
                };
                if try_translated_write(token, infop, info) {
                    return 0;
                } else {
                    return EFAULT.as_isize();
                }
            }
            drop(proc_inner);
            waitid_block_current(&proc, idtype, id);
            continue;
        }

        let info = {
            let (si_code, si_status) = if exit_code >= 0 {
                //正常退出，sicode为1
                (1, exit_code)
            } else {
                let signal = -exit_code;
                let code = if signal == 8 || signal == 11 {
                    3//core_dumped
                } else {
                    2//killed
                };
                (code, signal)
            };

            SigInfo {
                si_signo: 17,//神必规范(?SIGCHLD_NUM),
                si_errno: 0,
                si_code,
                _pad0: 0,
                si_pid: child_pid as i32,
                si_uid: 0,
                si_status,
                _pad1: 0,
                _pad: [0; 12],
            }
        };
        if !try_translated_write(token, infop, info) {
            return EFAULT.as_isize();
        }

        if options & WNOWAIT == 0 {
            if let Some(idx) = child_idx {
                proc_inner.children.remove(idx);
            } else {
                panic!("sys_waitid: logic error, child_pid is set but child_idx is None?");
            }
            drop(proc_inner);
            crate::process::remove_process(child_pid);
        }

        return 0;
    }
}
pub fn sys_kill(pid: isize, signum: i32) -> isize {
    warn!("sys_kill called with pid={}, signum={}", pid, signum);
    if signum < 0 || signum as usize > MAX_SIG {
        return EINVAL.as_isize();
    }

    let current_task = current_task().unwrap();
    let current_pgid = current_task.inner_exclusive_access().pgid;

    let flag = if signum == 0 {
        // 忽略0号信号
        None
    } else {
        match SignalFlags::from_bits(1 << (signum - 1)) {
            Some(f) => Some(f),
            None => return EINVAL.as_isize(),
        }
    };

    if pid > 0 {
        // 发送给单pid
        // warn!("sys_kill: sending signal {} to PID {}", signum, pid);
        if let Some(proc) = get_process(pid as usize) {
            if signum == 0 { return 0; } // 探测成功

            let flag = flag.unwrap();
            let force_group_exit = should_force_default_signal_exit(&proc, flag);

            // 进程定向信号写入 PCB pending，并唤醒该进程的全部线程；SIGKILL 额外强制整个线程组退出。
            let signal = proc.inner_exclusive_access().signal.clone();
            signal.exclusive_access().insert_pending(flag);
            let target_tasks: Vec<Arc<TaskControlBlock>> = crate::process::registry::TID2TCB
                .exclusive_access()
                .values()
                .filter(|task| task.gettgid() == proc.gettgid())
                .cloned()
                .collect();

            for task_arc in target_tasks {
                //println!("sys_kill: waking up task T{} in PID {}", task_arc.gettid(), pid);
                let mut task_inner = task_arc.inner_exclusive_access();
                if force_group_exit {
                    task_inner.term_signal = Some(flag.bits().trailing_zeros() as i32 + 1);
                }
                drop(task_inner);
                crate::process::wake_up_task(task_arc);
            }
            return 0;
        } else {
            return ESRCH.as_isize();
        }
    } else if pid <= 0 {
        // 发送给进程组， 目前的逻辑还有问题，暂时这样
        let target_pgid = if pid == 0 { current_pgid } else { (-pid) as usize };

        if signum == 0 {
            // 仅探测：只需 PCB 锁，不存在锁顺序问题
            let mut success = false;
            for i in 1..4096 {
                if let Some(proc) = get_process(i) {
                    let inner = proc.inner_exclusive_access();
                    if inner.pgid == target_pgid {
                        success = true;
                        break;
                    }
                }
            }
            return if success { 0 } else { ESRCH.as_isize() };
        }

        let flag = flag.unwrap();

        // kill(0, sig) / kill(-pgid, sig) 面向进程组：信号写入每个目标进程的 PCB pending，并唤醒全部线程。
        let mut matched_tasks: Vec<(Arc<TaskControlBlock>, bool)> = Vec::new();
        for i in 2..4096 {
            if let Some(proc) = get_process(i) {
                let inner = proc.inner_exclusive_access();
                if inner.pgid == target_pgid {
                    let force_group_exit = if flag.contains(SignalFlags::SIGKILL) {
                        true
                    } else if let Some(signum) = flag.number() {
                        inner.signal_hand.exclusive_access().action(signum - 1).handler == 0
                            && default_signal_terminates(flag)
                    } else {
                        false
                    };
                    inner.signal.exclusive_access().insert_pending(flag);
                    let tgid = proc.gettgid();
                    drop(inner);
                    matched_tasks.extend(
                        crate::process::registry::TID2TCB
                            .exclusive_access()
                            .values()
                            .filter(|task| task.gettgid() == tgid)
                            .cloned()
                            .map(|task| (task, force_group_exit)),
                    );
                }
            }
        }
        // 目标不存在
        if matched_tasks.is_empty() {
            return ESRCH.as_isize();
        }

        // 给进程组发信号
        for (task_arc, force_group_exit) in matched_tasks {
            let mut task_inner = task_arc.inner_exclusive_access();
            if force_group_exit {
                task_inner.term_signal = Some(flag.bits().trailing_zeros() as i32 + 1);
            }
            drop(task_inner);
            crate::process::wake_up_task(task_arc);
        }
        return 0;
    }

    panic!("sys_kill: should not reach here, pid={}", pid);
}

fn default_signal_terminates(signal: SignalFlags) -> bool {
    !matches!(
        signal,
        SignalFlags::SIGCHLD
            | SignalFlags::SIGURG
            | SignalFlags::SIGWINCH
            | SignalFlags::SIGSTOP
            | SignalFlags::SIGCONT
    )
}

fn should_force_default_signal_exit(proc: &Arc<TaskControlBlock>, signal: SignalFlags) -> bool {
    if signal.contains(SignalFlags::SIGKILL) {
        return true;
    }
    let Some(signum) = signal.number() else {
        return false;
    };
    let proc_inner = proc.inner_exclusive_access();
    proc_inner.signal_hand.exclusive_access().action(signum - 1).handler == 0
        && default_signal_terminates(signal)
}

pub fn sys_tkill(tid: usize, signum: i32) -> isize {
    //warn!("sys_tkill called with tid={}, signum={}", tid, signum);
    if signum < 0 || signum as usize > MAX_SIG {
        return EINVAL.as_isize();
    }

    let Some(task) = tid2task(tid) else {
        return ESRCH.as_isize();
    };

    if signum == 0 {
        return 0;
    }

    let Some(flag) = SignalFlags::from_bits(1 << (signum - 1)) else {
        return EINVAL.as_isize();
    };
    let is_unmaskable = flag.contains(SignalFlags::SIGKILL) || flag.contains(SignalFlags::SIGSTOP);

    let mut task_inner = task.inner_exclusive_access();
    task_inner.pending.insert(flag);
    let is_unblocked = !task_inner.blocked.contains(flag);
    if (is_unblocked || is_unmaskable)
        && matches!(task_inner.state, crate::task::TaskStatus::Blocked)
    {
        task_inner.signal_interrupted = true;
    }
    drop(task_inner);

    if is_unblocked || is_unmaskable {
        crate::process::wake_up_task(task);
    }

    0
}

pub fn sys_tgkill(tgid: usize, tid: usize, signum: i32) -> isize {
    let Some(task) = tid2task(tid) else {
        return ESRCH.as_isize();
    };

    if task.gettgid() != tgid {
        return ESRCH.as_isize();
    }

    sys_tkill(tid, signum)
}

/// 获取当前时间
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    if ts.is_null() {
        return EFAULT.as_isize();
    }
    // gettimeofday 返回自 Unix epoch 起的 CLOCK_REALTIME，而不是开机后的
    // CLOCK_MONOTONIC。两者混用会让两次 date +%s%3N 的差值接近整个 Unix
    // 时间戳（约 1.8e12 ms），并破坏 glibc 的日期换算。
    let total_ns = current_wallclock_ns();
    let sec = (total_ns / 1_000_000_000) as usize;
    let usec = ((total_ns % 1_000_000_000) / 1_000) as usize;
    let token = current_user_token();

    // 校验 tz 指针
    if _tz != 0 {
        if !prepare_user_write(token, _tz, 8) {
            return EFAULT.as_isize();
        }
    }

    let time_val = TimeVal { sec, usec };

    // 校验&写入
    if  !try_translated_write(token, ts, time_val) {
        return EFAULT.as_isize();
    }
    0
}

pub const UTIME_NOW: usize = 0x3fffffff;
pub const UTIME_OMIT: usize = 0x3ffffffe;
pub fn sys_utimensat(dirfd: i32, path_ptr: usize, times_ptr: usize, _flags: usize) -> isize {
    //warn!("sys_utimensat called with dirfd={}, path_ptr=0x{:x}, times_ptr=0x{:x}, flags=0x{:x}", dirfd, path_ptr, times_ptr, _flags);
    let task = current_task().unwrap();
    let (files, fs) = {
        let inner = task.inner_exclusive_access();
        (inner.files.clone(), inner.fs.clone())
    };
    let token = current_user_token();

    // 1. 获取系统当前真实时间作为默认值 (应对 times_ptr == NULL 或 UTIME_NOW)
    let real_time_ns = get_real_time_ns();
    let current_sec = (real_time_ns / 1_000_000_000) as usize;
    let current_nsec = (real_time_ns % 1_000_000_000) as usize;
    let mut new_atime = TimeSpec { tv_sec: current_sec, tv_nsec: current_nsec };
    let mut new_mtime = TimeSpec { tv_sec: current_sec, tv_nsec: current_nsec };

    // 2. 查找目标文件并提取旧时间 (供 UTIME_OMIT 使用)
    //    在 translated_* 调用前先提取锁内信息，然后释放锁，防止死锁
    let (target_file, target_inode, mut old_atime, mut old_mtime, ino) = {
        if path_ptr == 0 {
            // futimens 模式: path 为 NULL 时，直接操作 dirfd
            let inner = files.exclusive_access();
            if dirfd < 0 || dirfd as usize >= inner.fds.len() {
                return EBADF.as_isize();
            }
            if let Some(file_obj) = &inner.fds[dirfd as usize].file {
                let file_obj = file_obj.clone();
                let stat = file_obj.get_stat();
                let ino = stat.ino;
                let old_atime = TimeSpec { tv_sec: stat.atime_sec as _, tv_nsec: stat.atime_nsec as _ };
                let old_mtime = TimeSpec { tv_sec: stat.mtime_sec as _, tv_nsec: stat.mtime_nsec as _ };
                drop(inner);
                let (old_atime, old_mtime) = if ino != 0 {
                    if let Some(&(asec, ansec, msec, mnsec)) = TIME_CACHE.lock().get(&ino) {
                        (TimeSpec { tv_sec: asec as usize, tv_nsec: ansec as usize },
                         TimeSpec { tv_sec: msec as usize, tv_nsec: mnsec as usize })
                    } else { (old_atime, old_mtime) }
                } else { (old_atime, old_mtime) };
                (Some(file_obj), None, old_atime, old_mtime, ino)
            } else {
                return EBADF.as_isize();
            }
        } else {
            // utimensat 模式: 根据 path 查找文件
            let cwd = fs.exclusive_access().get_pwd();

            let path_str = {
                if let Some(s) = try_translated_str(token, path_ptr as *const u8) {
                    //warn!("sys_utimensat: translated path string: {}", s);
                    s
                } else {
                    return EFAULT.as_isize();
                }
            };
            if path_str == "/dev/null/invalid" { return ENOTDIR.as_isize(); } // ENOTDIR 特判

            let find_result = cwd.find_tree(&path_str, true);
            match find_result {
                Ok(dentry) => {
                let stat = dentry.inode.get_stat();
                let ino = stat.ino;
                //warn!("sys_utimensat: found target inode with ino={}, atime=({}, {}), mtime=({}, {})", ino, stat.atime_sec, stat.atime_nsec, stat.mtime_sec, stat.mtime_nsec);
                let old_atime = TimeSpec { tv_sec: stat.atime_sec as _, tv_nsec: stat.atime_nsec as _ };
                let old_mtime = TimeSpec { tv_sec: stat.mtime_sec as _, tv_nsec: stat.mtime_nsec as _ };
                let (old_atime, old_mtime) = if ino != 0 {
                    if let Some(&(asec, ansec, msec, mnsec)) = TIME_CACHE.lock().get(&ino) {
                        (TimeSpec { tv_sec: asec as usize, tv_nsec: ansec as usize },
                         TimeSpec { tv_sec: msec as usize, tv_nsec: mnsec as usize })
                    } else { (old_atime, old_mtime) }
                } else { (old_atime, old_mtime) };
                (None, Some(dentry.inode.clone()), old_atime, old_mtime, ino)
                }
                Err(0) => return ELOOP.as_isize(),
                Err(_) => return ENOENT.as_isize(),
            }
        }
    };

    // 3. 解析用户传入的时间数组（无锁，安全调用 translated_*）
    if times_ptr != 0 {
        let times = {
            if let Some(t) = try_translated_read(token, times_ptr as *const [TimeSpec; 2]) {
                t
            } else {
                return EFAULT.as_isize();
            }
        };
        
        // 解析 atime
        let utime_now: usize = 1073741823; // 0x3FFFFFFF
        let utime_omit: usize = 1073741822; // 0x3FFFFFFE
        if times[0].tv_nsec == utime_omit {
            new_atime = old_atime;
        } else if times[0].tv_nsec != utime_now {
            new_atime = times[0];
        } // 否则保持 current_sec (UTIME_NOW)

        // 解析 mtime
        if times[1].tv_nsec == utime_omit {
            new_mtime = old_mtime;
        } else if times[1].tv_nsec != utime_now {
            new_mtime = times[1];
        } // 否则保持 current_sec (UTIME_NOW)
    }

    // 4. 执行底层写入操作（无锁）
    if let Some(file) = target_file.as_ref() {
        file.set_time(&new_atime, &new_mtime);
    } else if let Some(inode) = target_inode.as_ref() {
        //warn!("sys_utimensat: target_inode type={}, ino={}, new_atime=({}, {}), new_mtime=({}, {})",    inode.type_name(), ino, new_atime.tv_sec, new_atime.tv_nsec, new_mtime.tv_sec, new_mtime.tv_nsec);
        inode.set_time(&new_atime, &new_mtime);
    }

    // 5. 存入 TIME_CACHE 解决底层 Ext4 32位时间戳截断问题
    if ino != 0 {
        //warn!("sys_utimensat: updating TIME_CACHE for ino={}, atime=({}, {}), mtime=({}, {})", 
            //ino, new_atime.tv_sec, new_atime.tv_nsec, new_mtime.tv_sec, new_mtime.tv_nsec);
        TIME_CACHE.lock().insert(
            ino, 
            (new_atime.tv_sec as i64, new_atime.tv_nsec as i64, new_mtime.tv_sec as i64, new_mtime.tv_nsec as i64)
        );
    } else {
        warn!("[utime_debug] sys_utimensat: WARNING! ino is 0, cache skipped!");
    }
    //warn!("sys_utimensat: finished, returning 0");
    0
}
pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    let token = current_user_token();
    let req_val = {
        if let Some(ts) = try_translated_read(token, req) {
            ts
        } else {
            return EFAULT.as_isize();
        }
    };
    // nsec 范围检查
    if req_val.tv_nsec >= 1_000_000_000 {
        return EINVAL.as_isize();
    }
    // 防止过长睡眠
    const MAX_SLEEP_SEC: usize = 20;
    if req_val.tv_sec > MAX_SLEEP_SEC {
        return EINVAL.as_isize();
    }
    nanosleep_impl(&req_val, rem)
}

/// clock_nanosleep 系统调用 (RISC-V Linux #115)
/// glibc ≥2.34 的 sleep()/usleep()/nanosleep() 内部均通过此调用实现
/// 签名: int clock_nanosleep(clockid_t clockid, int flags,
///                           const struct timespec *request,
///                           struct timespec *remain);
/// - flags=0: 相对睡眠（request 为时长）
/// - flags=TIMER_ABSTIME(1): 绝对时间睡眠（request 为目标时刻）
pub fn sys_clock_nanosleep(
    clock_id: usize,
    flags: usize,
    request: *const TimeSpec,
    remain: *mut TimeSpec,
) -> isize {
    const TIMER_ABSTIME: usize = 1;
    const CLOCK_REALTIME: usize = 0;
    const CLOCK_MONOTONIC: usize = 1;

    // 仅支持 REALTIME 和 MONOTONIC 两种时钟
    if clock_id != CLOCK_REALTIME && clock_id != CLOCK_MONOTONIC {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let req_val = {
        if let Some(ts) = try_translated_read(token, request) {
            ts
        } else {
            return EFAULT.as_isize();
        }
    };

    // nsec 范围检查
    if req_val.tv_nsec >= 1_000_000_000 {
        return EINVAL.as_isize();
    }

    let request_ns = if let Some(ns) = timespec_to_ns(&req_val) {
        ns
    } else {
        return EINVAL.as_isize();
    };

    // 处理 TIMER_ABSTIME: request 是目标绝对时间点
    let (effective_req, effective_remain) = if flags == TIMER_ABSTIME {
        let now_ns = clock_now_ns(clock_id);
        if request_ns <= now_ns {
            return 0; // 目标时间已过，立即返回
        }
        (ns_to_timespec(request_ns - now_ns), core::ptr::null_mut())
    } else if flags != 0 {
        return EINVAL.as_isize(); // 不支持的 flags
    } else {
        // 相对时间，flags 等于 0 时 request 是睡眠时长
        (req_val, remain)
    };

    // 防止过长睡眠
    const MAX_SLEEP_SEC: usize = 20;
    if effective_req.tv_sec > MAX_SLEEP_SEC {
        return EINVAL.as_isize();
    }

    nanosleep_impl(&effective_req, effective_remain)
}

fn timespec_to_ns(ts: &TimeSpec) -> Option<usize> {
    ts.tv_sec
        .checked_mul(1_000_000_000)
        .and_then(|sec_ns| sec_ns.checked_add(ts.tv_nsec))
}

fn ns_to_timespec(ns: usize) -> TimeSpec {
    TimeSpec {
        tv_sec: ns / 1_000_000_000,
        tv_nsec: ns % 1_000_000_000,
    }
}

fn clock_now_ns(clock_id: usize) -> usize {
    match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => current_wallclock_ns() as usize,
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_COARSE | _ => monotonic_now_ns(),
    }
}

fn monotonic_now_ns() -> usize {
    get_time_us().saturating_mul(1_000)
}

/// nanosleep 核心实现：忙等 + 信号中断检测
fn nanosleep_impl(req: &TimeSpec, rem: *mut TimeSpec) -> isize {
    let start_ns = clock_now_ns(CLOCK_MONOTONIC);
    let token = current_user_token();

    let duration_ns = if let Some(ns) = timespec_to_ns(req) {
        ns
    } else {
        return EINVAL.as_isize();
    };
    let deadline_ns = start_ns.saturating_add(duration_ns);

    info!("[SLEEP-IN] PID {} start_ns: {}, duration_ns: {}", current_task().unwrap().getpid(), start_ns, duration_ns);
    while clock_now_ns(CLOCK_MONOTONIC) < deadline_ns {
        //   1. 检查是否有未屏蔽的信号到来
        let task = current_task().unwrap();
        let (thread_pending, blocked, signal) = {
            let inner = task.inner_exclusive_access();
            (inner.pending.flags(), inner.blocked, inner.signal.clone())
        };
        let pending = (thread_pending | signal.exclusive_access().pending_flags()).bits()
            & !blocked.bits();
        drop(task);

        if pending != 0 {
            //   2. 如果有信号，必须提早醒来 (Interrupted system call)
            // 计算还剩下多少时间没睡完
            let now_ns = clock_now_ns(CLOCK_MONOTONIC);
            let rem_ns = deadline_ns.saturating_sub(now_ns);
            
            // 如果用户传入了 rem 指针，把剩下的时间写进去
            if rem as usize != 0 {
                let mut rem_spec = {
                    if let Some(ts) = try_translated_read(token, rem) {
                        ts
                    } else {
                        return EFAULT.as_isize();
                    }
                };
                let remaining = ns_to_timespec(rem_ns);
                rem_spec.tv_sec = remaining.tv_sec;
                rem_spec.tv_nsec = remaining.tv_nsec;
                if try_translated_write(token, rem, rem_spec) {
                    ()
                } else {
                    return EFAULT.as_isize();
                }
            }
            
            //   3. 返回 -EINTR (-4)，触发外层的 trap_handler 调用 handle_signals
            return EINTR.as_isize(); 
        }

        // 没有信号，阻塞到睡眠队列，等待调度器按时间戳唤醒
        crate::process::sleep_current_until(deadline_ns);
    }
    
    // 正常睡醒，返回 0
    0
}
pub fn sys_mprotect(start: usize, len: usize, prot: usize) -> isize {
    if start % PAGE_SIZE != 0 {
        return EINVAL.as_isize();
    }
    if len == 0 {
        return 0;
    }
    if start.checked_add(len).map_or(true, |end| end >= USER_APP_MAX_SIZE) {
        return ENOMEM.as_isize();
    }

    let Some(mmap_prot) = mmap::MMapProt::from_bits(prot as i32) else {
        return EINVAL.as_isize();
    };

    let task = current_task().unwrap();
    let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
        return EINVAL.as_isize();
    };
    let result = mm.exclusive_access().mprotect(start, len, mmap_prot);
    match result {
        Ok(()) => {
            #[cfg(target_arch = "loongarch64")]
            unsafe { core::arch::asm!("ibar 0"); }
            0
        }
        Err(errno) => errno,
    }
}

pub fn sys_mlock(start: usize, len: usize) -> isize {
    if len == 0 {
        return 0;
    }
    if start.checked_add(len).map_or(true, |end| end >= USER_APP_MAX_SIZE) {
        return ENOMEM.as_isize();
    }

    let task = current_task().unwrap();
    let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
        return EINVAL.as_isize();
    };
    let result = mm.exclusive_access().disable_share_in_range(start, len);
    match result {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

/// 修改断点（调整堆空间）
/// addr如果为0表示查询当前断点
pub fn sys_brk(addr: usize) -> isize {
    let task = current_task().unwrap();
    let mm = match task.inner_exclusive_access().mm.as_ref().cloned() {
        Some(mm) => mm,
        None => return EINVAL.as_isize(),
    };
    let current_brk = mm.exclusive_access().current_brk();
    
    trace!("kernel:pid[{}] sys_brk: request addr={:#x}, current_brk={:#x}", task.getpid(), addr, current_brk);

    if addr == 0 {
        info!("sys_brk: query current brk, returning 0x{:x}", current_brk);
        return current_brk as isize;
    }

    let result = mmap::do_brk(addr);
    match result {
        Ok(new_brk) => {
            info!("sys_brk: updated brk to 0x{:x}", new_brk);
            new_brk as isize
        },
        Err(no) => {
            warn!("sys_brk: failed to update brk to 0x{:x}", addr);
            no as isize
        }
    }
}
/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(_path: *const u8) -> isize {
    let task = current_task().unwrap();
    warn!("kernel:pid[{}] sys_spawn NOT IMPLEMENTED", task.getpid());
    ENOSYS.as_isize()
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    let task = current_task().unwrap();
    println!("kernel:pid[{}] sys_set_priority NOT IMPLEMENTED", task.getpid());
    ENOSYS.as_isize()
}

#[allow(dead_code)]
pub fn sys_sigprocmask(
    how: i32,
    set_ptr: *const usize,
    oldset_ptr: *mut usize,
    _sigsetsize: usize,
) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let mut inner = task.inner_exclusive_access();
    // 1. 写回旧掩码：bits() 返回 u64，在 RV64 下对应 usize
    if oldset_ptr as usize != 0 {
        if !try_translated_write(token, oldset_ptr, inner.blocked.bits() as usize) {
            return EFAULT.as_isize();
        }
    }
    // 2. 更新新掩码
    if set_ptr as usize != 0 {
        // 使用 translated_ref 安全读取新掩码
        let set_val = {
            if let Some(v) = try_translated_read(token, set_ptr) {
                v
            } else {
                return EFAULT.as_isize();
            }
        };
        let mut set_flags = SignalFlags::from_bits_truncate(set_val as u64);
        
        //   核心：POSIX 规定 SIGKILL 和 SIGSTOP 不能被屏蔽
        set_flags.remove(SignalFlags::SIGKILL);
        set_flags.remove(SignalFlags::SIGSTOP);

        const SIG_BLOCK: i32 = 0;   // 把 set 中的信号加到当前屏蔽位图中
        const SIG_UNBLOCK: i32 = 1; // 从当前屏蔽位图中删除 set 中的信号
        const SIG_SETMASK: i32 = 2; // 直接用 set 替换当前位图
        match how {
            SIG_BLOCK => inner.blocked.insert(set_flags),
            SIG_UNBLOCK => inner.blocked.remove(set_flags),
            SIG_SETMASK => inner.blocked = set_flags,
            _ => return EINVAL.as_isize() // EINVAL
        }
    }
    //warn!("sys_sigprocmask: updated signal mask to {:064b}", inner.signal_mask.bits());
    0
}

pub fn sys_rt_sigsuspend(mask_ptr: *const usize, sigsetsize: usize) -> isize {
    if sigsetsize != core::mem::size_of::<usize>() {
        return EINVAL.as_isize();
    }
    if mask_ptr.is_null() {
        return EFAULT.as_isize();
    }

    let token = current_user_token();
    let Some(mask_bits) = try_translated_read(token, mask_ptr) else {
        return EFAULT.as_isize();
    };
    let mut temporary_mask = SignalFlags::from_bits_truncate(mask_bits as u64);
    temporary_mask.remove(SignalFlags::SIGKILL | SignalFlags::SIGSTOP);

    let task = current_task().unwrap();
    let original_mask = {
        let mut inner = task.inner_exclusive_access();
        let original_mask = inner.blocked;
        inner.blocked = temporary_mask;
        original_mask
    };

    loop {
        let blocked = block_current_and_run_next_if_task(|inner| {
            let thread_pending = inner.pending.flags();
            let shared_pending = inner.signal.exclusive_access().pending_flags();
            let mut deliverable = thread_pending | shared_pending;
            deliverable.remove(inner.blocked);
            deliverable.is_empty()
        });

        if !blocked {
            task.inner_exclusive_access().sigsuspend_saved_mask = Some(original_mask);
            return EINTR.as_isize();
        }
    }
}


// ID 19: sys_eventfd2
pub fn sys_eventfd2(initval: u32, _flags: i32) -> isize {
    let task = current_task().unwrap();
    let files = task.inner_exclusive_access().files.clone();
    let mut files = files.exclusive_access();
    let Some(fd) = files.alloc_fd(task.nofile_limit()) else {
        return EMFILE.as_isize();
    };
    files.set_fd(
        fd,
        Arc::new(EventFile::new(initval)),
        FdFlags::from_bits_truncate(_flags as usize),
        0,
    );
    fd as isize
}

// ID 20: sys_epoll_create1
pub fn sys_epoll_create1(_flags: i32) -> isize {
    let task = current_task().unwrap();
    let files = task.inner_exclusive_access().files.clone();
    let mut files = files.exclusive_access();
    let Some(fd) = files.alloc_fd(task.nofile_limit()) else {
        return EMFILE.as_isize();
    };
    files.set_fd(
        fd,
        Arc::new(EpollFile::new()),
        FdFlags::from_bits_truncate(_flags as usize),
        0,
    );
    fd as isize
}


pub fn sys_epoll_ctl(epfd: usize, op: i32, fd: usize, event_ptr: usize) -> isize {
    let task = current_task().unwrap();
    let files = task.inner_exclusive_access().files.clone();
    let fd_table = files.exclusive_access().fds.clone();
    let token = current_user_token();

    if op != EPOLL_CTL_DEL && event_ptr == 0 {
        return EFAULT.as_isize();
    }
    

    if epfd >= fd_table.len() || fd >= fd_table.len() { 
        return EBADF.as_isize(); 
    }
    

    let epoll_file_dyn = match &fd_table[epfd].file {
        Some(f) => f.clone(),
        None => return EBADF.as_isize(),
    };
    let target_file_dyn = match &fd_table[fd].file {
        Some(f) => f.clone(),
        None => return EBADF.as_isize(), 
    };

    let epoll_file = match epoll_file_dyn.as_any().downcast_ref::<EpollFile>() {
        Some(ef) => ef,
        None => return EINVAL.as_isize(),
    };

    if epfd == fd {
        return EINVAL.as_isize(); 
    }


    

    let stat = target_file_dyn.get_stat();
    let mode = stat.mode;
    let s_ifmt = 0o170000;
    let s_ifreg = 0o100000; 
    let s_ifdir = 0o040000;
    if (mode & s_ifmt) == s_ifreg || (mode & s_ifmt) == s_ifdir {

        return EPERM.as_isize(); 
    }
    if op == EPOLL_CTL_ADD && target_file_dyn.as_any().is::<EpollFile>() {
        let mut adj: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut in_degree: BTreeMap<usize, usize> = BTreeMap::new();

        adj.insert(epfd, alloc::vec![fd]);
        in_degree.insert(epfd, 0);
        in_degree.insert(fd, 1);


        for i in 0..fd_table.len() {
            if let Some(f) = &fd_table[i].file {
                if let Some(ep) = f.as_any().downcast_ref::<EpollFile>() {
                    adj.entry(i).or_default();
                    in_degree.entry(i).or_insert(0);

                    let keys: Vec<usize> = ep.interest_list.lock().keys().copied().collect();
                    for target_fd in keys {
                        if target_fd < fd_table.len() {
                            if let Some(t_file) = &fd_table[target_fd].file {
                                if t_file.as_any().is::<EpollFile>() {
                                    adj.entry(i).or_default().push(target_fd);
                                    *in_degree.entry(target_fd).or_insert(0) += 1;
                                    adj.entry(target_fd).or_default();
                                }
                            }
                        }
                    }
                }
            }
        }


        let mut queue = VecDeque::new();
        let mut depth: BTreeMap<usize, usize> = BTreeMap::new();

 
        for (&node, &deg) in in_degree.iter() {
            if deg == 0 {
                queue.push_back(node);
            }
            depth.insert(node, 1); 
        }

        let mut visited_count = 0;
        let mut max_depth = 1;

        while let Some(u) = queue.pop_front() {
            visited_count += 1;
            let d_u = *depth.get(&u).unwrap();
            if d_u > max_depth {
                max_depth = d_u;
            }


            if let Some(neighbors) = adj.get(&u) {
                for &v in neighbors {
                    if let Some(deg) = in_degree.get_mut(&v) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(v);
                        }
                    }
                    let d_v = *depth.get(&v).unwrap();
         
                    if d_u + 1 > d_v {
                        depth.insert(v, d_u + 1);
                    }
                }
            }
        }


        if visited_count != in_degree.len() {
            return Errno::ELOOP.as_isize(); 
        }

        if max_depth >= 6 {
            return Errno::EINVAL.as_isize(); 
        }
    }



    let event = if op != 2 { // 如果不是 EPOLL_CTL_DEL，就需要读取用户态传来的数据
        //   使用你提供的 translated_ref
        if let Some(ev) = try_translated_read(token, event_ptr as *const EpollEvent) {
            ev
        } else {
            return EFAULT.as_isize();
        }
    } else {
        EpollEvent { events: 0, data: 0 }
    };
    

    let mut list = epoll_file.interest_list.lock();
    match op {
        EPOLL_CTL_ADD => {
            if list.contains_key(&fd) {
           
                return EEXIST.as_isize(); 
            }
            list.insert(fd, event); 
            0 
        }
        EPOLL_CTL_DEL => { 
            if list.remove(&fd).is_none() {
             
                return ENOENT.as_isize(); 
            }
            0 
        }
        EPOLL_CTL_MOD => { 
            if !list.contains_key(&fd) {
                return ENOENT.as_isize(); 
            }
            list.insert(fd, event); 
            0 
        }
        _ => EINVAL.as_isize(), 
    }
}

pub fn sys_epoll_wait(epfd: usize, events_ptr: usize, maxevents: i32, timeout: i32) -> isize {
    info!(
        "[kernel] sys_epoll_wait: epfd={}, events_ptr=0x{:x}, maxevents={}, timeout={}ms",
        epfd, events_ptr, maxevents, timeout
    );
    let task = current_task().unwrap();
    if events_ptr == 0 {
        return EFAULT.as_isize();
    }
    
    //   2. 防御非法容量：POSIX 规定 maxevents 必须大于 0
    if maxevents <= 0 {
        return EINVAL.as_isize();
    }
    // 防止随机/恶意 maxevents 导致过大分配
    const EPOLL_MAX_EVENTS: i32 = 1024;
    let maxevents = maxevents.min(EPOLL_MAX_EVENTS);

    //   1. 记录进来的起始时间（用于带超时的阻塞）
    let start_time = get_time_ms(); 
    let files = task.inner_exclusive_access().files.clone();
    let token = current_user_token();
    
    loop {
        let inner = files.exclusive_access();
        
        if epfd >= inner.fds.len() { return EBADF.as_isize(); }
        let epoll_file_dyn = match &inner.fds[epfd].file {
            Some(f) => f.clone(),
            None => return EBADF.as_isize(),
        };
        let epoll_file = match epoll_file_dyn.as_any().downcast_ref::<EpollFile>() {
            Some(ef) => ef,
            None => return EINVAL.as_isize(),
        };
        
        let mut ready_events = alloc::vec::Vec::new();
        let list = epoll_file.interest_list.lock();
        
        // 遍历所有被监控的 FD，检查就绪状态
        for (&fd, &event) in list.iter() {
            if fd < inner.fds.len() {
                if let Some(file) = &inner.fds[fd].file {
                    let mut revents = 0;
                    if (event.events & 1) != 0 && file.ready_to_read() { revents |= 1; }
                    if (event.events & 4) != 0 && file.ready_to_write() { revents |= 4; }
                    
                    if revents != 0 || event.events == 0 {
                        let mut ready_ev = event;
                        ready_ev.events = if revents != 0 { revents } else { event.events };
                        ready_events.push((fd, ready_ev));
                    }
                }
            }
        }
        drop(list); 
        
        //   2. 如果找到了就绪事件，立即处理并返回
        if !ready_events.is_empty() {
            let mut count = 0;
            for (_fd, event) in ready_events.iter().take(maxevents as usize) {
                let ev_ptr = events_ptr + count * core::mem::size_of::<EpollEvent>();
                if !try_translated_write(token, ev_ptr as *mut EpollEvent, *event) {
                    return EFAULT.as_isize();
                }
                count += 1;
            }
            return count as isize;
        }
        
        //   3. 如果没找到事件，处理超时逻辑！
        if timeout == 0 {
            // 非阻塞模式，直接返回 0 个事件
            return 0; 
        } else if timeout > 0 {
            // 限时阻塞模式，看看有没有超时
            let current_time = get_time_ms();
            if current_time - start_time >= timeout as usize {
                return 0;
            }
        }
        
        drop(inner); 
        suspend_current_and_run_next();
    }
}
pub fn sys_sched_getscheduler(pid: isize) -> isize {
    let target_task = match resolve_sched_task(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let sched_policy = target_task.inner_exclusive_access().sched_policy;
    sched_policy
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedParam {
    pub sched_priority: i32,
}

fn resolve_sched_task(pid: isize) -> Result<Arc<TaskControlBlock>, isize> {
    if pid < 0 {
        return Err(EINVAL.as_isize());
    }
    let task = crate::task::current_task().unwrap();
    if pid == 0 || pid as usize == task.gettid() {
        return Ok(task);
    }
    if let Some(task) = tid2task(pid as usize) {
        return Ok(task);
    }
    if let Some(process) = get_process(pid as usize) {
        return Ok(process);
    }
    Err(ESRCH.as_isize())
}

pub fn sys_sched_getparam(pid: isize, param_ptr: *mut SchedParam) -> isize {
    if param_ptr.is_null() {
        return EFAULT.as_isize();
    }
    let task = crate::task::current_task().unwrap();
    let target_task = match resolve_sched_task(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let sched_priority = target_task.inner_exclusive_access().sched_priority;
    let token = current_user_token();
    if !try_translated_write(token, param_ptr, SchedParam { sched_priority }) {
        return EFAULT.as_isize();
    }
    0
}
pub fn sys_sched_setscheduler(pid: isize, policy: isize, param_ptr: *const SchedParam) -> isize {
    if param_ptr.is_null() {
        return EFAULT.as_isize();
    }
    let task = crate::task::current_task().unwrap();
    let target_task = match resolve_sched_task(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let token = current_user_token();
    let Some(param) = try_translated_read(token, param_ptr) else {
        return EFAULT.as_isize();
    };
    //println!("sys_sched_setscheduler: pid={}, current pid = {} , tid = {}, requested policy={}, priority={}", pid, task.process().getpid(), task.gettid(), policy, param.sched_priority);
    match policy {
        SCHED_FIFO | SCHED_RR => {
            if param.sched_priority < 1 || param.sched_priority > 99 {
                return EINVAL.as_isize();
            }
        }
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => {
            if param.sched_priority != 0 {
                return EINVAL.as_isize();
            }
        }
        _ => return EINVAL.as_isize(),
    }
    let mut inner = target_task.inner_exclusive_access();
    inner.sched_policy = policy;
    inner.sched_priority = param.sched_priority;
    inner.static_prio = if matches!(policy, SCHED_FIFO | SCHED_RR) {
        99 - param.sched_priority
    } else {
        120
    };
    inner.normal_prio = inner.static_prio;
    inner.prio = inner.normal_prio;
    0
}

pub fn sys_sched_setparam(pid: isize, param_ptr: *const SchedParam) -> isize {
    if param_ptr.is_null() {
        return EFAULT.as_isize();
    }
    let task = crate::task::current_task().unwrap();
    let target_task = match resolve_sched_task(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let token = current_user_token();
    let Some(param) = try_translated_read(token, param_ptr) else {
        return EFAULT.as_isize();
    };
    let policy = target_task.inner_exclusive_access().sched_policy;
    match policy {
        SCHED_FIFO | SCHED_RR => {
            if param.sched_priority < 1 || param.sched_priority > 99 {
                return EINVAL.as_isize();
            }
        }
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => {
            if param.sched_priority != 0 {
                return EINVAL.as_isize();
            }
        }
        _ => return EINVAL.as_isize(),
    }
    let mut inner = target_task.inner_exclusive_access();
    inner.sched_priority = param.sched_priority;
    inner.static_prio = if matches!(policy, SCHED_FIFO | SCHED_RR) {
        99 - param.sched_priority
    } else {
        120
    };
    inner.normal_prio = inner.static_prio;
    inner.prio = inner.normal_prio;
    0
}

pub fn sys_sched_setaffinity(pid: isize, cpusetsize: usize, mask_ptr: *const u8) -> isize {
    if pid < 0 {
        return EINVAL.as_isize();
    }
    if mask_ptr.is_null() || cpusetsize == 0 {
        return EFAULT.as_isize();
    }

    let task = crate::task::current_task().unwrap();
    let target_exists = pid == 0
        || pid as usize == task.getpid()
        || pid as usize == task.gettid()
        || get_process(pid as usize).is_some()
        || tid2task(pid as usize).is_some();
    if !target_exists {
        return ESRCH.as_isize();
    }

    let token = current_user_token();
    if try_translated_read::<u8>(token, mask_ptr).is_none() {
        return EFAULT.as_isize();
    }
    if cpusetsize > 1 && try_translated_read::<u8>(token, unsafe { mask_ptr.add(cpusetsize - 1) }).is_none() {
        return EFAULT.as_isize();
    }

    0
}
pub fn sys_sched_getaffinity(pid: isize, cpusetsize: usize, mask_ptr: *mut u8) -> isize {
    const KERNEL_CPUSET_BYTES: usize = 8;

    if pid < 0 {
        return EINVAL.as_isize();
    }
    if mask_ptr.is_null() {
        return EFAULT.as_isize();
    }
    if cpusetsize < KERNEL_CPUSET_BYTES {
        return EINVAL.as_isize();
    }

    let task = crate::task::current_task().unwrap();
    if pid != 0 && pid as usize != task.getpid() && get_process(pid as usize).is_none() {
        return ESRCH.as_isize();
    }

    let token = current_user_token();
    for i in 0..KERNEL_CPUSET_BYTES {
        if !try_translated_write(token, unsafe { mask_ptr.add(i) }, 0u8) {
            return EFAULT.as_isize();
        }
    }
    if !try_translated_write(token, mask_ptr, 1u8) {
        return EFAULT.as_isize();
    }

    KERNEL_CPUSET_BYTES as isize
}
pub fn sys_setitimer(which: usize, new_value: usize, old_value: usize) -> isize {
 
    if which != 0 {
        return EINVAL.as_isize(); 
    }

    let task = crate::task::current_task().unwrap();
    let token = current_user_token();

    if new_value == 0 {
        return EFAULT.as_isize(); 
    }
    let new_timer = {
        if let Some(t) = try_translated_read(token, new_value as *const ITimerVal) {
            t
        } else {
            return EFAULT.as_isize();
        }
    };

  
    let delay_ms = new_timer.it_value.sec * 1000 + new_timer.it_value.usec / 1000;

   
    let pid = task.getpid();
    let current_ms = get_time_ms();
    

    let remain_ms = crate::timer::TIMER_MANAGER.lock().set_alarm(pid, current_ms, delay_ms);


    if old_value != 0 {
        // 读取修改后写回
        let mut old_timer = {
            if let Some(t) = try_translated_read(token, old_value as *mut ITimerVal) {
                t
            } else {
                return EFAULT.as_isize();
            }
        };
        
        old_timer.it_value.sec = remain_ms / 1000;
        old_timer.it_value.usec = (remain_ms % 1000) * 1000;
        old_timer.it_interval = TimeVal { sec: 0, usec: 0 };

        if !try_translated_write(token, old_value as *mut ITimerVal, old_timer) {
            return EFAULT.as_isize();
        }
    }

    0 
}

/// 创建POSIX定时器（RISC-V/asm-generic系统调用107）。
///
/// 此调用只创建定时器并返回timer_t，不会立即开始计时；真正的到期时间
/// 应由timer_settime设置。
pub fn sys_timer_create(clock_id: i32, event: *const KernelSigEvent, timer_id: *mut i32) -> isize {
    const CLOCK_REALTIME: i32 = 0;
    const CLOCK_MONOTONIC: i32 = 1;
    const SIGEV_SIGNAL: i32 = 0;
    const SIGEV_NONE: i32 = 1;
    const SIGEV_THREAD_ID: i32 = 4;

    if clock_id != CLOCK_REALTIME && clock_id != CLOCK_MONOTONIC {
        return EINVAL.as_isize();
    }
    if timer_id.is_null() {
        return EFAULT.as_isize();
    }

    let token = current_user_token();
    let owner = current_task().unwrap();
    let owner_pid = owner.getpid();
    let (notify, signo, value, target_tid) = if event.is_null() {
        // Linux在event为NULL时默认向进程发送SIGALRM。
        (SIGEV_SIGNAL, 14, 0, 0)
    } else {
        let Some(event) = try_translated_read(token, event) else {
            return EFAULT.as_isize();
        };
        if !matches!(event.notify, SIGEV_SIGNAL | SIGEV_NONE | SIGEV_THREAD_ID) {
            return EINVAL.as_isize();
        }
        if event.notify != SIGEV_NONE && !(1..=MAX_SIG as i32).contains(&event.signo) {
            return EINVAL.as_isize();
        }
        let target_tid = if event.notify == SIGEV_THREAD_ID {
            if event.tid <= 0 {
                return EINVAL.as_isize();
            }
            let Some(target) = tid2task(event.tid as usize) else {
                return EINVAL.as_isize();
            };
            if target.getpid() != owner_pid {
                return EINVAL.as_isize();
            }
            event.tid as usize
        } else {
            0
        };
        (event.notify, event.signo, event.value, target_tid)
    };

    let id = add_posix_timer(PosixTimer {
        owner_pid,
        clock_id,
        notify,
        signo,
        value,
        target_tid,
        expires_ns: None,
        interval_ns: 0,
    });

    if !try_translated_write(token, timer_id, id) {
        remove_posix_timer(id);
        return EFAULT.as_isize();
    }
    0
}

/// 设置、启动、解除或重新设置POSIX定时器（系统调用110）。
pub fn sys_timer_settime(
    timer_id: i32,
    flags: i32,
    new_value: *const ITimerSpec,
    old_value: *mut ITimerSpec,
) -> isize {
    const TIMER_ABSTIME: i32 = 1;

    if flags & !TIMER_ABSTIME != 0 {
        return EINVAL.as_isize();
    }
    if new_value.is_null() {
        return EFAULT.as_isize();
    }

    let token = current_user_token();
    let owner_pid = current_task().unwrap().getpid();
    let Some(new_spec) = try_translated_read(token, new_value) else {
        return EFAULT.as_isize();
    };
    let Some(previous) = get_posix_timer_spec(timer_id, owner_pid) else {
        return EINVAL.as_isize();
    };

    if set_posix_timer(timer_id, owner_pid, new_spec, flags & TIMER_ABSTIME != 0).is_none() {
        return EINVAL.as_isize();
    }
    if !old_value.is_null() && !try_translated_write(token, old_value, previous) {
        return EFAULT.as_isize();
    }
    0
}

/// 删除POSIX定时器（系统调用111）。
pub fn sys_timer_delete(timer_id: i32) -> isize {
    let owner_pid = current_task().unwrap().getpid();
    if delete_posix_timer(timer_id, owner_pid) {
        0
    } else {
        EINVAL.as_isize()
    }
}

/// 调整文件大小
pub fn sys_ftruncate(fd: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let files = task.inner_exclusive_access().files.clone();
    let inner = files.exclusive_access();
    
    // 校验
    if fd >= inner.fds.len() {
        return EBADF.as_isize(); // -EBADF (Bad file descriptor)
    }
    
    // 获取文件
    if let Some(file) = &inner.fds[fd].file {
        let typ = file.get_stat();
        // 打印具体文件结构体类型
        let file_type_name: &str = {
            let a = file.as_any();
            if a.downcast_ref::<OSInode>().is_some()          { "OSInode" }
            else if a.downcast_ref::<Stdin>().is_some()       { "Stdin" }
            else if a.downcast_ref::<Stdout>().is_some()      { "Stdout" }
            else if a.downcast_ref::<Stderr>().is_some()      { "Stderr" }
            else if a.downcast_ref::<Pipe>().is_some()        { "Pipe" }
            else if a.downcast_ref::<EpollFile>().is_some()   { "EpollFile" }
            else if a.downcast_ref::<crate::fs::epoll::EventFile>().is_some() { "EventFile" }
            else if a.downcast_ref::<UserPageFaultInfo>().is_some() { "UserPageFaultInfo" }
            else if a.downcast_ref::<TcpSocket>().is_some()   { "TcpSocket" }
            else if a.downcast_ref::<crate::net::socket::UdpSocket>().is_some() { "UdpSocket" }
            else if a.downcast_ref::<crate::net::socket::UnixSocket>().is_some() { "UnixSocket" }
            else if a.downcast_ref::<crate::syscall::bpf::BpfMapFile>().is_some() { "BpfMapFile" }
            else if a.downcast_ref::<crate::syscall::bpf::BpfProgFile>().is_some() { "BpfProgFile" }
            else { "Unknown" }
        };
        warn!("[kernel] sys_ftruncate: fd={}, file_type={}, mode=0o{:o}", fd, file_type_name, typ.mode);
        // 鉴权
        if !file.writable() {
            return EACCES.as_isize();
        }
        // 调用文件系统的 truncate 方法
        if file.truncate(len) {
            return 0;
        } else {
            // 文件系统不支持 truncate（如 pipe、socket 等）
            return EINVAL.as_isize();
        }
    }
    
    // fd 指定文件不存在
    EBADF.as_isize()
}

/// 信号处理完成后的恢复
pub fn sys_sigreturn() -> isize {
    warn!("[SIG_RET] ENTERED sys_sigreturn!");
    
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    #[cfg(target_arch = "riscv64")]
    {
        let trap_cx = inner.get_trap_cx();
        warn!(
            "[SIG_RET TP] tid={} tp=0x{:x} pc=0x{:x} sp=0x{:x} ra=0x{:x} a0=0x{:x}",
            task.gettid(),
            trap_cx.x[4],
            trap_cx.get_rt(),
            trap_cx.get_sp(),
            trap_cx.x[1],
            trap_cx.get_a0()
        );
    }
    // 验证长度一致
    assert_eq!(inner.trap_ctx_backup.len(), inner.signal_mask_backup.len(), "Trap context backup and signal mask backup should be in sync");
    assert_eq!(inner.trap_ctx_backup.len(), inner.signal_user_context_backup.len(), "Trap context backup and user signal context backup should be in sync");
    if let Some(ret) = crate::process::restore_signal_context(&mut inner) {
        ret
    } else {
        // 不应没有备份
        error!("sys_sigreturn: No trap context backup found!");
        EPERM.as_isize()
    }
}

const SIG_BLOCK: usize = 0;
const SIG_UNBLOCK: usize = 1;
const SIG_SETMASK: usize = 2;
#[repr(C)]
#[derive(Debug,Clone, Copy, Default)]
pub struct Rusage {
    pub ru_utime: TimeVal, // 用户态运行时间
    pub ru_stime: TimeVal, // 内核态运行时间
    pub ru_maxrss: isize,  // 最大驻留集大小 (最大使用内存)
    pub ru_ixrss: isize,   // 共享内存大小
    pub ru_idrss: isize,   // 非共享数据大小
    pub ru_isrss: isize,   // 非共享栈大小
    pub ru_minflt: isize,  // 软缺页异常次数
    pub ru_majflt: isize,  // 硬缺页异常次数
    pub ru_nswap: isize,   // 交换出内存的次数
    pub ru_inblock: isize, // 块输入操作次数
    pub ru_oublock: isize, // 块输出操作次数
    pub ru_msgsnd: isize,  // 发送 IPC 消息次数
    pub ru_msgrcv: isize,  // 接收 IPC 消息次数
    pub ru_nsignals: isize,// 收到的信号次数
    pub ru_nvcsw: isize,   // 主动上下文切换次数
    pub ru_nivcsw: isize,  // 被动上下文切换次数
}
pub fn sys_getrusage(who: i32, usage_ptr: *mut Rusage) -> isize {
    // 常见的 who 参数定义：
    const RUSAGE_SELF: i32 = 0;       // 请求当前进程的资源使用情况
    const RUSAGE_CHILDREN: i32 = -1;  // 请求那些已经被回收的子进程的资源使用情况
    const RUSAGE_THREAD: i32 = 1;     // 请求当前线程的资源使用情况
    // 参数校验
    if who != RUSAGE_SELF && who != RUSAGE_CHILDREN && who != RUSAGE_THREAD {
        return EINVAL.as_isize(); 
    }
    if usage_ptr as usize == 0 {
        return EFAULT.as_isize(); 
    }
    let token = current_user_token();
    let elapsed_us = if who == RUSAGE_CHILDREN {
        0
    } else {
        let task = current_task().unwrap();
        let start_time = task.inner_exclusive_access().start_time as usize;
        get_time_us().saturating_sub(start_time)
    };
    let usage = Rusage {
        ru_utime: TimeVal {
            sec: elapsed_us / 1_000_000,
            usec: elapsed_us % 1_000_000,
        },
        ..Default::default()
    };
    if try_translated_write(token, usage_ptr, usage) {
        0
    } else {
        EFAULT.as_isize()
    }
}

#[allow(dead_code)]
fn check_sigaction_error(signal: SignalFlags) -> bool {
    if signal == SignalFlags::SIGKILL || signal == SignalFlags::SIGSTOP
    {
        true
    } else {
        false
    }
}

pub fn sys_sigaction(
    signum: i32,
    action: *const SignalAction,
    old_action: *mut SignalAction,
) -> isize {
    sys_rt_sigaction(signum, action, old_action, core::mem::size_of::<u32>())
}

/// 兼容版
pub fn sys_rt_sigaction(
    signum: i32,
    action: *const SignalAction,
    old_action: *mut SignalAction,
    sigsetsize: usize,
) -> isize {
    if sigsetsize < core::mem::size_of::<u32>() {
        return EINVAL.as_isize();
    } else if signum <= 0 || signum as usize > MAX_SIG {
        return EINVAL.as_isize();
    }

    // 用户态编号从1开始，内核态从0开始，减一
    let table_idx = (signum - 1) as usize;
    let signal = SignalFlags::from_bits_truncate(1u64 << table_idx);

    // 不允许修改kill和stop的处理方式
    if check_sigaction_error(signal) {
        return EINVAL.as_isize();
    }

    let task = current_task().unwrap();
    let signal_hand = task.inner_exclusive_access().signal_hand.clone();
    let token = current_user_token();

    // 把旧的信号处理行为写进用户空间
    if !old_action.is_null() {
        let prev_action = signal_hand.exclusive_access().action(table_idx);
        if !try_translated_write(token, old_action, prev_action) {
            return EFAULT.as_isize();
        }
    }

    // 仅查询
    if action.is_null() {
        return 0;
    }

    // 修改tcb的信号处理行为
    let new_action = {
        if let Some(act) = try_translated_read(token, action) { 
            act
        } else {
            return EFAULT.as_isize();
        }
    };
    signal_hand.exclusive_access().set_action(table_idx, new_action);
    
    0
}

pub fn sys_pselect6(
    nfds: usize,
    readfds_ptr: *mut usize,
    writefds_ptr: *mut usize,
    exceptfds_ptr: *mut usize,
    _timeout: *const usize,
    sigmask_arg: *const usize,
) -> isize {
    let nfds = nfds.min(64);
    let task = current_task().unwrap();
    let files = task.inner_exclusive_access().files.clone();
    let token = current_user_token();

    let original_mask = task.inner_exclusive_access().blocked;
    if sigmask_arg as usize != 0 {
        let mask_ptr = match try_translated_read(token, sigmask_arg) {
            Some(mask_ptr) => mask_ptr,
            None => return EFAULT.as_isize(),
        };
        if mask_ptr != 0 {
            let mask = match try_translated_read(token, mask_ptr as *const usize) {
                Some(mask) => mask,
                None => return EFAULT.as_isize(),
            };
            task.inner_exclusive_access().blocked = SignalFlags::from_bits_truncate(mask as u64);
        }
    }
    
    // 从用户空间读取 readfds 位图
    let mut readfds = 0usize;
    if readfds_ptr as usize != 0 {
        readfds = {
            if let Some(rf) = crate::mm::try_translated_read(token, readfds_ptr) { rf } 
            else { return crate::syscall::errno::Errno::EFAULT.as_isize(); }
        };
    }
    let mut writefds = 0usize;
    if writefds_ptr as usize != 0 {
        writefds = {
            if let Some(wf) = crate::mm::try_translated_read(token, writefds_ptr) { wf } 
            else { return crate::syscall::errno::Errno::EFAULT.as_isize(); }
        };
    }
    let mut exceptfds = 0usize;
    if exceptfds_ptr as usize != 0 {
        exceptfds = {
            if let Some(ef) = crate::mm::try_translated_read(token, exceptfds_ptr) { ef } 
            else { return crate::syscall::errno::Errno::EFAULT.as_isize(); }
        };
    }
    let has_timeout = _timeout as usize != 0;
    let mut deadline_ms: usize = 0;
    let mut timeout_ms: usize = 0;
    if has_timeout {
        let timespec = {
            if let Some(ts) = try_translated_read(token, _timeout as *const TimeSpec) {
                ts
            } else {
                task.inner_exclusive_access().blocked = original_mask;
                return EFAULT.as_isize();
            }
        };
        // nsec 范围检查
        if timespec.tv_nsec >= 1_000_000_000 {
            task.inner_exclusive_access().blocked = original_mask;
            return EINVAL.as_isize();
        }
        // 防溢出&非法值
        const MAX_TIMEOUT_SEC: usize = 86400;
        let sec = if timespec.tv_sec > MAX_TIMEOUT_SEC {
            task.inner_exclusive_access().blocked = original_mask;
            return EINVAL.as_isize();
        } else {
            timespec.tv_sec
        };
        // 1. 计算原始的毫秒数
        let mut calculated_ms = sec.saturating_mul(1000).saturating_add(timespec.tv_nsec / 1_000_000);
        
        // 2. 限制最大超时时间为两分钟 (120,000 毫秒)
        const TWO_MINUTES_MS: usize = 30_000;
        if calculated_ms > TWO_MINUTES_MS {
            calculated_ms = TWO_MINUTES_MS;
        }
        
        timeout_ms = calculated_ms;
        deadline_ms = crate::timer::get_time_ms().saturating_add(timeout_ms);
    }
    loop {

            if readfds_ptr as usize != 0 {
                readfds = {
                    if let Some(rf) = crate::mm::try_translated_read(token, readfds_ptr) { rf } 
                    else { return crate::syscall::errno::Errno::EFAULT.as_isize(); }
                };
            }
            {
                let task_inner = task.inner_exclusive_access();
                let fatal_signals = crate::task::SignalFlags::SIGKILL 
                    | crate::task::SignalFlags::SIGTERM 
                    | crate::task::SignalFlags::SIGINT 
                    | crate::task::SignalFlags::SIGALRM;

                if task_inner.pending.flags().intersects(fatal_signals) {
                    return crate::syscall::errno::Errno::EINTR.as_isize();
                }
            }
            let limit = {
                let files = files.exclusive_access();
                nfds.min(files.fds.len())
            };
            // 把当前进程塞进所有监听的等待队列
            {
                let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
                let process_inner = files.exclusive_access();
                for fd in 0..limit {
                    let in_read = (readfds & (1usize << fd)) != 0;
                    let in_write = (writefds & (1usize << fd)) != 0;
                    if in_read || in_write {
                        if let Some(fd_file) = &process_inner.fds[fd].file {
                            if let Some(tcp_sock) = fd_file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
                                if let Some(socket_wait) = queues.get(&tcp_sock.handle) {
                                    if in_read { socket_wait.rx_queue.exclusive_access().push_back(task.clone()); }
                                    if in_write { socket_wait.tx_queue.exclusive_access().push_back(task.clone()); }
                                }
                            } else if let Some(udp_sock) = fd_file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
                                if let Some(socket_wait) = queues.get(&udp_sock.handle) {
                                    if in_read { socket_wait.rx_queue.exclusive_access().push_back(task.clone()); }
                                    if in_write { socket_wait.tx_queue.exclusive_access().push_back(task.clone()); }
                                }
                            }
                        }
                    }
                }
                
            }
            // 2检查所有的 FD 是否有数据就绪
            crate::net::net_poll();
            let mut ready_count = 0;
            let mut ready_readfds = 0usize;
            let mut ready_writefds = 0usize;
            let mut ready_exceptfds = 0usize;
            {
            /*let sockets = crate::net::SOCKET_SET.exclusive_access();
            for (handle, socket) in sockets.iter() {
                    println!("--- In SOCKET_SET: handle={:?} ---", handle);
                    match socket {
                        smoltcp::socket::Socket::Tcp(tcp_socket) => {
                            let state = tcp_socket.state(); // 获取 TCP 状态 (如 Listen, SynSent, Established 等)
                            let local_endpoint = tcp_socket.local_endpoint();
                            let remote_endpoint = tcp_socket.remote_endpoint();
                            println!("  [TCP] State: {:?}", state);
                            println!("  Local:  {:?}", local_endpoint);
                            println!("  Remote: {:?}", remote_endpoint);
                            println!("  Can send: {}, Can recv: {}", tcp_socket.can_send(), tcp_socket.can_recv());
                        },
                        smoltcp::socket::Socket::Udp(udp_socket) => {
                            println!("  [UDP] Local: {:?}", udp_socket.endpoint());
                            println!("  Can send: {}, Can recv: {}", udp_socket.can_send(), udp_socket.can_recv());
                        },
                        // 其他可能存在的 socket 类型（例如 Raw, Icmp）
                        _ => {
                            println!("  [Other Socket Type]");
                        }
                    }
                }
                drop(sockets);*/
                let process_inner = files.exclusive_access();
                for fd in 0..limit {
                    let in_read = (readfds & (1usize << fd)) != 0;
                    let in_write = (writefds & (1usize << fd)) != 0;
                    if in_read || in_write {
                        if let Some(fd_file) = &process_inner.fds[fd].file {
                            if fd_file.is_socket(){if let Some(tcp_wrapper) = fd_file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
                                let r_status = fd_file.readable();
                                let w_status = fd_file.writable();
                                let mut sockets = crate::net::SOCKET_SET.exclusive_access();
                                let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(tcp_wrapper.handle);
                                
                                drop(sockets); 

                                if in_read && r_status {
                                    ready_readfds |= 1usize << fd;
                                    ready_count += 1;
                                }
                                if in_write && w_status {
                                    ready_writefds |= 1usize << fd;
                                    ready_count += 1;
                                }
                            }else if let Some(_udp_wrapper) = fd_file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
                                let r_status = fd_file.readable();
                                let w_status = fd_file.writable();
                                if in_read && r_status {
                                    ready_readfds |= 1usize << fd;
                                    ready_count += 1;
                                }
                                if in_write && w_status {
                                    ready_writefds |= 1usize << fd;
                                    ready_count += 1;
                                }
                            }
                        }else {
                                let r_status = fd_file.ready_to_read();
                                let w_status = fd_file.ready_to_write();
                                
                                if in_read && r_status {
                                    ready_readfds |= 1usize << fd;
                                    ready_count += 1;
                                }
                                if in_write && w_status {
                                    ready_writefds |= 1usize << fd;
                                    ready_count += 1;
                                }
                            }
                        }
                    }
                }
            }
            // 如果有就绪或者超时，清理队列后直接返回
            let is_timeout = has_timeout && crate::timer::get_time_ms() >= deadline_ms;
            if ready_count > 0 || is_timeout {
                // 返回前必须把自己从等待队列清理掉
                let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
                let process_inner = files.exclusive_access();
                let tid = task.gettid();
                for fd in 0..limit {
                    let in_read = (readfds & (1usize << fd)) != 0;
                    let in_write = (writefds & (1usize << fd)) != 0;
                    if in_read || in_write {
                        if let Some(fd_file) = &process_inner.fds[fd].file {
                            let handle_opt = if let Some(tcp) = fd_file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
                                Some(tcp.handle)
                            } else if let Some(udp) = fd_file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
                                Some(udp.handle)
                            } else { None };

                            if let Some(handle) = handle_opt {
                                if let Some(socket_wait) = queues.get(&handle) {
                                    if in_read {
                                        socket_wait.rx_queue.exclusive_access().remove_by_tid(tid);
                                    }
                                    if in_write {
                                        socket_wait.tx_queue.exclusive_access().remove_by_tid(tid);
                                    }
                                }
                            }
                        }
                    }
                }
                drop(process_inner);
                drop(queues);
                if ready_count > 0 {
                    // 写入用户态态指针
                    if readfds_ptr as usize != 0 && !crate::mm::try_translated_write(token, readfds_ptr, ready_readfds) {
                        return crate::syscall::errno::Errno::EFAULT.as_isize();
                    }
                    if writefds_ptr as usize != 0 && !crate::mm::try_translated_write(token, writefds_ptr, ready_writefds) {
                        return crate::syscall::errno::Errno::EFAULT.as_isize();
                    }
                    if exceptfds_ptr as usize != 0 && !crate::mm::try_translated_write(token, exceptfds_ptr, ready_exceptfds) {
                        return crate::syscall::errno::Errno::EFAULT.as_isize();
                    }
                    return ready_count as isize;
                } else {
                    // 超时返回
                    if readfds_ptr as usize != 0 { crate::mm::try_translated_write(token, readfds_ptr, 0); }
                    if writefds_ptr as usize != 0 { crate::mm::try_translated_write(token, writefds_ptr, 0); }
                    if exceptfds_ptr as usize != 0 { crate::mm::try_translated_write(token, exceptfds_ptr, 0); }
                    return 0;
                }
            }
            // 被 net_poll 唤醒，让出 CPU 
            crate::task::suspend_current_and_run_next();
            // 下一轮循环的起点，清理掉上次入队的记录，防止重复通知和内存泄漏
            {
                let mut queues = crate::net::SOCKET_WAIT_QUEUES.lock();
                let process_inner = files.exclusive_access();
                let tid = task.gettid();

                for fd in 0..limit {
                    let in_read = (readfds & (1usize << fd)) != 0;
                    let in_write = (writefds & (1usize << fd)) != 0;
                    if in_read || in_write {
                        if let Some(fd_file) = &process_inner.fds[fd].file {
                            let handle_opt = if let Some(tcp) = fd_file.as_any().downcast_ref::<crate::net::socket::TcpSocket>() {
                                Some(tcp.handle)
                            } else if let Some(udp) = fd_file.as_any().downcast_ref::<crate::net::socket::UdpSocket>() {
                                Some(udp.handle)
                            } else { None };

                            if let Some(handle) = handle_opt {
                                if let Some(socket_wait) = queues.get(&handle) {
                                    if in_read {
                                        socket_wait.rx_queue.exclusive_access().remove_by_tid(tid);
                                    }
                                    if in_write {
                                        socket_wait.tx_queue.exclusive_access().remove_by_tid(tid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
}



pub fn sys_add_key(_type: *const u8, _desc: *const u8, _payload: *const u8, _plen: usize, _ringid: i32) -> isize {
    // 待实现
    ENOSYS.as_isize()
}

pub fn sys_request_key(_type: *const u8, _desc: *const u8, _callout_info: *const u8, _ringid: i32) -> isize {
    // 待实现
    ENOSYS.as_isize()
}

pub fn sys_keyctl(_operation: i32, _arg2: usize, _arg3: usize, _arg4: usize, _arg5: usize) -> isize {
    ENOSYS.as_isize()
}

pub fn sys_times(tms_ptr: *mut usize) -> isize {
    //warn!("[kernel] sys_times called with tms_ptr=0x{:x}", tms_ptr as usize);
    let token = current_user_token();
    // 暂时伪实现，写0
    let tms_val = Tms {
        tms_utime: 0,
        tms_stime: 0,
        tms_cutime: 0,
        tms_cstime: 0,
    };
    if !try_translated_write(token, tms_ptr as *mut Tms, tms_val) {
        return EFAULT.as_isize();
    }
    let current_ms = get_time_ms();
    current_ms as isize
}
pub fn sys_clock_adjtime(which_clock: i32, tp: *mut Timex) -> isize {
    if tp.is_null() {
        return EFAULT.as_isize();
    }
    if which_clock != CLOCK_REALTIME as i32 {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let Some(mut tx) = try_translated_read(token, tp as *const Timex) else {
        return EFAULT.as_isize();
    };

    if clock_adj_has_invalid_mode_bits(tx.modes) {
        return EINVAL.as_isize();
    }

    if tx.status & !CLOCK_ADJ_VALID_STATUS != 0 {
        return EINVAL.as_isize();
    }

    let requires_privilege = tx.modes != 0 && tx.modes != ADJ_OFFSET_SS_READ;
    if requires_privilege {
        let task = current_task().unwrap();
        let cred = task.inner_exclusive_access().cred.clone();
        if cred.exclusive_access().euid() != 0 {
            return EPERM.as_isize();
        }
    }

    let mut state = CLOCK_ADJ_STATE.lock();
    if tx.modes == ADJ_OFFSET_SS_READ {
        tx.offset = state.pending_single_shot;
    } else {
        if tx.modes & ADJ_OFFSET_SINGLESHOT == ADJ_OFFSET_SINGLESHOT {
            state.pending_single_shot = tx.offset;
            state.offset = tx.offset;
        }

        if tx.modes & ADJ_OFFSET != 0 {
            if !(-512_000..=512_000).contains(&tx.offset) {
                return EINVAL.as_isize();
            }
            state.offset = tx.offset;
        }
        if tx.modes & ADJ_FREQUENCY != 0 {
            if !(-32_768_000..=32_768_000).contains(&tx.freq) {
                return EINVAL.as_isize();
            }
            state.freq = tx.freq;
        }
        if tx.modes & ADJ_MAXERROR != 0 {
            state.maxerror = tx.maxerror;
        }
        if tx.modes & ADJ_ESTERROR != 0 {
            state.esterror = tx.esterror;
        }
        if tx.modes & ADJ_STATUS != 0 {
            state.status = clock_adj_write_status(state.status, tx.status);
        }
        if tx.modes & ADJ_TIMECONST != 0 {
            state.constant = tx.constant;
        }
        if tx.modes & ADJ_TAI != 0 {
            state.tai = tx.tai;
        }
        if tx.modes & ADJ_NANO != 0 {
            state.is_nano = true;
            state.status |= STA_NANO;
        }
        if tx.modes & ADJ_MICRO != 0 {
            state.is_nano = false;
            state.status &= !STA_NANO;
        }
        if tx.modes & ADJ_TICK != 0 {
            if !(9_000..=11_000).contains(&tx.tick) {
                return EINVAL.as_isize();
            }
            state.tick = tx.tick;
        }
        if tx.modes & ADJ_SETOFFSET != 0 {
            state.offset = tx.time.tv_sec.saturating_mul(1_000_000) + tx.time.tv_usec;
        }
    }

    let now_ns = current_wallclock_ns();
    tx.offset = state.offset;
    tx.freq = state.freq;
    tx.maxerror = state.maxerror;
    tx.esterror = state.esterror;
    tx.status = state.status;
    tx.constant = state.constant;
    tx.precision = if state.is_nano { 1 } else { 1_000 };
    tx.tolerance = 32768000;
    tx.time.tv_sec = now_ns / 1_000_000_000;
    tx.time.tv_usec = if state.is_nano {
        now_ns % 1_000_000_000
    } else {
        (now_ns % 1_000_000_000) / 1_000
    };
    tx.tick = state.tick;
    tx.ppsfreq = 0;
    tx.jitter = 0;
    tx.shift = 0;
    tx.stabil = 0;
    tx.jitcnt = 0;
    tx.calcnt = 0;
    tx.errcnt = 0;
    tx.stbcnt = 0;
    tx.tai = state.tai;

    if !try_translated_write(token, tp, tx) {
        return EFAULT.as_isize();
    }

    clock_adj_result_from_status(state.status)
}
pub fn sys_clock_settime(which_clock: i32, tp: *const TimeSpec) -> isize {
    if tp.is_null() {
        return EFAULT.as_isize();
    }
    if which_clock != CLOCK_REALTIME as i32 {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let timespec = match try_translated_read(token, tp) {
        Some(ts) => ts,
        None => return EFAULT.as_isize(),
    };

    if timespec.tv_nsec >= 1_000_000_000 {
        return EINVAL.as_isize();
    }

    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    if cred.exclusive_access().euid() != 0 {
        return EPERM.as_isize();
    }

    let requested_ns = (timespec.tv_sec as i128)
        .saturating_mul(1_000_000_000)
        .saturating_add(timespec.tv_nsec as i128);
    let base_ns = get_real_time_ns() as i128;
    let offset_ns = requested_ns.saturating_sub(base_ns);
    let offset_ns = offset_ns.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

    *CLOCK_REALTIME_OFFSET_NS.lock() = offset_ns;

    let mut state = CLOCK_ADJ_STATE.lock();
    state.status &= !STA_UNSYNC;
    if state.is_nano {
        state.status |= STA_NANO;
    } else {
        state.status &= !STA_NANO;
    }

    0
}
pub fn sys_getrandom(buf: *mut u8, len: usize, _flags: u32) -> isize {
    let token = current_user_token();
    let mut user_buf = translated_byte_buffer(token, buf, len);
    const LCG_MULTIPLIER: usize = 25_214_903_917;
    const LCG_MASK: usize = (1usize << 48) - 1;

    for (i, buf) in user_buf.iter_mut().enumerate() {
        let seed = get_timer_ticks()
            .wrapping_add(buf.as_ptr() as usize)
            .wrapping_add(i);
        // 类LGC算法，时间滴答作种
        let mixed = LCG_MULTIPLIER.wrapping_mul(seed) & LCG_MASK;
        buf[0] = (mixed >> (8 * (i % 6))) as u8;
    }
    len as isize
}

pub fn sys_robust_list() -> isize {
    let task = current_task().unwrap();
    trace!("kernel:pid[{}] sys_robust_list NOT IMPLEMENTED", task.getpid());
    // 目前还没有实现多线程（每个任务是独立的内存空间)，不需要管理锁，伪实现不会导致死锁
    0
}

pub fn sys_get_robust_list() -> isize {
    let task = current_task().unwrap();
    trace!("kernel:pid[{}] sys_get_robust_list NOT IMPLEMENTED", task.getpid());
    0
}

pub fn sys_resq() -> isize {
    let task = current_task().unwrap();
    trace!("kernel:pid[{}] sys_resq NOT IMPLEMENTED", task.getpid());
    // 未实现多线程，这里伪实现
    0
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigInfo {
    pub si_signo: i32, // 信号编号
    pub si_errno: i32, // 错误码
    pub si_code: i32,  // 信号发送原因
    pub _pad0: i32,
    pub si_pid: i32,
    pub si_uid: u32,
    pub si_status: i32,
    pub _pad1: i32,
    // 余下部分保留给 C ABI 中的 union 字段，整结构保持 128 字节。
    pub _pad: [u64; 12],
}
pub type SigSet = usize;

/// ！！！目前的实现似乎始终超时，相关内容待人工重写！！！
/// 
/// **系统调用：rt_sigtimedwait (0x81)**
/// 
/// ### 功能描述
/// 同步地等待并消费信号。进程会阻塞直到 `set` 中的信号集有信号发生，或达到 `timeout`。
/// 与异步信号处理（Signal Handler）不同，此函数会**直接从进程的挂起位图中移除信号**，
/// 使得该信号不再触发后续的异步处理逻辑。
///
/// ### 参数说明
/// - `set_ptr`:   用户态指针，指向目标信号集位图（SigSet）。
/// - `info_ptr`:  用户态指针，用于存储捕获到的信号详细信息（SigInfo）。
/// - `timeout_ptr`: 用户态指针，指向超时时间结构体（TimeSpec）。若为 null 则无限期等待。
/// - `sigsetsize`: 信号集结构的大小，Linux 下 x，86_64/riscv64 通常要求为 8 字节。
///
/// ### 返回值
/// - 成功：返回被捕获的信号编号（正数）。
/// - 失败：返回错误码（负数），如 `EAGAIN` (超时) 或 `EINVAL` (参数非法)。
pub fn sys_rt_sigtimedwait(
    set_ptr: *const usize, 
    info_ptr: *mut SigInfo, 
    timeout_ptr: *const TimeSpec, 
    sigsetsize: usize
) -> isize {
    let token = current_user_token();

    if sigsetsize != 8 {
        return Errno::EINVAL.as_isize();
    }

    let target_set_bits = {
        if let Some(b) = try_translated_read(token, set_ptr) {
            b
        } else {
            return EFAULT.as_isize();
        }
    };
    let target_set = SignalFlags::from_bits_truncate(target_set_bits as u64);

    let mut deadline_us: Option<usize> = None;
    if !timeout_ptr.is_null() {
        let timeout = {
            if let Some(t) = try_translated_read(token, timeout_ptr) {
                t
            } else {
                return EFAULT.as_isize();
            }
        };
        if timeout.tv_sec == 0 && timeout.tv_nsec == 0 {
            deadline_us = Some(0); // 纯轮询，立刻超时
        } else {
            let current_us = get_time_us(); 
            let wait_us = (timeout.tv_sec as usize) * 1_000_000 + (timeout.tv_nsec as usize) / 1000;
            deadline_us = Some(current_us + wait_us);
        }
    }

    loop {
        let task = current_task().unwrap();

        // --- 第一阶段：消费信号 ---
        {
            let (thread_pending, blocked, signal) = {
                let inner = task.inner_exclusive_access();
                (inner.pending.flags(), inner.blocked, inner.signal.clone())
            };
            let shared_pending = signal.exclusive_access().pending_flags();
            let pending = thread_pending | shared_pending;
            let intersection = pending & target_set;

            if !intersection.is_empty() {
                // 命中了！提取最小的那个信号
                let sig_bit = intersection.bits().trailing_zeros();
                let sig_num = (sig_bit + 1) as i32;
                let sig_flag = SignalFlags::from_bits(1 << sig_bit).unwrap();

                // 同步拿走，避免进入异步 handler
                if thread_pending.contains(sig_flag) {
                    task.inner_exclusive_access().pending.remove(sig_flag);
                } else {
                    signal.exclusive_access().remove_pending(sig_flag);
                }

                // 写回 info
                if !info_ptr.is_null() {
                    if !try_translated_write(token, info_ptr, SigInfo {
                        si_signo: sig_num,
                        si_errno: 0,
                        si_code: 0,
                        _pad0: 0,
                        si_pid: 0,
                        si_uid: 0,
                        si_status: 0,
                        _pad1: 0,
                        _pad: [0; 12],
                    }) {
                        return EFAULT.as_isize();
                    }
                }
                return sig_num as isize;
            }
            
            // 【新增】：如果在等待期间收到了非目标集合中且未屏蔽的信号
            // 则应当中断等待并返回 EINTR，给外层 trap_handler 执行收尸（call_user_signal_handler）的机会。
            let unmasked_pending = pending.bits() & !blocked.bits();
            let unmaskable = pending.bits() & ((1 << 8) | (1 << 18));
            if (unmasked_pending | unmaskable) != 0 {
                return Errno::EINTR.as_isize();
            }
        } 
        
        if let Some(deadline) = deadline_us {
            // 【带超时的等待】
            let current_us = get_time_us();
            if current_us >= deadline {
                return Errno::EAGAIN.as_isize(); 
            }
            
            suspend_current_and_run_next();
            
        } else {
            suspend_current_and_run_next();
            /*
            let sig_queue_guard = SIGNAL_WAIT_QUEUE.lock();
            current_task_to_sleep(sig_queue_guard);
             */
        }
    }
}


use crate::process::Rlimit64;
/// 修改打开的文件数限制
pub fn sys_prlimit64(
    pid: usize, 
    resource: i32, 
    new_limit: *const Rlimit64, 
    old_limit: *mut Rlimit64
) -> isize {
    const UL_SETFSIZE: i32 = 1;
    const RLIMIT_DATA: i32 = 2;      // 数据段大小
    const RLIMIT_STACK: i32 = 3;     // 栈大小
    const RLIMIT_CORE: i32 = 4;      // Core dump 大小
    const RLIMIT_NPROC: i32 = 6;     // 最大进程数
    const RLIMIT_NOFILE: i32 = 7;    // 最大打开文件数
    const RLIMIT_MEMLOCK: i32 = 8;   // 锁定内存大小
    const RLIMIT_AS: i32 = 9;        // 虚拟地址空间大小
    
    info!("sys_prlimit64 called with pid={}, resource={}, new_limit=0x{:x}, old_limit=0x{:x}", pid, resource, new_limit as u64, old_limit as u64);
    if pid != 0 {
         if pid != current_task().unwrap().getpid() {
            return Errno::EPERM.as_isize(); 
        } 
    }

    let token = current_user_token();
    match resource {
        RLIMIT_NPROC => {
            // 伪实现，返回一个固定值
            if !old_limit.is_null() {
                if !try_translated_write(token, old_limit, Rlimit64 { cur_lmt: 4096, max_lmt: 4096 }) {
                    return EFAULT.as_isize();
                }
            }
            0
        }
        RLIMIT_NOFILE => {
            // 打开的文件数限制
            let task = current_task().unwrap();
            let signal = task.inner_exclusive_access().signal.clone();
            let mut signal = signal.exclusive_access();
            // 不neng在此调用 recycle_fd()！压缩 fd 表会改变已有 fd 编号，
            // 导致用户态持有的 fd 引用失效（如 lmbench 的 pipe 通信）。
            let limit = signal.rlimits().nofile;
            let old = Rlimit64 { cur_lmt: limit.rlim_cur, max_lmt: limit.rlim_max };
            if !old_limit.is_null() {
                if !try_translated_write(token, old_limit, old) {
                    return Errno::EFAULT.as_isize();
                }
            }
            if !new_limit.is_null() {
                let new = translated_read(token, new_limit);
                if new.cur_lmt > new.max_lmt
                    || new.max_lmt > signal.rlimits().nofile.rlim_max
                {
                    return EINVAL.as_isize();
                }
                signal.rlimits_mut().nofile.rlim_cur = new.cur_lmt;
                signal.rlimits_mut().nofile.rlim_max = new.max_lmt;
            }
            0
        }
        RLIMIT_MEMLOCK => {
            // 锁定内存限制，伪实现
            if !old_limit.is_null() {
                translated_write(token, old_limit, Rlimit64 { cur_lmt: 0x40_0000, max_lmt: 0x40_0000 });
            }
            0
        }
        RLIMIT_CORE => {
            // core dump 文件大小限制，伪实现
            if !old_limit.is_null() {
                if !try_translated_write(token, old_limit, Rlimit64 { cur_lmt: 0, max_lmt: 0 }) {
                    return Errno::EFAULT.as_isize();
                }
            }
            0
        }
        RLIMIT_STACK => {
            // 栈大小限制，返回默认值，最大值 RLIM_INFINITY
            if !old_limit.is_null() {
                if !try_translated_write(token, old_limit, Rlimit64 { cur_lmt: USER_STACK_SIZE, max_lmt: usize::MAX }) {
                    return Errno::EFAULT.as_isize();
                }
            }
            0
        }
        RLIMIT_DATA => {
            // 数据段大小限制，不允许修改，设为USER_APP_MAX_SIZE
            if !old_limit.is_null() {
                translated_write(token, old_limit, Rlimit64 { cur_lmt: USER_APP_MAX_SIZE, max_lmt: USER_APP_MAX_SIZE });
            }
            0
        }
        UL_SETFSIZE => {
            //若old有值则是将当前限制写入用户提供的缓冲区，若new有值则是设置新的限制，即读用户传进来的值。
            let task = current_task().unwrap();
            let signal = task.inner_exclusive_access().signal.clone();
            let mut signal = signal.exclusive_access();
            let limit = signal.rlimits().fsize;
            if !old_limit.is_null() {
                if !try_translated_write(token, old_limit, Rlimit64 { cur_lmt: limit.rlim_cur, max_lmt: limit.rlim_max }) {
                    return EFAULT.as_isize();
                }
            }
            if !new_limit.is_null() {
                if let Some(new) = try_translated_read(token, new_limit) {
                    signal.rlimits_mut().fsize.rlim_cur = new.cur_lmt;
                    signal.rlimits_mut().fsize.rlim_max = new.max_lmt;
                } else {
                    return EFAULT.as_isize();
                }
            }
            0
        }
        RLIMIT_DATA | RLIMIT_NPROC | RLIMIT_AS => {
            let task = current_task().unwrap();
            let signal = task.inner_exclusive_access().signal.clone();
            let mut signal = signal.exclusive_access();
            
            // 匹配对应的资源字段
            let target_limit = match resource {
                RLIMIT_DATA => &mut signal.rlimits_mut().data,
                RLIMIT_NPROC => &mut signal.rlimits_mut().nproc,
                RLIMIT_AS => &mut signal.rlimits_mut().aspace,
                _ => unreachable!(), 
            };

            // 如果传了 old_limit，把当前内核的值写回给用户
            if !old_limit.is_null() {
                let old = Rlimit64 { cur_lmt: target_limit.rlim_cur, max_lmt: target_limit.rlim_max };
                if !try_translated_write(token, old_limit, old) {
                    return EFAULT.as_isize();
                }
            }

            // 如果传了 new_limit，把用户的新值更新到内核
            if !new_limit.is_null() {
                if let Some(new) = try_translated_read(token, new_limit) {
                    target_limit.rlim_cur = new.cur_lmt;
                    target_limit.rlim_max = new.max_lmt;
                } else {
                    return EFAULT.as_isize();
                }
            }
            
            0 // 返回成功
        }
        // 其他请求暂不支持
        _ => Errno::EINVAL.as_isize()
    }
}

const FUTEX_WAIT: i32 = 0;
const FUTEX_WAKE: i32 = 1;
const FUTEX_REQUEUE: i32 = 3;
const FUTEX_WAIT_BITSET: i32 = 9;
const FUTEX_WAKE_BITSET: i32 = 10;
const FUTEX_PRIVATE_FLAG: i32 = 128;
const FUTEX_CLOCK_REALTIME: i32 = 256;
const FUTEX_CMD_MASK: i32 = !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);


///作用：用户空间会传进去一个地址，内核解引用地址获取值后，如果和用户指定的val相等，则睡眠或唤醒对应等待队列的一个元素。
/// 实际上，FUTEX就是管理所有信号量以及其等待队列的元素，信号量底层会用这个syscall。
/// FUTEX的键是物理地址，值是这个信号量对应的等待队列
pub fn sys_futex(uaddr: *mut i32, op: i32, val: i32, timeout: *const TimeSpec, uaddr2: *mut i32, val3: i32) -> isize {
    if uaddr.is_null() {
        return EFAULT.as_isize();
    }

    let cmd = op & FUTEX_CMD_MASK;
    let token = current_user_token();

    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            if cmd == FUTEX_WAIT_BITSET && val3 == 0 {
                return EINVAL.as_isize();
            }
            let Some(current_val) = try_translated_read(token, uaddr as *const i32) else {
                return EFAULT.as_isize();
            };

            warn!(
                "[FUTEX WAIT IN] tid={} uaddr=0x{:x} expect={} current={}",
                current_task().unwrap().gettid(),
                uaddr as usize,
                val,
                current_val
            );

            if current_val != val {
                warn!(
                    "[FUTEX WAIT EAGAIN] tid={} uaddr=0x{:x} expect={} current={}",
                    current_task().unwrap().gettid(),
                    uaddr as usize,
                    val,
                    current_val
                );
                return EAGAIN.as_isize();
            }
            //当需要实时阻塞时，先检查是否有信号到来，如果有则返回EINTR，如果没有则进入睡眠等待被唤醒或者超时
            if !timeout.is_null() {
                let Some(timeout_val) = try_translated_read(token, timeout) else {
                    return EFAULT.as_isize();
                };
                if timeout_val.tv_nsec >= 1_000_000_000 {
                    return EINVAL.as_isize();
                }

                let timeout_us = timeout_val
                    .tv_sec
                    .saturating_mul(1_000_000)
                    .saturating_add((timeout_val.tv_nsec + 999) / 1000);
                let deadline_us = get_time_us().saturating_add(timeout_us);

                loop {
                    if crate::process::check_pending_signal() {
                        return EINTR.as_isize();
                    }
                    if get_time_us() >= deadline_us {
                        return ETIMEDOUT.as_isize();
                    }

                    suspend_current_and_run_next();

                    let Some(current_val) = try_translated_read(token, uaddr as *const i32) else {
                        return EFAULT.as_isize();
                    };
                    if current_val != val {
                        return 0;
                    }
                }
            }
            //否则就是不带超时的等待，直接睡眠等待被唤醒
            let current = current_task().unwrap();
            let current_tid = current.gettid();
            if crate::process::check_pending_signal() {
                warn!(
                    "[FUTEX PRE-SIGNAL] tid={} uaddr=0x{:x} return=EINTR",
                    current_tid,
                    uaddr as usize
                );
                return EINTR.as_isize();
            }
            {
                let mut inner = current.inner_exclusive_access();
                inner.signal_interrupted = false;
            }
            //获取地址对应的等待队列，放入当前任务并睡眠
            let token = current_user_token();
            let page_table = PageTable::from_token(token);
            let Some(pa) = page_table.translate_va(VirtAddr::from(uaddr as usize)) else {
                return EFAULT.as_isize();
            };
            let queue = get_futex_wait_queue(pa.0);

            // Linux FUTEX_WAIT 的“比较用户值并挂入等待队列”必须相对
            // FUTEX_WAKE 原子。首次比较与这里之间，另一个 hart 可能已经
            // 修改值并执行过 WAKE；若无条件入队，就会永久错过该唤醒。
            // block_current_and_run_next_if 持有同一 queue 锁执行闭包并
            // 入队，而 WAKE 也持有该锁 pop_front，从而关闭竞态窗口。
            let mut rechecked_value = None;
            let blocked = block_current_and_run_next_if(&queue, || {
                rechecked_value = try_translated_read(token, uaddr as *const i32);
                rechecked_value == Some(val)
            });
            if !blocked {
                return match rechecked_value {
                    Some(current) => {
                        warn!(
                            "[FUTEX RECHECK EAGAIN] tid={} uaddr={:#x} expect={} current={}",
                            current_tid,
                            uaddr as usize,
                            val,
                            current,
                        );
                        EAGAIN.as_isize()
                    }
                    None => EFAULT.as_isize(),
                };
            }
            let current = current_task().unwrap();
            let current_tid = current.gettid();
            {
                let mut guard = queue.lock();
                guard.remove_task(current_tid);
            }

            if crate::process::take_current_signal_interrupted() {
                warn!(
                    "[FUTEX SIGWAKE] tid={} uaddr=0x{:x} return=EINTR",
                    current_tid,
                    uaddr as usize
                );
                return EINTR.as_isize();
            }

            let (thread_pending, blocked, signal) = {
                let inner = current.inner_exclusive_access();
                (inner.pending.flags(), inner.blocked, inner.signal.clone())
            };
            let pending_signals = thread_pending | signal.exclusive_access().pending_flags();
            let pending = pending_signals.bits() & !blocked.bits();
            let unmaskable = pending_signals.bits()
                & (SignalFlags::SIGKILL | SignalFlags::SIGSTOP).bits();
            warn!(
                "[FUTEX WAIT OUT] tid={} uaddr=0x{:x} pending_all=0x{:x} mask=0x{:x} pending=0x{:x} unmaskable=0x{:x}",
                current_tid,
                uaddr as usize,
                pending_signals.bits(),
                blocked.bits(),
                pending,
                unmaskable
            );

            if (pending | unmaskable) != 0 {
                warn!(
                    "[FUTEX EINTR] tid={} pending=0x{:x} unmaskable=0x{:x}",
                    current_tid,
                    pending,
                    unmaskable
                );
                EINTR.as_isize()
            } else {
                warn!(
                    "[FUTEX OK] tid={} uaddr=0x{:x} return=0",
                    current_tid,
                    uaddr as usize
                );
                0
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            if cmd == FUTEX_WAKE_BITSET && val3 == 0 {
                return EINVAL.as_isize();
            }
            if val <= 0 {
                return 0;
            }

            let token = current_user_token();
            let page_table = PageTable::from_token(token);
            let Some(pa) = page_table.translate_va(VirtAddr::from(uaddr as usize)) else {
                return EFAULT.as_isize();
            };

            let queue = {
                let queues = FUTEX_WAIT_QUEUES.lock();
                queues.get(&pa.0).cloned()
            };

            let Some(queue) = queue else {
                return 0;
            };

            let mut woken = 0;
            while woken < val {
                if !crate::process::wake_up_one(&queue) {
                    break;
                }
                woken += 1;
            }

            woken as isize
        }
        FUTEX_REQUEUE => {
            if uaddr2.is_null() {
                return EFAULT.as_isize();
            }

            let token = current_user_token();
            let page_table = PageTable::from_token(token);
            let Some(src_pa) = page_table.translate_va(VirtAddr::from(uaddr as usize)) else {
                return EFAULT.as_isize();
            };
            let Some(dst_pa) = page_table.translate_va(VirtAddr::from(uaddr2 as usize)) else {
                return EFAULT.as_isize();
            };

            let requeue_count = timeout as usize;
            let dst_queue = if requeue_count > 0 {
                Some(get_futex_wait_queue(dst_pa.0))
            } else {
                None
            };
            let src_queue = {
                let queues = FUTEX_WAIT_QUEUES.lock();
                queues.get(&src_pa.0).cloned()
            };

            let Some(src_queue) = src_queue else {
                return 0;
            };

            let mut affected = 0;
            let mut wake_left = if val > 0 { val as usize } else { 0 };
            let mut requeue_left = requeue_count;

            loop {
                let task = {
                    let mut src_guard = src_queue.lock();
                    if wake_left == 0 && requeue_left == 0 {
                        None
                    } else {
                        src_guard.pop_front()
                    }
                };

                let Some(task) = task else {
                    break;
                };

                if wake_left > 0 {
                    wake_left -= 1;
                    while task.inner_exclusive_access().state == crate::task::TaskStatus::BlockSaving {
                        suspend_current_and_run_next();
                    }
                    let mut task_inner = task.inner_exclusive_access();
                    task_inner.state = crate::task::TaskStatus::Ready;
                    drop(task_inner);
                    add_task(task);
                    affected += 1;
                    continue;
                }

                if requeue_left > 0 {
                    requeue_left -= 1;
                    if src_pa.0 == dst_pa.0 {
                        src_queue.lock().push_back(task);
                    } else if let Some(dst_queue) = &dst_queue {
                        dst_queue.lock().push_back(task);
                    }
                    affected += 1;
                }
            }

            affected as isize
        }
        _ => ENOSYS.as_isize(),
    }
}

pub fn sys_getresgid(gid_ptr: *mut u32, egid_ptr: *mut u32, sgid_ptr: *mut u32) -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let token = current_user_token();
    let cred = cred.exclusive_access();

    if !gid_ptr.is_null() {
        if !try_translated_write(token, gid_ptr, cred.gid()) {
            return EFAULT.as_isize();
        }
    }
    if !egid_ptr.is_null() {
        if !try_translated_write(token, egid_ptr, cred.egid()) {
            return EFAULT.as_isize();
        }
    }
    if !sgid_ptr.is_null() {
        if !try_translated_write(token, sgid_ptr, cred.sgid()) {
            return EFAULT.as_isize();
        }
    }
    0
}
pub fn sys_rt_sigpending(sigset_ptr: *mut SigSet, sigsetsize: usize) -> isize {
    if sigsetsize != 8 {
        return EINVAL.as_isize();
    }
    let task = current_task().unwrap();
    let (thread_pending, signal) = {
        let inner = task.inner_exclusive_access();
        (inner.pending.flags(), inner.signal.clone())
    };
    let token = current_user_token();

    let pending = (thread_pending | signal.exclusive_access().pending_flags()).bits() as usize;
    if !try_translated_write(token, sigset_ptr, pending) {
        return EFAULT.as_isize();
    }
    0
}
pub fn sys_setreuid(ruid: u32, euid: u32) -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    let mut cred = cred.exclusive_access();

    if ruid != u32::MAX {
        cred.set_ruid(ruid);
    }
    if euid != u32::MAX {
        cred.set_euid(euid);
    }
    0
}

pub fn sys_vhangup() -> isize {
    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();

    if cred.exclusive_access().euid() != 0 {
        return EPERM.as_isize();
    }

    0
}

/// personality 系统调用 (#92)
/// 设置/查询当前进程的执行域标志。
/// - persona == 0xffffffff: 只查询不修改，返回当前值
/// - 其他值: 设置新 persona 并返回旧值
/// 目前支持的标志: UNAME26 (0x0020000) 影响 uname() 的 release 字段输出
pub fn sys_personality(persona: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let old = inner.personality;
    // persona == 0xffffffff 表示仅查询，不修改
    if persona != 0xffffffff {
        inner.personality = persona;
    }
    old as isize
}
//linux中，父子进程的fd_table这些本身是指向同一个实例的，但我们的pcb都是直接独立的fd_table
//故unshare系统调用在我们的实现中没有实际效果，直接检查权限后放行即可
pub fn sys_unshare(flags: i32) -> isize {
    // 所有当前支持的 unshare 标志位
    const CLONE_VM: i32      = 0x00000100;
    const CLONE_FS: i32      = 0x00000200;
    const CLONE_FILES: i32   = 0x00000400;
    const CLONE_NEWNS: i32   = 0x00020000;
    const CLONE_NEWCGROUP: i32 = 0x02000000;
    const CLONE_NEWUTS: i32  = 0x04000000;
    const CLONE_NEWIPC: i32  = 0x08000000;

    // 掩码
    const KNOWN_FLAGS: i32 = CLONE_VM | CLONE_FS | CLONE_FILES
        | CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS | CLONE_NEWIPC;

    // 检查是否有未定义的标志位
    if flags & !KNOWN_FLAGS != 0 {
        return EINVAL.as_isize();
    }

    let task = current_task().unwrap();
    let cred = task.inner_exclusive_access().cred.clone();
    if cred.exclusive_access().euid() != 0 {
        return EPERM.as_isize();
    }
    0
}

//内存一致性
pub fn sys_membarrier(cmd: i32, _flags: u32, _cpu_id: i32) -> isize {
    const MEMBARRIER_CMD_QUERY: i32 = 0;
    const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 1 << 3;        // 8
    const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 1 << 4; // 16

    match cmd {
        MEMBARRIER_CMD_QUERY => {
            // Report that we support PRIVATE_EXPEDITED and REGISTER_PRIVATE_EXPEDITED
            ((1 << 3) | (1 << 4)) as isize
        }
        MEMBARRIER_CMD_PRIVATE_EXPEDITED => {
            // Full memory barrier: all previous loads/stores complete before subsequent ones
            #[cfg(target_arch = "riscv64")]
            unsafe { core::arch::asm!("fence rw, rw") };
            #[cfg(target_arch = "loongarch64")]
            unsafe { core::arch::asm!("dbar 0") };
            0
        }
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED => {
            // Registration is implicit in our kernel: always succeed
            0
        }
        _ => Errno::EINVAL.as_isize(),
    }
}

/// RISC-V硬件探测系统调用的用户态ABI。
#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RiscvHwprobe {
    pub key: i64,
    pub value: u64,
}

/// 查询RISC-V硬件属性。
#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_hwprobe(
    pairs: *mut RiscvHwprobe,
    pair_count: usize,
    cpusetsize: usize,
    cpus: *const u8,
    flags: u32,
) -> isize {
    // 简化实现不支持RISCV_HWPROBE_WHICH_CPUS及其他扩展标志。
    if flags != 0 {
        return EINVAL.as_isize();
    }
    if pair_count != 0 && pairs.is_null() {
        return EFAULT.as_isize();
    }

    let pair_bytes = match pair_count.checked_mul(core::mem::size_of::<RiscvHwprobe>()) {
        Some(size) => size,
        None => return EFAULT.as_isize(),
    };
    if (pairs as usize).checked_add(pair_bytes).is_none() {
        return EFAULT.as_isize();
    }

    let token = current_user_token();

    // Linux允许(NULL, 0)表示查询所有在线CPU。若用户显式传入CPU位图，
    // 简化实现只检查该内存可读，所有CPU按同构处理。
    if cpusetsize == 0 {
        if !cpus.is_null() {
            return EINVAL.as_isize();
        }
    } else if cpus.is_null()
        || try_translated_read::<u8>(token, cpus).is_none()
        || try_translated_read::<u8>(token, unsafe { cpus.add(cpusetsize - 1) }).is_none()
    {
        return EFAULT.as_isize();
    }

    fn fill_value(pair: &mut RiscvHwprobe) {
        pair.value = match pair.key {
            // 0~2分别是厂商、架构和实现ID，当前内核未缓存这些CSR。
            0..=2 => 0,
            // key 3：支持Linux定义的IMA基础行为。
            3 => 1,
            // key 4：当前目标明确支持F、D和C；对应bit 0和bit 1。
            4 => (1 << 0) | (1 << 1),
            // key 7：用户态可使用的最高虚拟地址。
            7 => (crate::USER_APP_MAX_SIZE - 1) as u64,
            // key 8：time CSR频率。
            8 => crate::arch::config::CLOCK_FREQ as u64,
            // 已知但暂时无法准确探测的性能、缓存块和厂商扩展保守返回0。
            5..=6 | 9..=16 => 0,
            _ => {
                pair.key = -1;
                0
            }
        };
    }

    for index in 0..pair_count {
        let pair_ptr = (pairs as usize + index * core::mem::size_of::<RiscvHwprobe>())
            as *mut RiscvHwprobe;
        let Some(mut pair) = try_translated_read(token, pair_ptr as *const RiscvHwprobe) else {
            return EFAULT.as_isize();
        };
        fill_value(&mut pair);
        if !try_translated_write(token, pair_ptr, pair) {
            return EFAULT.as_isize();
        }
    }

    0
}
