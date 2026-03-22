//! Process management syscalls
//! 这里是进程管理相关的系统调用实现，包含了进程创建、退出、等待、信号等功能
//! 内存管理也暂时放在此处
use crate::get_hart_id;
pub use crate::{
    arch::timer::{get_time_ms,get_time_us, get_timer_ticks}, 
    fs::*, 
    mm::{UserBuffer, mmap, translated_byte_buffer, translated_ref, translated_refmut, translated_str}, 
    process::{
        task::{
            MAX_SIG, SignalAction, SignalFlags, add_task, current_task, current_user_token, exit_current_and_run_next, suspend_current_and_run_next
        },
        clone::*,
        manager::*
    },
    syscall::errno::Errno
};
use alloc::task;
pub use alloc::{string::{String,ToString}, sync::Arc, vec::Vec};

use super::{errno::Errno::*, normalize_leading_dot_path};

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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,    // 行数
    pub ws_col: u16,    // 列数
    pub ws_xpixel: u16, // 像素宽度 (通常不用，填 0)
    pub ws_ypixel: u16, // 像素高度 (通常不用，填 0)
}
#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
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

pub fn sys_ppoll(ufds_ptr: usize, nfds: usize, _tmo_p: usize, _sigmask: usize) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    let token = inner.get_user_token();
    if ufds_ptr == 0 || nfds == 0 {
        return 0; 
    }
    let mut ready_count = 0;
    // 遍历用户传进来的 pollfd 数组
    for i in 0..nfds {
        // 根据虚拟地址算出真实物理地址，并拿到可变引用
        let pollfd_ptr = (ufds_ptr + i * core::mem::size_of::<PollFd>()) as *mut PollFd;
        let pollfd = translated_refmut(token, pollfd_ptr);
        let fd = pollfd.fd;
        pollfd.revents = 0; // 先清空返回状态
        // 负数的 fd 按照 POSIX 标准被忽略
        if fd < 0 {
            continue;
        }
        let fd_usize = fd as usize;
        
        // 检查 fd 是否合法
        if fd_usize >= inner.fd_table.len() || inner.fd_table[fd_usize].file.is_none() {
            pollfd.revents = POLLERR; // 报错：坏的描述符
            ready_count += 1;
        } else {
            let file = inner.fd_table[fd_usize].file.as_ref().unwrap();
            // 没做复杂的阻塞等待，直接查看文件状态并标记
            if (pollfd.events & POLLIN) != 0 && file.readable() {
                pollfd.revents |= POLLIN;
            }
            if (pollfd.events & POLLOUT) != 0 && file.writable() {
                pollfd.revents |= POLLOUT;
            }
            if pollfd.revents != 0 {
                ready_count += 1;
            }
        }
    }
    // 返回有多少个 FD 已经准备好了
    ready_count as isize
}
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().process().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_gettid() -> isize {
    // 目前线程ID和进程ID是一样的
    sys_getpid()
}

pub fn sys_getuid() -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    inner.uid as isize
}
// 假装获取成功，返回 PGID 为 0
pub fn sys_getpgid(_pid: usize) -> isize { 
    0 
}

// 假装设置成功，返回 0
pub fn sys_setpgid(_pid: usize, _pgid: usize) -> isize { 
    0 
}

pub fn sys_getgid() -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    inner.gid as isize
}

pub fn sys_geteuid() -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    inner.euid as isize
}

pub fn sys_getegid() -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    inner.egid as isize
}
pub fn sys_setuid(uid: u32) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let mut proc_inner = proc.inner_exclusive_access();
    proc_inner.uid = uid;
    proc_inner.euid = uid;
    0 
}

pub fn sys_setgid(gid: u32) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let mut proc_inner = proc.inner_exclusive_access();

    proc_inner.gid = gid;
    proc_inner.egid = gid;
    0 
}

