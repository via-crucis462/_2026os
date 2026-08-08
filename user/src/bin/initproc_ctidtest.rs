#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

#[cfg(target_arch = "riscv64")]
use core::arch::global_asm;
#[cfg(target_arch = "riscv64")]
use user_lib::{exit, fork, syscall6, waitpid};

#[cfg(target_arch = "riscv64")]
const SYSCALL_FUTEX: usize = 98;
#[cfg(target_arch = "riscv64")]
const SYSCALL_PIPE: usize = 59;
#[cfg(target_arch = "riscv64")]
const SYSCALL_READ: usize = 63;
#[cfg(target_arch = "riscv64")]
const SYSCALL_SCHED_SETAFFINITY: usize = 122;
#[cfg(target_arch = "riscv64")]
const SYSCALL_RT_SIGPROCMASK: usize = 135;
#[cfg(target_arch = "riscv64")]
const SYSCALL_MMAP: usize = 222;
#[cfg(target_arch = "riscv64")]
const FUTEX_WAIT: usize = 0;
#[cfg(target_arch = "riscv64")]
const PROT_READ_WRITE: usize = 0x3;
#[cfg(target_arch = "riscv64")]
const MAP_PRIVATE_ANONYMOUS: usize = 0x22;
#[cfg(target_arch = "riscv64")]
const PAGE_SIZE: usize = 4096;

#[cfg(target_arch = "riscv64")]
const CLONE_VM: usize = 0x0000_0100;
#[cfg(target_arch = "riscv64")]
const CLONE_FS: usize = 0x0000_0200;
#[cfg(target_arch = "riscv64")]
const CLONE_FILES: usize = 0x0000_0400;
#[cfg(target_arch = "riscv64")]
const CLONE_SIGHAND: usize = 0x0000_0800;
#[cfg(target_arch = "riscv64")]
const CLONE_THREAD: usize = 0x0001_0000;
#[cfg(target_arch = "riscv64")]
const CLONE_SYSVSEM: usize = 0x0004_0000;
#[cfg(target_arch = "riscv64")]
const CLONE_PARENT_SETTID: usize = 0x0010_0000;
#[cfg(target_arch = "riscv64")]
const CLONE_CHILD_CLEARTID: usize = 0x0020_0000;
#[cfg(target_arch = "riscv64")]
const THREAD_CLONE_FLAGS: usize = CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_THREAD
    | CLONE_SYSVSEM
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID;

#[cfg(target_arch = "riscv64")]
const THREAD_STACK_SIZE: usize = 16 * 1024;

#[cfg(target_arch = "riscv64")]
#[repr(align(16))]
#[allow(dead_code)]
struct ThreadStack([u8; THREAD_STACK_SIZE]);

#[cfg(target_arch = "riscv64")]
static mut THREAD_STACK: ThreadStack = ThreadStack([0; THREAD_STACK_SIZE]);
#[cfg(target_arch = "riscv64")]
#[repr(C)]
struct TestTimeSpec {
    tv_sec: usize,
    tv_nsec: usize,
}

// These objects are read by the assembly child after its inner fork.  The
// actual clear-tid word is deliberately in its own private anonymous page.
#[cfg(target_arch = "riscv64")]
#[no_mangle]
static CTID_FORK_READY_BYTE: u8 = 1;
#[cfg(target_arch = "riscv64")]
#[no_mangle]
static CTID_FORK_DELAY: TestTimeSpec = TestTimeSpec {
    tv_sec: 0,
    tv_nsec: 50_000_000,
};

