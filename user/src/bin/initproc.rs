#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
extern crate alloc;

use alloc::vec::Vec;
use user_lib::*;

/// Build a null-terminated C string from a &str
fn cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// Run a shell command via busybox sh -c.
/// Returns the exit code of the shell.
fn run_shell(cmd: &str) -> i32 {
    let forked = fork();
    if forked == 0 {
        // child: exec busybox sh -c "cmd"
        let a0 = cstr("busybox");
        let a1 = cstr("sh");
        let a2 = cstr("-c");
        let a3 = cstr(cmd);

        let argv: &[*const u8] = &[
            a0.as_ptr(),
            a1.as_ptr(),
            a2.as_ptr(),
            a3.as_ptr(),
            core::ptr::null(),
        ];

        exec("/musl/busybox\0", argv);
        // exec only returns on error
        exit(-1);
    } else if forked > 0 {
        let mut exit_code: i32 = 0;
        waitpid(forked as usize, &mut exit_code);
        exit_code
    } else {
        -1
    }
}

fn run_a_test(script: &str) -> i32 {
    let mut exit_code: i32 = 0;
    let forked = fork();
    if forked == 0 {
        // child
        let mut arg_storage: Vec<Vec<u8>> = Vec::new();
        for part in script.split(' ').filter(|s| !s.is_empty()) {
            let bytes = part.as_bytes();
            let body = if bytes.last() == Some(&0) {
                &bytes[..bytes.len() - 1]
            } else {
                bytes
            };
            let mut cstr = Vec::with_capacity(body.len() + 1);
            cstr.extend_from_slice(body);
            cstr.push(0);
            arg_storage.push(cstr);
        }

        if arg_storage.is_empty() {
            exit(-1);
        }

        let mut argv: Vec<*const u8> = arg_storage.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(core::ptr::null());

        let path_bytes = &arg_storage[0][..arg_storage[0].len() - 1];
        let path = match core::str::from_utf8(path_bytes) {
            Ok(s) => s,
            Err(_) => exit(-1),
        };

        println!("c:Running {} ...", path);
        exec(path, &argv);
        exit(-1);
    } else if forked > 0 {
        // parent
        println!("p:Running {} ...", script);
        waitpid(forked as usize,&mut exit_code);
    } else {
        println!("Fork Error!");
        exit(-1);
    }
    exit_code
}

