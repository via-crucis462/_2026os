#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{exit, fork, waitpid, yield_};

const CHILDREN: usize = 16;
const STACK_WORDS: usize = 256;

#[inline(never)]
fn exercise_child_stack(round: usize) -> i32 {
    let mut words = [0usize; STACK_WORDS];
    for (index, word) in words.iter_mut().enumerate() {
        *word = round.wrapping_mul(0x10001).wrapping_add(index);
    }

    for _ in 0..4 {
        yield_();
    }

    for (index, word) in words.iter().enumerate() {
        let value = unsafe { core::ptr::read_volatile(word) };
        if value != round.wrapping_mul(0x10001).wrapping_add(index) {
            return 1;
        }
    }
    0
}

#[no_mangle]
fn main() -> i32 {
    println!("MM_CLONE_REGRESSION begin children={}", CHILDREN);

    let mut pids = [0usize; CHILDREN];
    for (round, slot) in pids.iter_mut().enumerate() {
        let pid = fork();
        if pid < 0 {
            println!("MM_CLONE_REGRESSION fork failed round={} ret={}", round, pid);
            return 1;
        }
        if pid == 0 {
            exit(exercise_child_stack(round));
        }
        *slot = pid as usize;
    }

    for (round, pid) in pids.iter().copied().enumerate() {
        let mut status = -1;
        let waited = waitpid(pid, &mut status);
        if waited != pid as isize || status != 0 {
            println!(
                "MM_CLONE_REGRESSION wait failed round={} pid={} waited={} status={}",
                round, pid, waited, status
            );
            return 1;
        }
    }

    println!("MM_CLONE_REGRESSION pass");
    0
}