// Returning into Rust after switching to a different stack would make this
// probe depend on compiler frame layout. The child therefore exits directly.
// The second path forks, notifies the caller through a pipe, then sleeps
// before exit. At that point its private ctid page is COW-protected, while the
// caller has a deterministic indication that it may enter FUTEX_WAIT.
#[cfg(target_arch = "riscv64")]
global_asm!(
    ".section .text\n",
    ".global ctid_clone_child_exit\n",
    ".type ctid_clone_child_exit,@function\n",
    "ctid_clone_child_exit:\n",
    "    li a7, 220\n",
    "    ecall\n",
    "    bnez a0, 1f\n",
    "    li a0, 0\n",
    "    li a7, 93\n",
    "    ecall\n",
    "1:\n",
    "    ret\n",
    ".global ctid_clone_child_fork_then_exit\n",
    ".type ctid_clone_child_fork_then_exit,@function\n",
    "ctid_clone_child_fork_then_exit:\n",
    // The sixth C argument is the ready-pipe write fd. It is not part of
    // clone's ABI, so preserve it across the two clone syscalls in t0.
    "    mv t0, a5\n",
    "    li a7, 220\n",
    "    ecall\n",
    "    bnez a0, 3f\n",
    // fork() is clone(0, 0, 0, 0, 0). Both resulting tasks exit below.
    "    li a0, 0\n",
    "    li a1, 0\n",
    "    li a2, 0\n",
    "    li a3, 0\n",
    "    li a4, 0\n",
    "    li a7, 220\n",
    "    ecall\n",
    // The fork child exits immediately. The original clear-tid thread first
    // tells the waiter that COW is now armed, then sleeps long enough for it
    // to enter the queue before clear_child_tid performs the first write.
    "    beqz a0, 1f\n",
    "    mv a0, t0\n",
    "    la a1, CTID_FORK_READY_BYTE\n",
    "    li a2, 1\n",
    "    li a7, 64\n",
    "    ecall\n",
    "    la a0, CTID_FORK_DELAY\n",
    "    li a1, 0\n",
    "    li a7, 101\n",
    "    ecall\n",
    "1:\n",
    "    li a0, 0\n",
    "    li a7, 93\n",
    "    ecall\n",
    "3:\n",
    "    ret\n",
);

#[cfg(target_arch = "riscv64")]
extern "C" {
    fn ctid_clone_child_exit(
        flags: usize,
        stack: usize,
        parent_tid: *mut u32,
        tls: usize,
        child_tid: *mut u32,
        ready_pipe_write: usize,
    ) -> isize;
    fn ctid_clone_child_fork_then_exit(
        flags: usize,
        stack: usize,
        parent_tid: *mut u32,
        tls: usize,
        child_tid: *mut u32,
        ready_pipe_write: usize,
    ) -> isize;
}

#[cfg(target_arch = "riscv64")]
type CloneEntry = unsafe extern "C" fn(usize, usize, *mut u32, usize, *mut u32, usize) -> isize;

#[cfg(target_arch = "riscv64")]
fn thread_stack_top() -> usize {
    unsafe {
        let base = core::ptr::addr_of_mut!(THREAD_STACK) as *mut u8;
        base.add(THREAD_STACK_SIZE) as usize
    }
}

#[cfg(target_arch = "riscv64")]
fn pin_current_to_hart0() -> bool {
    let mask = 1u8;
    let result = syscall6(
        SYSCALL_SCHED_SETAFFINITY,
        [0, 1, core::ptr::addr_of!(mask) as usize, 0, 0, 0],
    );
    if result != 0 {
        println!("CTID_REGRESSION sched_setaffinity failed ret={}", result);
    }
    result == 0
}

#[cfg(target_arch = "riscv64")]
fn block_sigchld() -> bool {
    // SignalFlags stores signal N at bit N - 1. The COW trigger forks a child;
    // its exit must not interrupt the futex waiter before clear_child_tid does.
    let sigchld_mask = 1usize << 16;
    let result = syscall6(
        SYSCALL_RT_SIGPROCMASK,
        [
            0,
            core::ptr::addr_of!(sigchld_mask) as usize,
            0,
            core::mem::size_of::<usize>(),
            0,
            0,
        ],
    );
    if result != 0 {
        println!("CTID_REGRESSION rt_sigprocmask failed ret={}", result);
    }
    result == 0
}

#[cfg(target_arch = "riscv64")]
fn allocate_private_tid_word() -> Option<*mut u32> {
    let address = syscall6(
        SYSCALL_MMAP,
        [
            0,
            PAGE_SIZE,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS,
            usize::MAX,
            0,
        ],
    );
    if address < 0 {
        return None;
    }
    let word = address as usize as *mut u32;
    unsafe { core::ptr::write_volatile(word, 0) };
    Some(word)
}