pub fn sys_set_tid_address(tidptr: usize) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let mut inner = task.inner_exclusive_access();
    inner.clear_child_tid = tidptr;
    proc.pid.0 as isize 
}
// 假装获取会话 ID 成功，返回 0
pub fn sys_getsid(_pid: usize) -> isize { 
    0 
}
// 假装创建新会话成功，返回新的 SID (这里用 0 代替)
pub fn sys_setsid() -> isize { 
    0 
}
pub fn sys_clock_gettime(_clock_id: usize, tp: *mut TimeSpec) -> isize {
    let total_us = get_time_us();
    let sec = total_us / 1_000_000;
    let nsec = (total_us % 1_000_000) * 1_000;
    if tp as usize == 0 {
        return -14; 
    }
    let token = current_user_token();
    let time_spec = translated_refmut(token, tp);
    time_spec.tv_sec = sec;
    time_spec.tv_nsec = nsec;
    0
}
const TCGETS: u32 = 0x5401;
const TIOCGWINSZ: u32 = 0x5413;
const RTC_RD_TIME: u32 = 0x80247009; // 真实的 RTC 读取指令号

pub fn sys_ioctl(fd: usize, request: usize, argp: usize) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let fd_table = proc.inner_exclusive_access().fd_table.clone();
    // fd合法性检查
    if fd >= fd_table.len() || fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let token = proc.inner_exclusive_access().get_user_token();
    match request as u32 {
        TCGETS => {
            if fd > 2 { return ENOTTY.as_isize(); }
            let mut termios = Termios {
                c_iflag: 0o012402, c_oflag: 0o000005,
                c_cflag: 0o002277, c_lflag: 0o0105011,
                c_line: 0, c_cc: [0; 19],
            };
            termios.c_cc[0] = 3; termios.c_cc[1] = 28;
            termios.c_cc[2] = 127; termios.c_cc[4] = 4;
            if argp != 0 {
                *translated_refmut(token, argp as *mut Termios) = termios;
                0 // 成功
            } else { EFAULT.as_isize() }
        }
        TIOCGWINSZ => {
            if fd > 2 { return ENOTTY.as_isize(); } // ENOTTY
            let winsize = Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
            if argp != 0 {
                *translated_refmut(token, argp as *mut Winsize) = winsize;
                0 // 成功
            } else { EFAULT.as_isize() }
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
                *translated_refmut(token, argp as *mut RtcTime) = rtc_time;
                0
            } else {
                EFAULT.as_isize() // 指针错误
            }
        }
        _ => {
          
            ENOTTY.as_isize()
        }
    }
}

pub fn sys_renameat2(
    _olddirfd: i32, oldpath_ptr: usize,
    _newdirfd: i32, newpath_ptr: usize, _flags: usize
) -> isize {
    let proc = current_task().unwrap().process();
    let token = proc.inner_exclusive_access().get_user_token();


    let old_path = normalize_leading_dot_path(translated_str(token, oldpath_ptr as *const u8));
    let new_path = normalize_leading_dot_path(translated_str(token, newpath_ptr as *const u8));
    
    // 解析父目录和文件名
    let old_parent_path = parent_path(&old_path);
    let old_name = file_name(&old_path);
    let new_parent_path = parent_path(&new_path);
    let new_name = file_name(&new_path);

    let cwd = proc.inner_exclusive_access().cwd.clone();

    // 找到新老父目录的内存 Dentry
    if let (Some(old_parent), Some(new_parent)) = (
        cwd.find_tree(&old_parent_path, true),
        cwd.find_tree(&new_parent_path, true)
    ) {
        // 先从前台 VFS 树上把旧节点摘下来
        let moved_dentry_opt = {
            let mut old_children = old_parent.children.lock(); 
            old_children.remove(&old_name)
        }; 

        if let Some(moved_dentry) = moved_dentry_opt {
            // 先改名
            let disk_success = old_parent.inode.rename_dir_entry(&old_name, &new_name);
            if disk_success {
                // 底层成功了，再把节点以新名字挂到 VFS 树上
                let mut new_children = new_parent.children.lock();
                new_children.insert(new_name.to_string(), moved_dentry);
                return 0;
            } else {
                // 不成功，挂回去
                let mut old_children = old_parent.children.lock();
                old_children.insert(old_name.to_string(), moved_dentry);
                return EIO.as_isize();
            }
        }
    }
    
    -1
}
pub fn sys_getpid() -> isize {
	let task = current_task().unwrap();
    let process = task.process();
	trace!("kernel: sys_getpid pid:{}", process.pid.0);
    process.pid.0 as isize
}
pub fn sys_getppid() -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    match inner.parent.as_ref().and_then(|p| p.upgrade()) {
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
        return -EFAULT.as_isize();
    }
    let token = current_task().unwrap().process().inner_exclusive_access().memory_set.token();
    let sysinfo = translated_refmut(token, sysinfo_ptr as *mut Sysinfo);
    // 临时用硬编码系统信息
    sysinfo.uptime = 1000;              
    sysinfo.loads = [0, 0, 0];          
    sysinfo.totalram = 128 * 1024 * 1024;
    sysinfo.freeram = 64 * 1024 * 1024;  
    sysinfo.sharedram = 0;
    sysinfo.bufferram = 0;
    sysinfo.totalswap = 0;
    sysinfo.freeswap = 0;
    sysinfo.procs = 2;
    sysinfo.pad = 0;
    sysinfo.totalhigh = 0;
    sysinfo.freehigh = 0;
    sysinfo.mem_unit = 1; // 内存单元长度（byte）
    sysinfo._pad = 0;

    // 返回 0 表示获取成功！
    0
}

