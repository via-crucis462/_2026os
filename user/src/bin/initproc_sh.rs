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

/// 优先用 bash，如果 bash 不存在，则尝试 busybox
fn run_shell() -> i32 {
    let forked = fork();
    if forked == 0 {
        let bash = cstr("bash");
        let interactive = cstr("-i");
        let bash_argv: &[*const u8] = &[bash.as_ptr(), interactive.as_ptr(), core::ptr::null()];
        exec("/bin/bash\0", bash_argv);

        let busybox = cstr("busybox");
        let sh = cstr("sh");
        let busybox_argv: &[*const u8] = &[
            busybox.as_ptr(),
            sh.as_ptr(),
            interactive.as_ptr(),
            core::ptr::null(),
        ];
        exec("/musl/busybox\0", busybox_argv);
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
    // PID 1 must survive a shell exit so that the kernel keeps running.
    let status = run_shell();
    println!("init: shell exited with status {}, restarting...", status);
    status
}
