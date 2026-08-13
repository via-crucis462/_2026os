#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{close, exec, syscall6, write, SYSCALL_OPENAT};

const AT_FDCWD: usize = usize::MAX - 99;
const O_WRONLY: usize = 1;
const O_CREAT: usize = 1 << 6;
const O_TRUNC: usize = 1 << 9;
const MMBENCH_PATH: &str = "/tmp/mmbench\0";
const MMBENCH: &[u8] = include_bytes!("../../../benchmarks/build/mmbench");

fn open_output() -> isize {
    syscall6(
        SYSCALL_OPENAT,
        [
            AT_FDCWD,
            MMBENCH_PATH.as_ptr() as usize,
            O_WRONLY | O_CREAT | O_TRUNC,
            0o755,
            0,
            0,
        ],
    )
}

#[no_mangle]
fn main() -> i32 {
    let fd = open_output();
    if fd < 0 {
        println!("MMBENCH_LAUNCH_FAIL open={}", fd);
        return 1;
    }

    let fd = fd as usize;
    let mut offset = 0;
    while offset < MMBENCH.len() {
        let end = core::cmp::min(offset + 8192, MMBENCH.len());
        let written = write(fd, &MMBENCH[offset..end]);
        if written <= 0 {
            println!("MMBENCH_LAUNCH_FAIL write={}", written);
            close(fd);
            return 1;
        }
        offset += written as usize;
    }
    close(fd);

    let argv = [MMBENCH_PATH.as_ptr(), core::ptr::null()];
    let result = exec(MMBENCH_PATH, &argv);
    println!("MMBENCH_LAUNCH_FAIL exec={}", result);
    1
}