pub fn sys_uname(uts: *mut UtsName) -> isize {
    let token = current_user_token();
    let uts_name = translated_refmut(token, uts);
    
    // 填充系统信息
    let sysname = b"rCore";
    let nodename = b"rCore-Nodename";
    let release = b"5.10.0-rcore";
    let version = b"v0.1.0";
    let machine = b"riscv64";
    let domainname = b"rcore.os";

    // 辅助函数，安全复制并补 0
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
    
    0
}

pub fn _sys_fork() -> isize {
	let current_task = current_task().unwrap();
    let current_process = current_task.process();
	trace!("kernel:pid[{}] old_sys_fork", current_process.pid.0);
    let proc = current_task.process();
    let (new_proc, new_task) = proc.fork(None, current_task);//此处添加了一个 None 参数
    let new_pid = new_proc.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.set_a0(0);
    // add new task to scheduler
    add_process(new_proc);
    add_task(new_task);
    new_pid as isize
}


// 在当前进程中克隆出一个线程
pub const CLONE_THREAD: usize = 0x00010000;

// 部分实现
pub fn sys_clone(func: usize, stack: usize, flags: usize) -> isize {
    trace!("kernel:pid[{}] sys_clone", current_task().unwrap().process().pid.0);
    if func == 0 && stack == 0 && flags == 0 {
        // 不含参数，直接调用旧的 fork 实现
        return _sys_fork();
    }
    match flags {
        CLONE_THREAD => {
            // 线程克隆
            do_clone_thread(func, stack)
        }
        _ => {
            // 默认按照进程克隆处理
            do_fork(func, stack, flags)
        }
    }
}

