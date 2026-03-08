//! Implementation of syscalls
//!
//! The single entry point to all system calls, [`syscall()`], is called
//! whenever userspace wishes to perform a system call using the `ecall`
//! instruction. In this case, the processor raises an 'Environment call from
//! U-mode' exception, which is handled as one of the cases in
//! [`crate::trap::trap_handler`].
//!
//! For clarity, each single syscall is implemented as its own function, named
//! `sys_` then the name of the syscall. You can find functions like this in
//! submodules, and you should also implement syscalls this way.

/// dup syscall
const SYSCALL_DUP: usize = 23;
/// dup2 syscall
const SYSCALL_DUP2: usize = 24;
const SYSCALL_FCNTL: usize = 25;    
const SYSCALL_IOCTL: usize = 29;
/// unlinkat syscall
const SYSCALL_UNLINKAT: usize = 35;
/// linkat syscall
const SYSCALL_LINKAT: usize = 37;
/// openat syscall
const SYSCALL_OPENAT: usize = 56;
/// close syscall
const SYSCALL_CLOSE: usize = 57;
/// pipe syscall
const SYSCALL_PIPE: usize = 59;
/// read syscall
const SYSCALL_READ: usize = 63;
/// write syscall
const SYSCALL_WRITE: usize = 64;
const SYSCALL_PREAD64: usize = 67;
const SYSCALL_READLINKAT: usize = 78;
const SYSCALL_FSTATAT: usize = 79;
/// fstat syscall
const SYSCALL_FSTAT: usize = 80;
/// exit syscall
const SYSCALL_EXIT: usize = 93;
const SYSCALL_EXIT_GROUP: usize = 94;
const SYSCALL_SET_TID_ADDRESS: usize = 96;
const SYSCALL_SET_ROBUST_LIST: usize = 99;// RISCV
const SYSCALL_SLEEP:usize =101;
/// yield syscall
const SYSCALL_YIELD: usize = 124;
/// kill syscall
const SYSCALL_KILL: usize = 129;
/// sigaction syscall
const SYSCALL_CLOCK_GETTIME: usize = 113;
const SYSCALL_SIGACTION: usize = 134;
/// sigprocmask syscall
const SYSCALL_SIGPROCMASK: usize = 135;
/// sigreturn syscall
const SYSCALL_SIGRETURN: usize = 139;
/// setpriority syscall
const SYSCALL_SET_PRIORITY: usize = 140;
const SYSCALL_SETGID: usize = 144;
const SYSCALL_SETUID: usize = 146;
const SYSCALL_TIMES: usize = 153;
const SYSCALL_SETPGID: usize = 154;
const SYSCALL_GETPGID: usize = 155;
const SYSCALL_GETSID:  usize = 156;
const SYSCALL_SETSID:  usize = 157;
const SYSCALL_UNAME: usize = 160;
const SYSCALL_PRCTL: usize = 167;
const SYSCALL_GET_TIME: usize = 169;
/// getpid syscall
const SYSCALL_GETPID: usize = 172;
const SYSCALL_GETPPID: usize = 173;
const SYSCALL_GETUID: usize = 174;
const SYSCALL_GETEUID: usize = 175;
const SYSCALL_GETGID: usize = 176;
const SYSCALL_GETEGID: usize = 177;
const SYSCALL_GETTID: usize = 178;
/// brk syscall
const SYSCALL_BRK: usize = 214;
/// munmap syscall
const SYSCALL_MUNMAP: usize = 215;
/// clone syscall
const SYSCALL_CLONE: usize = 220;
/// exec syscall
const SYSCALL_EXEC: usize = 221;
/// mmap syscall
const SYSCALL_MMAP: usize = 222;
const SYSCALL_MPROTECT: usize = 226;
/// waitpid syscall
const SYSCALL_WAIT4: usize = 260;
const SYSCALL_WAITPID: usize = 261;
/// statx syscall
const SYSCALL_STATX: usize = 291;
/// spawn syscall
const SYSCALL_SPAWN: usize = 400;
/// mkdir syscall
const SYSCALL_MKDIR: usize = 34;
/// getdents syscall
const SYSCALL_GETDENTS: usize = 61;
/// getcwd syscall
const SYSCALL_GETCWD: usize = 17;
/// chdir syscall
const SYSCALL_CHDIR: usize = 49;
/// mount syscall
const SYSCALL_MOUNT: usize = 40;
/// umount syscall
const SYSCALL_UMOUNT: usize = 39;
/// random syscall
const SYSCALL_GETRANDOM: usize = 278;
/// resq
const SYSCALL_RESQ: usize = 293;
/// accessat syscall
const SYSCALL_ACCESSAT: usize = 48;
mod fs;
mod process;
mod prctl;

use fs::*;
use process::*;
use prctl::*;

use crate::{fs::Stat, task::SignalAction};

#[no_mangle]
/// handle syscall exception with `syscall_id` and other arguments

