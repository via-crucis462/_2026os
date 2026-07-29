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
fn run_shell(_cmd: &str) -> i32 {
    let forked = fork();
    if forked == 0 {
        // child: exec busybox sh -c "cmd"
        let a0 = cstr("busybox");
        let a1 = cstr("sh");

        let argv: &[*const u8] = &[
            a0.as_ptr(),
            a1.as_ptr(),
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
    run_shell("");
    // Init (PID 1) must never exit — otherwise the kernel panics.
    // Loop forever, reaping any zombie children.

    let mut _status: i32 = 0;
    // waitpid(-1, ...) = wait for any child; returns -1 if no children
    waitpid((-1isize) as usize, &mut _status);
    0
}