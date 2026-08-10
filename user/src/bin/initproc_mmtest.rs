#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{
    exit, fork, syscall, syscall6, waitpid, yield_, SYSCALL_MMAP, SYSCALL_MREMAP,
    SYSCALL_MUNMAP,
};

const CHILDREN: usize = 16;
const STACK_WORDS: usize = 256;
const PAGE_SIZE: usize = 4096;
const PROT_READ_WRITE: usize = 0x3;
const MAP_PRIVATE_ANONYMOUS: usize = 0x02 | 0x20;
const MAP_FIXED_PRIVATE_ANONYMOUS: usize = MAP_PRIVATE_ANONYMOUS | 0x10;
const MREMAP_MAYMOVE: usize = 0x1;

fn mmap_anonymous(addr: usize, length: usize, flags: usize) -> isize {
    syscall6(
        SYSCALL_MMAP,
        [
            addr,
            length,
            PROT_READ_WRITE,
            flags,
            usize::MAX,
            0,
        ],
    )
}

fn mremap_zero_copy_regression() -> i32 {
    const OLD_PAGES: usize = 2;
    const NEW_PAGES: usize = 3;

    let old = mmap_anonymous(0, OLD_PAGES * PAGE_SIZE, MAP_PRIVATE_ANONYMOUS);
    if old < 0 {
        println!("MREMAP_REGRESSION source mmap failed ret={}", old);
        return 1;
    }
    let old = old as usize;
    let blocker = mmap_anonymous(
        old + OLD_PAGES * PAGE_SIZE,
        PAGE_SIZE,
        MAP_FIXED_PRIVATE_ANONYMOUS,
    );
    if blocker < 0 {
        println!("MREMAP_REGRESSION blocker mmap failed ret={}", blocker);
        let _ = syscall(SYSCALL_MUNMAP, [old, OLD_PAGES * PAGE_SIZE, 0]);
        return 1;
    }

    unsafe {
        core::ptr::write_volatile(old as *mut usize, 0x0123_4567_89ab_cdef);
        core::ptr::write_volatile(
            (old + PAGE_SIZE) as *mut usize,
            0xfedc_ba98_7654_3210,
        );
    }

    let moved = syscall6(
        SYSCALL_MREMAP,
        [
            old,
            OLD_PAGES * PAGE_SIZE,
            NEW_PAGES * PAGE_SIZE,
            MREMAP_MAYMOVE,
            0,
            0,
        ],
    );
    if moved < 0 || moved as usize == old {
        println!("MREMAP_REGRESSION move failed old={:#x} ret={}", old, moved);
        let _ = syscall(SYSCALL_MUNMAP, [old, OLD_PAGES * PAGE_SIZE, 0]);
        let _ = syscall(SYSCALL_MUNMAP, [blocker as usize, PAGE_SIZE, 0]);
        return 1;
    }
    let moved = moved as usize;

    let first = unsafe { core::ptr::read_volatile(moved as *const usize) };
    let second = unsafe { core::ptr::read_volatile((moved + PAGE_SIZE) as *const usize) };
    if first != 0x0123_4567_89ab_cdef || second != 0xfedc_ba98_7654_3210 {
        println!(
            "MREMAP_REGRESSION data mismatch first={:#x} second={:#x}",
            first, second
        );
        let _ = syscall(SYSCALL_MUNMAP, [moved, NEW_PAGES * PAGE_SIZE, 0]);
        let _ = syscall(SYSCALL_MUNMAP, [blocker as usize, PAGE_SIZE, 0]);
        return 1;
    }

    unsafe {
        core::ptr::write_volatile(
            (moved + 2 * PAGE_SIZE) as *mut usize,
            0xa5a5_a5a5_a5a5_a5a5,
        );
    }
    let tail = unsafe { core::ptr::read_volatile((moved + 2 * PAGE_SIZE) as *const usize) };
    if tail != 0xa5a5_a5a5_a5a5_a5a5 {
        println!("MREMAP_REGRESSION lazy tail mismatch value={:#x}", tail);
        let _ = syscall(SYSCALL_MUNMAP, [moved, NEW_PAGES * PAGE_SIZE, 0]);
        let _ = syscall(SYSCALL_MUNMAP, [blocker as usize, PAGE_SIZE, 0]);
        return 1;
    }

    let _ = syscall(SYSCALL_MUNMAP, [moved, NEW_PAGES * PAGE_SIZE, 0]);
    let _ = syscall(SYSCALL_MUNMAP, [blocker as usize, PAGE_SIZE, 0]);
    println!("MREMAP_REGRESSION pass");
    0
}

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

    if mremap_zero_copy_regression() != 0 {
        return 1;
    }

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