pub fn sys_exec(path: *const u8, mut args: *const usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let cwd = task.process().inner_exclusive_access().cwd.clone();
    drop(task);
    let path = normalize_leading_dot_path(translated_str(token, path));
    let mut args_vec: Vec<String> = Vec::new();
    loop {
        let arg_str_ptr = *translated_ref(token, args);
        if arg_str_ptr == 0 {
            break;
        }
        let arg_str = translated_str(token, arg_str_ptr as *const u8);
        debug!("[kernel] sys_exec: arg='{}'", arg_str);
        args_vec.push(arg_str);
        unsafe {
            args = args.add(1);
        }
    }
    trace!("[kernel] sys_exec: before open_file");
    let mut on_main_hart = false;
    if let Some(mut app_inode) = open_file(cwd.clone(), path.as_str(), OpenFlags::RDONLY) {
        debug!("[kernel] sys_exec: after open_file, size={}", app_inode.inode.get_size());
        // initproc和shell在主核上运行
        if app_inode.get_dentry().name.contains("shell") || app_inode.get_dentry().name.contains("init") {
            on_main_hart = true;
        }
        if app_inode.get_dentry().name.ends_with(".sh") {
            let busybox = "/musl/busybox".to_string();
            

            let mut busybox_inode_opt: Option<Arc<OSInode>> = None;
            debug!("[kernel] sys_exec: trying to open busybox at '{}'", busybox);
            if let Some(inode) = open_file(ROOT_DENTRY.clone(), busybox.as_str(), OpenFlags::RDONLY) {
                busybox_inode_opt = Some(inode);
            }

            if busybox_inode_opt.is_none() {
                debug!("[kernel] sys_exec: open busybox failed for script '{}': tried {:?}", path, busybox);
                return ENOENT.as_isize();
            }

            let mut new_args:Vec<String> = Vec::new();
            new_args.push("busybox".to_string());
            new_args.push("sh".to_string());
            if args_vec.is_empty() {
                new_args.push(path.clone());
            }
            for arg in args_vec.iter(){
                new_args.push(arg.clone());
            }
            args_vec = new_args;
            app_inode = busybox_inode_opt.unwrap();
        }

        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        let argc = args_vec.len();
        debug!("[kernel] sys_exec: before task.exec, current working dir={}, path='{}', argc={}, args={:?}", cwd.name, path, argc, args_vec);
        task.process().exec(task, all_data.as_slice(), args_vec, on_main_hart);
        let task = current_task().unwrap();
        let task_inner = task.inner_exclusive_access();
        let trap_cx = task_inner.get_trap_cx();
        /*println!(
            "[kernel][exec-debug] done: hart={}, pid={}, tid={}, task_cx.ra={:#x}, task_cx.sp={:#x}, trap.sepc={:#x}, trap.sp={:#x}, trap.ksp={:#x}, trap_addr={:#x}",
            get_hart_id(),
            task.getpid(),
            task.gettid(),
            task_inner.task_cx.ra,
            task_inner.task_cx.sp,
            trap_cx.get_rt(),
            trap_cx.x[2],
            trap_cx.kernel_sp,
            task_inner.trap_cx_addr
        );*/
        trace!("[kernel] sys_exec: after task.exec");
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        warn!("[kernel] sys_exec: open_file failed");
        ENOENT.as_isize()
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_wait4(pid: isize, exit_code_ptr: *mut i32, options: usize) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    const WNOHANG: usize = 0x1;
    let nohang = (options & WNOHANG) != 0;
    // 开启一个死循环，直到找到僵尸才 return
    loop {
        let mut proc_inner = proc.inner_exclusive_access();
        
        // 1. 检查是否存在符合要求的子进程
        if !proc_inner.children.iter().any(|p| pid == -1 || pid as *const () as usize == p.getpid()) {
            return -1; // 一个孩子都没有，直接返回错误
        }
    
        // 2. 尝试找一个“已经死掉”的僵尸孩子
        let pair = proc_inner.children.iter().enumerate().find(|(_, p)| {
            p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as *const () as usize == p.getpid())
        });
    
        if let Some((idx, _)) = pair {
            // --- A. 找到了僵尸！收尸成功 ---
            let child = proc_inner.children.remove(idx);
            let pid = child.getpid();
            let exit_code = child.inner_exclusive_access().exit_code;
            assert_eq!(Arc::strong_count(&child), 1);
            // 左移 8 位（这里还是要保留的！）
            let status = (exit_code & 0xff) << 8;
            // wait(NULL) is valid: userspace may pass a null status pointer.
            if exit_code_ptr as usize != 0 {
                *translated_refmut(proc_inner.memory_set.token(), exit_code_ptr) = status;
            }
            
            return pid as isize; // 成功返回
        } else {
            if nohang {
                return 0;
            }
            // --- B. 孩子还活着 ---
            // 释放进程锁并阻塞当前任务，等待子进程退出时被唤醒。
            drop(proc_inner);
            crate::process::current_task_to_sleep(proc.wait_queue.lock());
        }
    }
}

