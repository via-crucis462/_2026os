#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
extern crate alloc;

use alloc::vec::Vec;
use user_lib::{exec, exit, fork, waitpid};

fn cstr(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() + 1);
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    bytes
}

fn run_bash(command: &str) -> i32 {
    let child = fork();
    if child == 0 {
        let bash = cstr("bash");
        let dash_c = cstr("-c");
        let script = cstr(command);
        let argv = [
            bash.as_ptr(),
            dash_c.as_ptr(),
            script.as_ptr(),
            core::ptr::null(),
        ];
        exec("/bin/bash\0", &argv);
        exit(127);
    }
    if child < 0 {
        return child as i32;
    }

    let mut status = -1;
    if waitpid(child as usize, &mut status) != child {
        return -1;
    }
    status
}

#[no_mangle]
fn main() -> i32 {
    println!("CARGO_REGRESSION begin");

    // Match the guest buildstorm environment.  The init process does not run
    // a login shell, so its default PATH intentionally does not include
    // rustup's cargo shim directory.
    let toolchain_status = run_bash(
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; \
         export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo; \
         export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true; \
         rustc --version && cargo --version",
    );
    println!("CARGO_REGRESSION toolchain status={}", toolchain_status);
    if toolchain_status != 0 {
        return 1;
    }

    // `cargo new` invokes rustfmt when it is available.  Run that subprocess
    // on a one-line source file first so a thread/futex failure is isolated
    // from Cargo's project creation and filesystem work.
    let rustfmt_status = run_bash(
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; \
         export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo; \
         export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true; \
         printf '%s\\n' 'fn main(){let value=1;println!(\"{}\",value);}' > /tmp/mm-rustfmt-regression.rs; \
         rustfmt /tmp/mm-rustfmt-regression.rs",
    );
    println!("CARGO_REGRESSION rustfmt status={}", rustfmt_status);
    if rustfmt_status != 0 {
        return 1;
    }

    let new_status = run_bash(
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; \
         export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo; \
         export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true; \
         rm -rf /tmp/mm-cargo-regression; cargo new --vcs none /tmp/mm-cargo-regression",
    );
    println!("CARGO_REGRESSION cargo_new status={}", new_status);
    if new_status != 0 {
        return 1;
    }

    // Keep the first compiler invocation outside Cargo.  A failure here is a
    // rustc/thread/runtime regression; a later Cargo-only failure narrows the
    // investigation to Cargo's process and file-coordination paths.
    let direct_rustc_status = run_bash(
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; \
         export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo; \
         export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true; \
         rustc /tmp/mm-cargo-regression/src/main.rs -o /tmp/mm-cargo-regression/mm-direct-rustc",
    );
    println!("CARGO_REGRESSION direct_rustc status={}", direct_rustc_status);
    if direct_rustc_status != 0 {
        return 1;
    }

    let direct_run_status = run_bash("/tmp/mm-cargo-regression/mm-direct-rustc");
    println!("CARGO_REGRESSION direct_run status={}", direct_run_status);
    if direct_run_status != 0 {
        return 1;
    }

    let build_status = run_bash(
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; \
         export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo; \
         export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true; \
         cd /tmp/mm-cargo-regression && cargo build",
    );
    println!("CARGO_REGRESSION cargo_build status={}", build_status);
    if build_status != 0 {
        return 1;
    }

    let run_status = run_bash("/tmp/mm-cargo-regression/target/debug/mm-cargo-regression");
    println!("CARGO_REGRESSION cargo_run status={}", run_status);
    if run_status != 0 {
        return 1;
    }

    println!("CARGO_REGRESSION pass");
    0
}
