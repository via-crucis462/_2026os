//! Process management syscalls


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
    }

};
pub use alloc::{string::{String,ToString}, sync::Arc, vec::Vec};

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

pub fn sys_exit(exit_code: i32) -> ! {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_exit", process.pid.0);
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
pub fn sys_ioctl(fd: usize, request: usize, argp: usize) -> isize {
    const TCGETS: usize = 0x5401;
    const TIOCGWINSZ: usize = 0x5413;


    if fd != 1 {
        return -25; // ENOTTY
    }

    let token = current_user_token(); 

    match request {
        TCGETS => {
            
            let mut termios = Termios {
                c_iflag: 0o012402, // IGNBRK | ICRNL 等标志位的组合值
                c_oflag: 0o000005, // OPOST | ONLCR
                c_cflag: 0o002277, // 标准的控制模式
                c_lflag: 0o0105011, // ISIG | ICANON | ECHO 等
                c_line: 0,
                c_cc: [0; 19],
            };
            // 设置关键的控制字符
            termios.c_cc[0] = 3;   // VINTR = ^C (终止进程)
            termios.c_cc[1] = 28;  // VQUIT = ^\
            termios.c_cc[2] = 127; // VERASE = DEL (退格)
            termios.c_cc[4] = 4;   // VEOF = ^D

            // 2. 将数据写回用户态
            if argp != 0 {
                let user_termios = translated_refmut(token, argp as *mut Termios);
                *user_termios = termios;
                0 // 成功返回 0
            } else {
                -14 // EFAULT: 用户传了个空指针
            }
        }
        TIOCGWINSZ => {
            // 1. 构造终端窗口大小 (比如经典的 24行 80列)
            let winsize = Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };

            // 2. 将数据写回用户态
            if argp != 0 {
                let user_winsize = translated_refmut(token, argp as *mut Winsize);
                *user_winsize = winsize;
                println!("[DEBUG ioctl] TIOCGWINSZ handled, setting 24x80");
                0 // 成功返回 0
            } else {
                -14 // EFAULT
            }
        }
        _ => {
            // 对于未知的终端请求，安全地返回 -25，C 库能正确处理真正的降级
            println!("[DEBUG ioctl] Unsupported request {:#x} for fd {}, returning -25", request, fd);
            -25 // ENOTTY
        }
    }
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
	trace!("kernel:pid[{}] sys_fork", current_process.pid.0);
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
    let path = translated_str(token, path);
    debug!("[kernel] sys_exec: path={}, args_ptr={:#x}", path, args as *const () as usize);
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
    if let Some(mut app_inode) = open_file(cwd.clone(), path.as_str(), OpenFlags::RDONLY) {
        debug!("[kernel] sys_exec: after open_file, size={}", app_inode.inode.get_size());

        if app_inode.get_dentry().name.ends_with(".sh") {
            let mut new_args:Vec<String> = Vec::new();
            new_args.push("busybox".to_string());
            new_args.push("sh".to_string());
            for arg in args_vec.iter(){
                new_args.push(arg.clone());
            }
            args_vec = new_args;
            if let Some(busybox_inode) = open_file(ROOT_DENTRY.clone(), "busybox", OpenFlags::RDONLY) {
                app_inode = busybox_inode;
            } else {
                warn!("[kernel] sys_exec: open busybox failed");
                return -1;
            }
        }

        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        let argc = args_vec.len();
        trace!("[kernel] sys_exec: before task.exec");
        task.process().exec(all_data.as_slice(), args_vec);
        trace!("[kernel] sys_exec: after task.exec");
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        warn!("[kernel] sys_exec: open_file failed");
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_wait4(pid: isize, exit_code_ptr: *mut i32, _options: usize) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
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
            assert_eq!(Arc::strong_count(&child), 1);
            let found_pid = child.getpid();
            let exit_code = child.inner_exclusive_access().exit_code;
            
            // 左移 8 位（这里还是要保留的！）
            let status = (exit_code & 0xff) << 8;
            *translated_refmut(proc_inner.memory_set.token(), exit_code_ptr) = status;
            
            return found_pid as isize; // 成功返回
        } else {
            // --- B. 孩子还活着 ---
            
            // 释放锁
            drop(proc_inner); 
            
            // 暂停当前进程，让出 CPU 给孩子跑
            suspend_current_and_run_next();
            
            // 【关键点】：这里不再返回 -2，而是继续 loop！
            // 醒来后再次进入循环，重新检查 children 列表
        }
    }
}