pub fn sys_kill(pid: isize, signum: i32) -> isize {
    let current = current_task().unwrap();
    let process = current.process();
    trace!("kernel:pid[{}] sys_kill", process.pid.0);
    drop(process);
    drop(current);
    if let Some(proc) = get_process(pid as usize) {
        if let Some(flag) = SignalFlags::from_bits(1 << signum) {
            // insert the signal if legal
            let mut inner = proc.inner_exclusive_access();
            if inner.signals.contains(flag) {
                return 0;
            }
            inner.signals.insert(flag);
            for task in inner.tasks.iter() {
                let mut task_inner = task.inner_exclusive_access();
                if !task_inner.signal_mask.contains(flag) {
                    task_inner.signals.insert(flag);
                    drop(task_inner);
                    break;
                }
            }
            0
        } else {
            EINVAL.as_isize() // 信号不合法
        }
    } else {
        ESRCH.as_isize() // 进程不存在
    }
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let total_us = get_time_us();
    let sec = total_us / 1_000_000;
    let usec = total_us % 1_000_000;
    // 写入用户传入的结构体
    let token = current_user_token();
    if ts as *const () as usize != 0 {
        let time_val = translated_refmut(token, ts);
        time_val.sec = sec;
        time_val.usec = usec;
    } else {
        return EFAULT.as_isize();
    }
    0
}
pub fn sys_nanosleep(req: *const TimeSpec, _rem: *mut TimeSpec) -> isize {
    let start = get_time_ms();
    // 写入用户传入的结构体
    let token = current_user_token();
    let len = *translated_ref(token, req); 

    let duration_ms = len.tv_sec * 1000 + len.tv_nsec / 1_000_000;
    while get_time_ms() < start + duration_ms {
        suspend_current_and_run_next();// 切换任务除非时间到
    }
    0
}
pub fn sys_mprotect(_start: usize, _len: usize, _prot: usize) -> isize {
    0
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, port: i32, flags: i32, fd: i32, _off: usize) -> isize {
    let mmap_flags = mmap::MMapFlags::from_bits_truncate(flags);
    let mmap_prot = mmap::MMapProt::from_bits_truncate(port);

    // 分配内存并映射
    let ret = match mmap::do_mmap(start, len, mmap_prot , mmap_flags) {
        Ok(addr) => addr,
        Err(_) => {
            //debug!("[kernel] sys_mmap: do_mmap failed for start={:#x}, len={:#x}, prot={:?}, flags={:?}", start, len, mmap_prot, mmap_flags);
            return Errno::ENOMEM.as_isize(); // 内存不足
        }
    };

    // 如果是文件映射（非匿名映射）且 FD 合法，读取内容
    if !mmap_flags.contains(mmap::MMapFlags::MAP_ANONYMOUS) && fd >= 0 { // 先前逻辑反了
        //debug!("[kernel] sys_mmap: file mapping requested for fd={}, start={:#x}, len={:#x}, prot={:?}, flags={:?}", fd, start, len, mmap_prot, mmap_flags);
        let task = current_task().unwrap();
        let process = task.process();
        let token = current_user_token();
        let inner = process.inner_exclusive_access();
            if let Some(file) = &inner.fd_table[fd as usize].file {
                if file.readable() {
                    let file = file.clone();
                    // 释放锁避免阻塞
                    drop(inner);
                    // 构造 UserBuffer，指向刚刚映射出来的用户态虚地址
                    let user_buf = UserBuffer::new(translated_byte_buffer(token, ret as *const u8, len));
                    // 使用 read_at 确保不受 FD 当前 offset 影响，并使用系统调用传入的 _off
                    file.read_at(_off, user_buf);
                }
            }
        }
    //println!("[kernel] sys_mmap: mapped addr={:#x} for start={:#x}, len={:#x}, prot={:?}, flags={:?}", ret, start, len, mmap_prot, mmap_flags);
    ret as isize
    }

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_munmap NOT COMPLITED", process.pid.0);
    if let Ok(_) = mmap::do_munmap(start,len) {
        0
    } else {
        EINVAL.as_isize() // 目标地址不合法
    }
}

