#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
extern crate alloc;

use alloc::vec::Vec;
use user_lib::*;

// raw open flags matching kernel's OpenFlags (not user_lib's mismatched ones!)
// kernel: WRONLY=1, CREATE=1<<6=64, TRUNC=1<<9=512
const K_WRONLY: u32 = 1;
const K_CREAT: u32 = 1 << 6;
const K_TRUNC: u32 = 1 << 9;

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
//        "shm_test",
/*      "shmat01",
        "shmat02",
        "shmat03",
        "shmat04",
        "shmat1",
        "shmctl01",
        "shmctl02",
        "shmctl03",
        "shmctl04",
        "shmctl05",
        "shmctl06",
        "shmctl07",
        "shmctl08",
        "shmdt01",
        "shmdt02",
        "shmem_2nstest",*/
        "shmget02",
        "shmget03",
        "shmget04",
        "shmget05",
        "shmget06",
        "shmt02",
        "shmt03",
        "shmt04",
        "shmt05",
        "shmt06",
        "shmt07",
        "shmt08",
//        "shmt09",
        "shmt10",
        "sigaction01",
        "sigaction02",
//        "sigaltstack01",
        "sighold02",
        "signal06",
        "signalfd01",
        "signalfd4_01",
        "signalfd4_02",
        // "sigpending02",
//        "sigprocmask01",
        "sigrelse01",
        "sigsuspend01",
        // ---
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
//        "brk02",
//        "chdir01",
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
//        "fallocate04",
        "fchmod01",
        "fchmod03",
        "fchmod04",
        "fchmodat01",
//        "fchmodat02",
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
//        "mount01",
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
//        "thp01",
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
        "waitid11",
        "waitpid01",
/*      "waitpid06",
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
        "wqueue09", */
        "write03",
        "write04",
        "write06",
        "write_freezing.sh",
        "writetest",
        "writev02",
//        "writev03",
        "writev05",
        "writev06",
        "writev07", 
//        "mmap01",
//        "mmap-corruption01",
        "mmap001",
//        "mmap01",
//        "mmap03",
        "mmap04",
//        "mmap05",
        "mmap06",
//        "mmap1",
        "mmap10",
        "mmap11",
        "mmap12",
//        "mmap13",
        "mmap14",
        "mmap16",
        "mmap17",
//        "mmap18",
        "mmap2",
        "mmap20", 
//        "mmap3",
        // 追加
        "uaccess",
//        "uevent01",
        "uevent02",
        "uevent03",
        "ulimit01",
        "umask01",
        "umip_basic_test",
        "umount01",
        "umount02",
        "umount03",
        "umount2_01",
        "umount2_02",
        "unlink05",
        "unlink07",
        "unlink08",
        "unlink09",
        "unshare01",
        "unshare01.sh",
        "unshare02",
        "unzip01.sh",
//        "userfaultfd01",
        "userns01",
        "userns02",
        "userns03",
        "userns04",
        "userns05",
        "userns06",
        "userns06_capcheck",
        "userns07",
        "userns08",
        "ustat01",
        "ustat02",
//        "utime01",
//        "utime02",
//        "utime03",
//        "utime04",
//        "utime05",
        "utime06",
        "utime07",
        "utimensat01",
        "utimes01",
//        "utsname02",
//        "utsname03",
        "verify_caps_exec",
        "vfork",
        "vfork01",
//        "vfork02",
//        "vfork_freeze.sh",
        "vhangup01",
        "vhangup02",
        "virt_lib.sh",
        "vlan01.sh",
        "vlan02.sh",
        "vlan03.sh",
//        "vma01",
//        "vma02",
//        "vma03",
//        "vma04",
//        "vma05.sh",
//        "vma05_vdso",
        "vmsplice01",
        "vmsplice02",
        "vmsplice03",
        "vmsplice04",
        "vsock01",
        "vxlan01.sh",
        "vxlan02.sh",
        "vxlan03.sh",
        "vxlan04.sh",
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

// mmap回盘测试
#[cfg(true)]{
    mmap_test();
}