pub fn syscall(syscall_id: usize, args: [usize; 6]) -> isize {
    /*if syscall_id != 64 && syscall_id != 63 {
        println!("[Syscall Trace] ID: {}, args: {:#x?}", syscall_id, args);
    }*/
    //info!("[kernel] syscall: id={}, args={:?}", syscall_id, args);
    match syscall_id {
        SYSCALL_DUP => sys_dup(args[0]),
        SYSCALL_DUP2 => sys_dup2(args[0], args[1]),
        SYSCALL_OPENAT => sys_openat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32),
        SYSCALL_CLOSE => sys_close(args[0]),
        SYSCALL_ACCESSAT => sys_accessat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32),
        SYSCALL_PIPE => sys_pipe(args[0] as *mut u32),
        SYSCALL_LINKAT => sys_linkat(args[1] as *const u8, args[3] as *const u8),
        SYSCALL_UNLINKAT => sys_unlinkat(args[1] as *const u8),
        SYSCALL_READ => sys_read(args[0], args[1] as *const u8, args[2]),
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_FSTAT => sys_fstat(args[0], args[1] as *mut Stat),
        SYSCALL_EXIT => sys_exit(args[0] as i32),
        SYSCALL_EXIT_GROUP => sys_exit(args[0] as i32),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_KILL => sys_kill(args[0], args[1] as i32),
        SYSCALL_SIGACTION => sys_sigaction(
            args[0] as i32,
            args[1] as *const SignalAction,
            args[2] as *mut SignalAction,
        ),
        SYSCALL_SLEEP => sys_nanosleep(args[0] as *const TimeSpec, args[1] as *mut TimeSpec),
        SYSCALL_SIGPROCMASK => sys_sigprocmask(args[0] as u32),
        SYSCALL_SIGRETURN => sys_sigreturn(),
        SYSCALL_CLOCK_GETTIME => sys_clock_gettime(args[0], args[1]as *mut _),
        SYSCALL_SET_TID_ADDRESS => sys_set_tid_address(args[0]),
        SYSCALL_SETUID => sys_setuid(args[0] as u32),
        SYSCALL_SETGID => sys_setgid(args[0] as u32),
        SYSCALL_GETPID => sys_getpid(),
        SYSCALL_GETPPID => sys_getppid(),
        SYSCALL_GETUID => sys_getuid(),
        SYSCALL_GETEUID => sys_geteuid(),
        SYSCALL_GETGID => sys_getgid(),
        SYSCALL_GETEGID => sys_getegid(),
        SYSCALL_SETPGID => sys_setpgid(args[0], args[1]),
        SYSCALL_GETPGID => sys_getpgid(args[0]),
        SYSCALL_GETSID => sys_getsid(args[0]),
        SYSCALL_SETSID => sys_setsid(),
        SYSCALL_GETTID => sys_gettid(),
        SYSCALL_CLONE => sys_clone(args[0], args[1], args[2]),
        SYSCALL_EXEC => sys_exec(args[0] as *const u8, args[1] as *const usize),
        SYSCALL_WAIT4 | SYSCALL_WAITPID => sys_wait4(args[0] as isize, args[1] as *mut i32, args[2]),//注意：为了跑通脚本，暂时将waitpid和wait4合并了
        SYSCALL_GET_TIME => sys_get_time(args[0] as *mut TimeVal, args[1]),
        SYSCALL_MMAP => sys_mmap(
            args[0], args[1], args[2] as i32, 
            args[3] as i32, args[4] as i32, args[5]
        ),
        SYSCALL_FCNTL => sys_fcntl(args[0], args[1], args[2]),
        SYSCALL_IOCTL => sys_ioctl(args[0], args[1], args[2]),
        SYSCALL_MPROTECT => sys_mprotect(args[0], args[1], args[2]),
        SYSCALL_READLINKAT => sys_readlinkat(
        args[0] as isize, 
        args[1] as *const u8, 
        args[2] as *mut u8, 
        args[3]
        ),
        SYSCALL_MUNMAP => sys_munmap(args[0], args[1]),
        SYSCALL_BRK => sys_brk(args[0] as *const () as usize),
        SYSCALL_UNAME => sys_uname(args[0] as *mut UtsName),
        SYSCALL_SPAWN => sys_spawn(args[0] as *const u8),
        SYSCALL_SET_PRIORITY => sys_set_priority(args[0] as isize),
        SYSCALL_TIMES => sys_times(args[0] as *mut usize),
        SYSCALL_MKDIR => sys_mkdir(args[1] as *const u8, args[2] as u32),
        SYSCALL_GETDENTS => sys_getdents(args[0], args[1] as *mut u8, args[2]),
        SYSCALL_GETCWD => sys_getcwd(args[0] as *mut u8, args[1]),
        SYSCALL_CHDIR => sys_chdir(args[0] as *const u8),
        SYSCALL_MOUNT => sys_mount(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3] as u32),
        SYSCALL_UMOUNT => sys_umount(args[0] as *const u8),
        SYSCALL_STATX => sys_statx(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32, args[4] as *mut Statx),
        SYSCALL_GETRANDOM => sys_getrandom(args[0] as *mut u8, args[1], args[2] as u32),
        SYSCALL_PRCTL => sys_prctl(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_SET_ROBUST_LIST => sys_robust_list(),
        SYSCALL_RESQ => sys_resq(),
        SYSCALL_FSTATAT => sys_fstatat(args[0] as isize,args[1] as *const u8, args[2] as *mut Stat),
        SYSCALL_PREAD64 => sys_pread64(args[0], args[1] as *mut u8, args[2], args[3] as usize),
        _ =>  panic!("Unsupported syscall_id: {}", syscall_id),
    }
}