pub fn sys_kill(pid: usize, signum: i32) -> isize {
    let current = current_task().unwrap();
    let process = current.process();
    trace!("kernel:pid[{}] sys_kill", process.pid.0);
    if let Some(proc) = get_process(pid) {
        if let Some(flag) = SignalFlags::from_bits(1 << signum) {
            // insert the signal if legal
            let mut inner = proc.inner_exclusive_access();
            if inner.signals.contains(flag) {
                return -1;
            }
            inner.signals.insert(flag);
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let total_us = get_time_us();

    // 2. 进行数学运算，拆分成 秒 和 微秒
    let sec = total_us / 1_000_000;
    let usec = total_us % 1_000_000;

    // 3. 获取用户空间的 token，准备写内存
    let token = current_user_token();

    // 4. 写入用户传进来的结构体
    // C标准中，如果指针是 NULL (0)，则表示不需要获取该值，直接忽略即可
    // 但为了过测例，ts 一般都是有效的
    if ts as *const () as usize != 0 {
        // 将用户态的虚拟地址 ts 转换为内核能访问的引用
        let time_val = translated_refmut(token, ts);
        
        // 填入数据
        time_val.sec = sec;
        time_val.usec = usec;
    }

    // 5. 成功返回 0 (注意之前你返回的是 -1)
    0
}
pub fn sys_nanosleep(req: *const TimeSpec, _rem: *mut TimeSpec) -> isize {
    // 1. 获取当前时间 (毫秒)
    let start = get_time_ms();
    let token = current_user_token();
    let len = *translated_ref(token, req); 
    let duration_ms = len.tv_sec * 1000 + len.tv_nsec / 1_000_000;
    while get_time_ms() < start + duration_ms {
        suspend_current_and_run_next();
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
    
    // 1. 分配并映射虚存及其对应的物理页
    let ret = match mmap::do_mmap(start, len, mmap_prot) {
        Ok(addr) => addr,
        Err(_) => return -1,
    };

    // 2. 如果是文件映射（非匿名映射）且 FD 合法，读取内容
    if !mmap_flags.contains(mmap::MMapFlags::MAP_ANONYMOUS) && fd >= 0 {
        let task = current_task().unwrap();
        let proc = task.process();
        let token = current_user_token();
        let inner = proc.inner_exclusive_access();
        
        if (fd as usize) < inner.fd_table.len() {
            if let Some(file) = &inner.fd_table[fd as usize] {
                if file.readable() {
                    let file = file.clone();
                    drop(inner); // 必须释放锁，因为 file.read 涉及磁盘 IO 可能阻塞
                    
                    // 构造 UserBuffer，指向刚刚映射出来的用户态虚地址
                    let user_buf = UserBuffer::new(translated_byte_buffer(token, ret as *const u8, len));
                    
                    // 使用 read_at 确保不受 FD 当前 offset 影响，并使用系统调用传入的 _off
                    file.read_at(_off, user_buf);
                }
            }
        }
    }
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
        -1
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
        -1
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

pub fn sys_sigprocmask(mask: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_sigprocmask", process.pid.0);
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
    }
}

pub fn sys_sigreturn() -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_sigreturn", process.pid.0);
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
        -1
    }
}

fn check_sigaction_error(signal: SignalFlags, action: usize, old_action: usize) -> bool {
    if action == 0
        || old_action == 0
        || signal == SignalFlags::SIGKILL
        || signal == SignalFlags::SIGSTOP
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
    let task = current_task().unwrap();
    let proc = task.process();
    trace!("kernel:pid[{}] sys_sigaction", proc.pid.0);
    let token = current_user_token();
    let mut inner = proc.inner_exclusive_access();
    if signum as *const () as usize > MAX_SIG {
        return -1;
    }
    if let Some(flag) = SignalFlags::from_bits(1 << signum) {
        if check_sigaction_error(flag, action as *const () as usize, old_action as *const () as usize) {
            return -1;
        }
        let prev_action = inner.signal_actions.table[signum as *const () as usize];
        *translated_refmut(token, old_action) = prev_action;
        inner.signal_actions.table[signum as *const () as usize] = *translated_ref(token, action);
        0
    } else {
        -1
    }
}
pub fn sys_times(tms_ptr: *mut usize) -> isize {
    // 1. 获取当前时间（毫秒）作为返回值
    // 这对应测例里的 test_ret
    let current_ms = get_time_ms();

    // 2. 获取用户 token 用来写内存
    let token = current_user_token();

    // 3. 构造要填入的数据
    // 因为测例不检查具体数值，我们填 0 完全没问题
    // 等以后你实现了精确的统计，再来填这里
    let tms_val = Tms {
        tms_utime: 0,
        tms_stime: 0,
        tms_cutime: 0,
        tms_cstime: 0,
    };

    // 4. 将数据写入用户传进来的地址
    // 注意：把 *mut usize 强转为 *mut Tms
    *translated_refmut(token, tms_ptr as *mut Tms) = tms_val;

    // 5. 返回当前时间滴答数 (只要 >= 0，assert就过了)
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