// --- basic cases ---
#[cfg(true)]
{
    write_fd(fd, "
export PAGER=cat
cd /musl 
sh /musl/basic_testcode.sh
sh /musl/busybox_testcode.sh
sh /musl/lua_testcode.sh
sh /musl/libctest_testcode.sh
cd /glibc
sh /glibc/basic_testcode.sh
sh /glibc/busybox_testcode.sh
sh /glibc/lua_testcode.sh
   ");
}

// --- benchmark cases ---
/* 
#[cfg(true)]
{
    write_fd(fd, "
export PAGER=cat
cd /musl
sh /musl/libcbench_testcode.sh
sh /musl/iozone_testcode.sh
sh /musl/lmbench_testcode.sh
cd /glibc
sh /glibc/libcbench_testcode.sh
sh /glibc/iozone_testcode.sh
sh /glibc/lmbench_testcode.sh
");
}
*/

// --- musl ltp ---
#[cfg(true)]
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
#[cfg(true)]
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
    }
    write_fd(fd, "echo \"#### OS COMP TEST GROUP END ltp-glibc ####\"\n");
}

// --- benchmark cases ---
#[cfg(true)]
{
    write_fd(fd, "
export PAGER=cat
cd /musl
sh /musl/cyclictest_testcode.sh
cd /glibc
sh /glibc/cyclictest_testcode.sh
");
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
    while waitpid((-1isize) as usize, &mut _status) > 0 {
        // Reap zombies
    }
    0
}

// 创建文件 → mmap(MAP_SHARED)写入 → munmap → read验证回盘正确
fn mmap_test(){
    const AT_FDCWD: isize = -100;
    const MAP_SHARED: usize = 0x01;
    const PROT_READ: usize  = 1;
    const PROT_WRITE: usize = 2;
    const PAGE_SIZE: usize  = 4096;

    let path = "/musl/mmap_dump\0";
    let test_data: &[u8] = b"HELLO_MMAP_WRITEBACK_0123456789_ABCDEF";
    let data_len = test_data.len();

    // 1. 创建文件，写入初始占位数据（让文件有大小，mmap 才能映射）
    let fd = sys_openat(AT_FDCWD as usize, path, K_WRONLY | K_CREAT | K_TRUNC, 0o777);
    if fd < 0 {
        println!("mmap_dump: create file failed: {}", fd);
    } else {
        let fd = fd as usize;
        let mut init_buf = [0u8; PAGE_SIZE];
        let n = sys_write(fd, &init_buf);
        println!("mmap_dump: wrote {} bytes to set file size", n);
        sys_close(fd);

        // 2. 打开文件用于 mmap（需要读写权限）
        let fd2 = sys_openat(AT_FDCWD as usize, path, 2 /* O_RDWR */, 0);
        if fd2 < 0 {
            println!("mmap_dump: reopen for mmap failed: {}", fd2);
        } else {
            let fd2 = fd2 as usize;

            // 3. mmap: MAP_SHARED, PROT_READ|PROT_WRITE
            let addr = syscall6(
                SYSCALL_MMAP,
                [0, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd2, 0],
            );
            if addr <= 0 {
                println!("mmap_dump: mmap failed: {}", addr);
            } else {
                let addr = addr as usize;
                println!("mmap_dump: mmap at {:#x}", addr);

                // 4. 通过 mmap 指针直接写入测试数据
                let dst = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, data_len) };
                dst.copy_from_slice(test_data);
                println!("mmap_dump: wrote '{}' via mmap",
                    core::str::from_utf8(dst).unwrap_or("?"));

                // 5. munmap
                let ret = sys_munmap(addr, PAGE_SIZE);
                println!("mmap_dump: munmap ret={}", ret);
            }
            sys_close(fd2);
        }

        // 6. 重新打开文件，读取验证回盘内容
        let fd3 = sys_openat(AT_FDCWD as usize, path, 0 /* O_RDONLY */, 0);
        if fd3 < 0 {
            println!("mmap_dump: reopen for verify failed: {}", fd3);
        } else {
            let fd3 = fd3 as usize;
            let mut read_buf = [0u8; 128];
            let n = sys_read(fd3, &mut read_buf);
            if n as usize >= data_len
                && &read_buf[..data_len] == test_data
            {
                println!("mmap_dump: VERIFY OK - file content matches!");
            } else {
                println!("mmap_dump: VERIFY FAIL - read {} bytes, expected '{}', got '{:?}'",
                    n,
                    core::str::from_utf8(test_data).unwrap_or("?"),
                    core::str::from_utf8(&read_buf[..core::cmp::min(n as usize, data_len)]).unwrap_or("?"));
            }
            sys_close(fd3);
        }

        // 7. 触发块缓存刷盘：重新打开并写入，迫使 LRU 淘汰前面 ext4 脏块
        let fd4 = sys_openat(AT_FDCWD as usize, path, 2 /* O_RDWR */, 0);
        if fd4 >= 0 {
            sys_write(fd4 as usize, b"FLUSH");
            sys_close(fd4 as usize);
            println!("mmap_dump: flush done");
        }
    }
}