/// change data segment size
pub fn sys_brk(addr: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_brk", process.pid.0);
    if let Ok(res) = mmap::do_brk(addr){
        res as isize
    } else {
        current_task().unwrap().process().inner_exclusive_access().program_brk as isize
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(_path: *const u8) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_spawn NOT IMPLEMENTED", process.pid.0);
    -1
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_set_priority NOT IMPLEMENTED", process.pid.0);
    -1
}

#[allow(dead_code)]
pub fn sys_sigprocmask(mask: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_sigprocmask", process.pid.0);
    drop(process);
    drop(task);
    if let Some(task) = current_task() {
        let mut inner = task.inner_exclusive_access();
        let old_mask = inner.signal_mask;
        if let Some(flag) = SignalFlags::from_bits(mask) {
            inner.signal_mask = flag;
            old_mask.bits() as isize
        } else {
            -1
        }
    } else {
        -1
    }}

pub fn sys_sigreturn() -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_sigreturn", process.pid.0);
    drop(process);
    drop(task);
    if let Some(task) = current_task() {
        let mut inner = task.inner_exclusive_access();
        inner.handling_sig = -1;
        // restore the trap context
        let trap_ctx = inner.get_trap_cx();
        *trap_ctx = inner.trap_ctx_backup.unwrap();
        // Here we return the value of a0 in the trap_ctx,
        // otherwise it will be overwritten after we trap
        // back to the original execution of the application.
        trap_ctx.get_a0() as isize
    } else {
        ESRCH.as_isize() // 没有当前任务
    }
}

const SIG_BLOCK: usize = 0;
const SIG_UNBLOCK: usize = 1;
const SIG_SETMASK: usize = 2;

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
    trace!("kernel:pid[{}] sys_sigaction", current_task().unwrap().process().pid.0);
    if sigsetsize < core::mem::size_of::<u32>() {
        return EINVAL.as_isize();
    }
    if signum <= 0 || signum as usize > MAX_SIG {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    trace!("kernel:pid[{}] sys_sigaction", proc.pid.0);
    let mut inner = proc.inner_exclusive_access();
    let token = inner.memory_set.token();
    if signum as *const () as usize > MAX_SIG {
        return -1;
    }
    if let Some(flag) = SignalFlags::from_bits(1 << signum) {
        if check_sigaction_error(flag) {
            return -1;
        }
    if !old_action.is_null() {
        let prev_action = inner.signal_actions.table[signum as *const () as usize];
        *translated_refmut(token, old_action) = prev_action;
    }
    if action.is_null() {
        println!("action is null");
        return 0;
    }
        inner.signal_actions.table[signum as *const () as usize] = *translated_ref(token, action);
        0
    } else {
        ESRCH.as_isize()
    }
}

pub fn sys_times(tms_ptr: *mut usize) -> isize {
    let token = current_user_token();
    // 暂时伪实现，写0
    let tms_val = Tms {
        tms_utime: 0,
        tms_stime: 0,
        tms_cutime: 0,
        tms_cstime: 0,
    };
    *translated_refmut(token, tms_ptr as *mut Tms) = tms_val;
    let current_ms = get_time_ms();
    current_ms as isize
}

pub fn sys_getrandom(buf: *mut u8, len: usize, _flags: u32) -> isize {
    let token = current_user_token();
    let mut user_buf = translated_byte_buffer(token, buf, len);
    for (i, buf) in user_buf.iter_mut().enumerate() {
        let seed = get_timer_ticks() + buf.as_ptr() as usize + i;
        // 类LGC算法，时间滴答作种
        buf[0] = (((25214903917usize * seed) & ((1 << 48) - 1)) >> (8 * (i % 6))) as u8;
    }
    len as isize
}

pub fn sys_robust_list() -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_robust_list NOT IMPLEMENTED", process.pid.0);
    // 目前还没有实现多线程（每个任务是独立的内存空间)，不需要管理锁，伪实现不会导致死锁
    0
}

pub fn sys_resq() -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_resq NOT IMPLEMENTED", process.pid.0);
    // 未实现多线程，这里伪实现
    0
}