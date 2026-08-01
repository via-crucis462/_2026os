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

/// Run the final image's interactive Bash and return its exit status.
fn run_shell() -> i32 {
    let forked = fork();
    if forked == 0 {
        let a0 = cstr("bash");
        let a1 = cstr("-i");

        let argv: &[*const u8] = &[
            a0.as_ptr(),
            a1.as_ptr(),
            core::ptr::null(),
        ];

        exec("/bin/bash\0", argv);
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
    // PID 1 must survive a shell exit so that the kernel keeps running.
    loop {
        let status = run_shell();
        println!("init: bash exited with status {}, restarting", status);
        for _ in 0..1000 {
            yield_();
        }
    }
}
