# RISC-V LTP / buildstorm syscall check

Date: 2026-08-07

## Scope and method

- Root filesystem: `/home/asta/Documents/sdcard-rv.img`, using `/glibc/ltp`.
- The test list was derived from the syscall directories in the image's
  `/glibc/ltp/metadata/ltp.json` and the image's `runtest/syscalls` file.
  The repository `initproc_ltp` whitelist was not used as the source of truth.
- Kernel launch: `make debug-rv INIT=sh RV_IMAGE=/home/asta/Documents/sdcard-rv.img RV_MEM=16G RV_SMP=1`.
- Individual tests were invoked from the shell as
  `busybox timeout N /glibc/ltp/testcases/bin/<case>`.

## Directly observed results

| notes syscall | LTP case | result | evidence |
|---|---|---|---|
| `brk` | `brk01` | PASS | libc and direct-syscall variants both reported `TPASS`; exit 0 |
| `futex` | `futex_wait01` | PASS | four futex wait checks reported `TPASS`; exit 0 |
| `ppoll` | `ppoll01` | PASS | libc and direct-syscall variants passed; 20 TPASS, 0 failures, exit 0 |
| `read` | `read01` | PASS | 1 TPASS, exit 0 |
| `wait4` | `wait401` | PASS | 3 TPASS, exit 0 |
| `rt_sigsuspend` | `rt_sigsuspend01` | PASS | 2 TPASS, exit 0 |
| `mprotect` | `mprotect05` | PASS | 1 TPASS, exit 0 |
| `close` | `close01` | PASS | regular file, pipe, socket close all TPASS |
| `fcntl` | `fcntl02` | PASS | six F_DUPFD cases TPASS |
| `getrandom` | `getrandom04` | PASS | 100-byte getrandom TPASS |
| `sched_getaffinity` | `sched_getaffinity01` | PASS | CPU mask and EFAULT/EINVAL/ESRCH checks TPASS |
| `epoll_pwait` | `epoll_pwait01` | FAIL | one event-wait assertion TFAIL; epoll_pwait2 was TCONF (syscall 441) |
| `statx` | `statx03` | FAIL | 2 TPASS, 5 TFAIL (errno/path validation) |
| `openat` | `openat01` | FAIL | 4 TPASS, 1 TFAIL (file fd expected ENOTDIR, got ENOENT) |
| `getdents64` | `getdents02` | FAIL | 2 TPASS, 6 TFAIL, 2 TCONF |
| `clone3` | `clone301` | FAIL | 3 TPASS, 2 TFAIL (signal/clone3 argument semantics) |
| `waitid` | `waitid01` | PASS | repeated twice; each run reported 5 TPASS, 0 failures, exit 0 |

The `ppoll01` hang was fixed in `sys_ppoll`: the temporary signal mask is read
before taking the task lock, timeout sleeping uses the scheduler deadline path,
the original mask is restored on every return path, and invalid descriptors use
`POLLNVAL`.

The `waitid01` hang had three related user-memory issues. The page-table token
was captured while the current task lock was held, user-memory writes were
attempted while that lock was still held, and one intermediate revision passed
an ASID instead of the full user page-table token. `sys_waitid` now captures
`get_user_token()`, releases the task lock before writing `siginfo_t`, and
revalidates the zombie child by PID before reaping it.

## Candidate inventory

The notes file contains 52 syscall names. The image has matching LTP syscall
families for the majority, including `clone3`, `epoll_pwait`, `futex`, `mmap`,
`mprotect`, `ppoll`, `statx`, `waitid`, and `fadvise`. Some names in `notes.md`
are libc-facing aliases (`newfstatat`, `getdents64`, `rt_sigprocmask`,
`set_robust_list`, `rseq`) and must be mapped to their underlying LTP family
rather than searched as literal binary names.

No result from the repository's old ELF/whitelist runner is included in this
record.
