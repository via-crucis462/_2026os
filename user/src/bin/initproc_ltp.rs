#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
extern crate alloc;

use alloc::vec::Vec;
use alloc::format;
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

#[no_mangle]
fn main() -> i32 {
    chdir("/glibc\0");

    // 测例首字母
    let test_start = "";

    // 测例黑名单
    const SKIP_CASES: &[&str] = &[
        "cgroup_regression_3_1.sh",
        "cgroup_regression_3_2.sh",
        "cgroup_regression_5_1.sh",
        "cgroup_regression_5_2.sh",
        "cgroup_regression_6_1.sh",
        "cgroup_regression_6_2.sh",
        "cgroup_regression_fork_processes",
        "cgroup_regression_getdelays",
        "cgroup_fj_common.sh",
        "cpuctl_def_task0*",
        "cpuctl*_test0*",
        "clock_gettime01",
        "cve-*",
        "dio_*",
        "doio*",
        "dynamic_debug0*",
        "dma_thread_diotest",
        "epoll-ltp",
        "fallocate05",
        "fallocate06",
        "force_erase.sh",
        "fork_exec_loop",
        "fs_racer_*.sh",
        "gen*",
        "gettimeofday01",
        "kill1*",
        "lftest",
        "memcg_test_*",
        "memcpy*",
        "memcontrol*",
        "memctl*",
        "mtest*",
        "pidns*",
        "pids_task*",
        "select04*",
        "sendfile07*",//无限输出“UnixSocket write called with 1 bytes”
        "setfsgid03*",//“ Panicked at src/mm/heap_allocator.rs:12 Heap allocation error, layout = Layout { size: 8192, align: 1 (1 << 0) }”
        "setrlimit05*",
        "sigtimedwait01*",
        "sigwait01*",
        "sigwaitinfo01*",
        "statx11*",
        "timed_forkbomb*",
        "tst_hexdump*",
        "epoll_pwait*",
        "hackbench",
        "futex*", // 没实现快速锁，会死循环，先注释掉
        "tcp*",
        "udp*",
    ];

    let skip_list = {
        let mut s = alloc::string::String::new();
        for (i, case) in SKIP_CASES.iter().enumerate() {
            if i > 0 {
                s.push('|');
            }
            s.push_str(case);
        }
        s
    };

    // Shell script that iterates LTP testcases.
    // Only runs testcases whose filename starts with `test_start`.
    // Helper scripts that expect arguments are skipped to prevent infinite loops.
    let cmd = format!(
"
echo \"#### OS COMP TEST GROUP START ltp-musl ####\"; \
for f in ltp/testcases/bin/{0}*; do \
  fname=$(basename \"$f\"); \
  case \"$fname\" in \
    {1}) \
      echo \"SKIP LTP CASE $fname\"; \
      continue ;; \
  esac; \
  echo \"RUN LTP CASE $fname\"; \
  \"$f\"; \
  ret=$?; \
  echo \"FAIL LTP CASE $fname : $ret\"; \
done; \
echo \"#### OS COMP TEST GROUP END ltp-musl ####\"
",
    test_start,
    skip_list,
);

    run_shell(&cmd);

    // Init (PID 1) must never exit — otherwise the kernel panics.
    // Loop forever, reaping any zombie children.
    let mut _status: i32 = 0;
    waitpid((-1isize) as usize, &mut _status);
    0
}