#[cfg(target_arch = "riscv64")]
fn open_ready_pipe() -> Option<[u32; 2]> {
    let mut fds = [0u32; 2];
    let result = syscall6(
        SYSCALL_PIPE,
        [fds.as_mut_ptr() as usize, 0, 0, 0, 0, 0],
    );
    if result == 0 {
        Some(fds)
    } else {
        None
    }
}

#[cfg(target_arch = "riscv64")]
fn read_ready_byte(fd: u32) -> bool {
    let mut byte = 0u8;
    syscall6(
        SYSCALL_READ,
        [fd as usize, core::ptr::addr_of_mut!(byte) as usize, 1, 0, 0, 0],
    ) == 1
        && byte == 1
}

#[cfg(target_arch = "riscv64")]
fn run_clear_child_tid_join(label: &str, clone_entry: CloneEntry, fork_before_exit: bool) -> i32 {
    let Some(child_tid) = allocate_private_tid_word() else {
        println!("{} private ctid mmap failed", label);
        return 1;
    };

    let ready_pipe = if fork_before_exit {
        let Some(fds) = open_ready_pipe() else {
            println!("{} ready pipe failed", label);
            return 1;
        };
        Some(fds)
    } else {
        None
    };

    let child = unsafe {
        clone_entry(
            THREAD_CLONE_FLAGS,
            thread_stack_top(),
            child_tid,
            0,
            child_tid,
            ready_pipe.map_or(0, |fds| fds[1] as usize),
        )
    };
    if child <= 0 {
        println!("{} clone failed ret={}", label, child);
        return 1;
    }

    let expected = child as u32;
    let before_wait = unsafe { core::ptr::read_volatile(child_tid) };
    println!(
        "{} child_tid={} word_before_wait={}",
        label, expected, before_wait
    );
    if before_wait != expected {
        println!("{} child tid was not published", label);
        return 1;
    }

    if let Some(fds) = ready_pipe {
        if !read_ready_byte(fds[0]) {
            println!("{} inner-fork readiness failed", label);
            return 1;
        }
    }

    let wait_result = syscall6(
        SYSCALL_FUTEX,
        [
            child_tid as usize,
            FUTEX_WAIT,
            expected as usize,
            0,
            0,
            0,
        ],
    );
    let after_wait = unsafe { core::ptr::read_volatile(child_tid) };
    println!(
        "{} futex_ret={} word_after_wait={}",
        label, wait_result, after_wait
    );

    if wait_result != 0 || after_wait != 0 {
        println!("{} clear-and-wake failed", label);
        return 1;
    }
    0
}

#[cfg(target_arch = "riscv64")]
#[no_mangle]
fn main() -> i32 {
    // PID 1 is the test harness. A thread must run in a child thread group so
    // that its normal thread exit does not terminate the harness itself.
    println!("CTID_REGRESSION begin");
    let worker = fork();
    if worker < 0 {
        println!("CTID_REGRESSION worker fork failed ret={}", worker);
        return 1;
    }
    if worker == 0 {
        if !pin_current_to_hart0() || !block_sigchld() {
            exit(1);
        }
        let basic = run_clear_child_tid_join("CTID_REGRESSION", ctid_clone_child_exit, false);
        if basic != 0 {
            println!("CTID_REGRESSION worker result={}", basic);
            exit(basic);
        }
        let result = run_clear_child_tid_join(
            "CTID_COW_REGRESSION",
            ctid_clone_child_fork_then_exit,
            true,
        );
        println!("CTID_REGRESSION worker result={}", result);
        exit(result);
    }

    let mut status = -1;
    let waited = waitpid(worker as usize, &mut status);
    if waited != worker || status != 0 {
        println!(
            "CTID_REGRESSION worker wait failed waited={} worker={} status={}",
            waited, worker, status
        );
        return 1;
    }
    println!("CTID_REGRESSION pass");
    0
}

#[cfg(not(target_arch = "riscv64"))]
#[no_mangle]
fn main() -> i32 {
    println!("CTID_REGRESSION skipped: riscv64-only probe");
    0
}