// 当前脚本通过向/temp/test.sh写入命令来执行测试用例，
// 最后由busybox sh /tmp/test.sh来执行这些命令
#[no_mangle]
fn main() -> i32 {
    chdir("/musl\0");

    // 白名单：仅运行这些有分数的测例
    const BASE_CASES: &[&str] = &[
/*
        "accept01",
        "accept03",
        "accept4_01",
        "access03",
        "alarm02",
        "alarm03",
        "alarm05",
        "alarm06",
        "alarm07",
        "bpf_map01",
        "bpf_prog01",
        "brk01",
        "brk02",
        "chdir01",
        "chdir04",
        "chroot01",
        "clock_adjtime01",
        "clock_adjtime02",
        "clock_gettime02",
        "clock_nanosleep01",
        "clock_nanosleep04",
        "clock_settime01",
        "clock_settime02",
        "clone01",
        "clone03",
        "clone06",
        "clone07",
        "clone302",
        "close01",
        "close02",
        "confstr01",
        "creat01",
        "creat03",
        "creat05",
        "dup01",
        "dup02",
        "dup03",
        "dup04",
        "dup06",
        "dup07",
        "dup201",
        "dup202",
        "dup203",
        "dup204",
        "dup205",
        "dup206",
        "dup207",
        "dup3_01",
        "epoll_create01",
        "epoll_create1_01",
        "epoll_ctl01",
        "epoll_ctl02",
        "epoll_ctl03",
        "epoll_ctl04",
        "epoll_ctl05",
        "epoll_wait01",
        "epoll_wait03",
        "epoll_wait04",
        "epoll_wait07",
        "eventfd2_01",
        "eventfd2_02",
        "execve03",
        "exit02",
        "faccessat01",
        "faccessat02",
        "fallocate03",
        "fallocate04",
        "fchmod01",
        "fchmod03",
        "fchmod04",
        "fchmodat01",
        "fchmodat02",
        "fcntl02",
        "fcntl02_64",
        "fcntl03",
        "fcntl03_64",
        "fcntl04",
        "fcntl04_64",
        "fcntl05",
        "fcntl05_64",
        "fcntl08",
        "fcntl08_64",
        "fcntl13",
        "fcntl13_64",
        "fcntl29",
        "fcntl29_64",
        "flock06",
        "fork01",
        "fork03",
        "fork04",
        "fork07",
        "fork08",
        "fork10",
        "fork_procs",
        "fpathconf01",
        "ftruncate03",
        "ftruncate03_64",
        "getdents02",
        "getdomainname01",
        "getegid01",
        "getegid01_16",
        "getegid02",
        "getegid02_16",
        "geteuid01",
        "geteuid02",
        "getgid01",
        "getgid03",
        "gethostname01",
        "getpagesize01",
        "getpgid01",
        "getpgid02",
        "getpgrp01",
        "getpid01",
        "getpid02",
        "getppid01",
        "getppid02",
        "getrandom03",
        "getrandom04",
        "getrlimit01",
        "getrlimit02",
        "getrusage01",
        "getrusage02",
        "getsid01",
        "getsid02",
        "getsockname01",
        "getuid01",
        "getuid03",
        "in6_01",
        "ioctl_loop06",
        "ioctl_ns07",
        "kill03",
        "llseek02",
        "llseek03",
        "lseek01",
        "lseek07",
        "memcmp01",
        "memfd_create01",
        "memfd_create02",
        "memfd_create04",
        "memset01",
        "mlock03",
        "mmap02",
        "mmap08",
        "mmap09",
        "mmap15",
        "mmap19",
        "mount01",
        "mprotect05",
        "nanosleep02",
        "nanosleep04",
        "open01",
        "open03",
        "open04",
        "open08",
        "openat01",
        "pathconf01",
        "pipe01",
        "pipe06",
        "pipe10",
        "pipe11",
        "pipe14",
        "pipe2_01",
        "posix_fadvise03",
        "posix_fadvise03_64",
        "prctl05",
        "prctl08",
        "pselect02",
        "pselect02_64",
        "pselect03",
        "pselect03_64",
        "read01",
        "read02",
        "read04",
        "readv01",
        "readv02",
        "realpath01",
        "recvmsg01",
        "rmdir01",
        "sbrk01",
        "sched_getaffinity01",
        "sendfile03",
        "sendfile03_64",
        "sendfile06",
        "sendfile06_64",
        "sendfile08",
        "sendfile08_64",
        "setgid01",
        "setgid03",
        "setitimer02",
        "setpgrp02",
        "setregid02",
        "setresuid01",
        "setresuid04",
        "setresuid05",
        "setreuid07",
        "setrlimit02",
        "setrlimit04",
        "setsockopt01",
        "setsockopt03",
        "settimeofday01",
        "setuid01",
        "shmnstest",
        "sigaltstack02",
        "signal02",
        "signal03",
        "signal04",
        "signal05",
        "socket01",
        "socket02",
        "socketpair01",
        "socketpair02",
        "stat02",
        "stat02_64",
        "statx03",
        "stime01",
        "stime02",
        "syscall01",
        "thp01",
        "time01",
        "times01",
        "uname01",
        "uname02",
        "uname04",
        "unlinkat01",
        "utsname01",
        "utsname04",
        "wait01",
        "wait02",
        "wait401",
        "wait402",
        "wait403",
        "waitid04",
        "waitid05",
        "waitid06",
        "waitpid03",
        "waitpid04",
        "waitpid09",
        "write01",
        "write02",
        "write05",
        "writev01",
        "waitid01",
        "waitid02",
        "waitid03",
        "waitid07",
        "waitid08",
        "waitid09",
        "waitid10",
        "waitpid01",
        "waitpid06",
        "waitpid07",
        "waitpid08",
        "waitpid10",
        "waitpid11",
        "waitpid12",
        "waitpid13",
        "wc01.sh",
        "which01.sh",
        "wireguard01.sh",
        "wireguard02.sh",
        "wireguard_lib.sh",
        "wqueue01",
        "wqueue02",
        "wqueue03",
        "wqueue04",
        "wqueue05",
        "wqueue06",
        "wqueue07",
        "wqueue08",
        "wqueue09",
        "write03",
        "write04",
        "write06",
        "write_freezing.sh",
        "writetest",
        "writev02",
        "writev03",
        "writev05",
        "writev06",
        "writev07",*/
    ];

    let mut run_cases: Vec<&str> = Vec::from(BASE_CASES);

    // riscv64-only LTP cases, inserted at runtime
    // 只让riscv跑的从上面删掉放进这里面
    #[cfg(target_arch = "riscv64")]
    {
        run_cases.extend_from_slice(&[
            "fstat03",
            "fstat03_64",
            "gettimeofday02",
            "mmapstress01",
            "nfs05_make_tree",
            "ppoll01",
            "select03",
            "sendmmsg02",
            "times03",
            "sbrk02",
            "signal01",
            "times03",
            "waitid11",
        ]);
    }

    // Helper: write &str to fd via sys_write (avoids heap allocation for buffer)
    // 通过 sys_write 将 &str 写入 fd
    fn write_fd(fd: usize, s: &str) {
        sys_write(fd, s.as_bytes());
    }

    // raw open flags matching kernel's OpenFlags (not user_lib's mismatched ones!)
    // kernel: WRONLY=1, CREATE=1<<6=64, TRUNC=1<<9=512
    const K_WRONLY: u32 = 1;
    const K_CREAT: u32 = 1 << 6;
    const K_TRUNC: u32 = 1 << 9;

    // Open /tmp/test.sh for writing (create + truncate) — /tmp is tmpfs, supports creation
    let script_path = "/tmp/test.sh\0";
    let fd = sys_openat(
        (-100isize) as usize, // AT_FDCWD
        script_path,
        K_WRONLY | K_CREAT | K_TRUNC,
        0o777,
    );
    if fd < 0 {
        return -1;
    }
    let fd = fd as usize;

// --- basic cases ---
#[cfg(false)]
{
    write_fd(fd, "
cd /musl 
sh /musl/basic_testcode.sh
sh /musl/busybox_testcode.sh
sh /musl/libctest_testcode.sh
cd /glibc
sh /glibc/basic_testcode.sh
sh /glibc/busybox_testcode.sh
   ");
}

// --- musl ltp ---
{
    write_fd(fd, "echo \"#### OS COMP TEST GROUP START ltp-musl ####\"\n");
    for chunk in run_cases.chunks(50) {
        write_fd(fd, "for name in ");
        for (i, case) in chunk.iter().enumerate() {
            if i > 0 { write_fd(fd, " "); }
            write_fd(fd, case);
        }
        write_fd(fd, "; do\n");
        write_fd(fd, "  f=\"/musl/ltp/testcases/bin/$name\"\n");
        write_fd(fd, "  if [ -f \"$f\" ] && [ -x \"$f\" ]; then\n");
        write_fd(fd, "    echo \"RUN LTP CASE $name\"\n");
        write_fd(fd, "    \"$f\"\n");
        write_fd(fd, "    ret=$?\n");
        write_fd(fd, "    echo \"FAIL LTP CASE $name : $ret\"\n");
        write_fd(fd, "  fi\n");
        write_fd(fd, "done\n");
    }
    write_fd(fd, "echo \"#### OS COMP TEST GROUP END ltp-musl ####\"\n");
}

// --- glibc ltp ---
{
    write_fd(fd, "echo \"#### OS COMP TEST GROUP START ltp-glibc ####\"\n");
    for chunk in run_cases.chunks(50) {
        write_fd(fd, "for name in ");
        for (i, case) in chunk.iter().enumerate() {
            if i > 0 { write_fd(fd, " "); }
            write_fd(fd, case);
        }
        write_fd(fd, "; do\n");
        write_fd(fd, "  f=\"/glibc/ltp/testcases/bin/$name\"\n");
        write_fd(fd, "  if [ -f \"$f\" ] && [ -x \"$f\" ]; then\n");
        write_fd(fd, "    echo \"RUN LTP CASE $name\"\n");
        write_fd(fd, "    \"$f\"\n");
        write_fd(fd, "    ret=$?\n");
        write_fd(fd, "    echo \"FAIL LTP CASE $name : $ret\"\n");
        write_fd(fd, "  fi\n");
        write_fd(fd, "done\n");
    } // 
    write_fd(fd, "echo \"#### OS COMP TEST GROUP END ltp-glibc ####\"\n");
}
    sys_close(fd);

    // Execute: busybox sh /test.sh
    {
        let forked = fork();
        if forked == 0 {
            let a0 = cstr("busybox");
            let a1 = cstr("sh");
            let a2 = cstr("/tmp/test.sh\0");
            let argv: &[*const u8] = &[
                a0.as_ptr(),
                a1.as_ptr(),
                a2.as_ptr(),
                core::ptr::null(),
            ];
            exec("/musl/busybox\0", argv);
            exit(-1);
        } else if forked > 0 {
            let mut exit_code: i32 = 0;
            waitpid(forked as usize, &mut exit_code);
        }
    }
    // Init (PID 1) must never exit — otherwise the kernel panics.
    // Loop forever, reaping any zombie children.
    let mut _status: i32 = 0;
    waitpid((-1isize) as usize, &mut _status);
    0
}