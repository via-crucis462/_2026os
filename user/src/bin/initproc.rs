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

/// 优先用 bash 执行命令，如果 bash 不存在，则尝试 busybox sh
fn run_bash(cmd: &str) -> i32 {
    let forked = fork();
    if forked == 0 {
        // child: exec /bin/bash -c "cmd"
        let bash = cstr("bash");
        let dash_c = cstr("-c");
        let script = cstr(cmd);

        let bash_argv: &[*const u8] = &[
            bash.as_ptr(),
            dash_c.as_ptr(),
            script.as_ptr(),
            core::ptr::null(),
        ];
        exec("/bin/bash\0", bash_argv);

        // fallback: exec busybox sh -c "cmd"
        let busybox = cstr("busybox");
        let sh = cstr("sh");
        let busybox_argv: &[*const u8] = &[
            busybox.as_ptr(),
            sh.as_ptr(),
            dash_c.as_ptr(),
            script.as_ptr(),
            core::ptr::null(),
        ];
        exec("/musl/busybox\0", busybox_argv);
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
    // 直接运行 /glibc 下的测试脚本（不再注入自己的脚本）。
    // 依次运行两个脚本：即使第一个失败，第二个也必须执行。
    println!("[init] dumping /glibc/buildstorm_testcode.sh ...");
    let dump_status = run_bash("cat /glibc/buildstorm_testcode.sh");
    println!("[init] script dump exited with status {}", dump_status);

    println!("[init] running cagent_testcode.sh ...");
    let status1 = run_bash("cd /glibc && ./cagent_testcode.sh");
    println!("[init] cagent_testcode.sh exited with status {}", status1);

    println!("[init] running buildstorm_testcode.sh ...");
    let status2 = run_bash("cd /glibc && ./buildstorm_testcode.sh");
    println!("[init] buildstorm_testcode.sh exited with status {}", status2);

    if status1 == 0 && status2 == 0 { 0 } else { 1 }
}
