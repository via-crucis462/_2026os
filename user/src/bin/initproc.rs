#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;
extern crate alloc;

use alloc::vec::Vec;
use user_lib::*;

// kernel 的 OpenFlags：WRONLY=1, CREATE=1<<6, TRUNC=1<<9
const K_WRONLY: u32 = 1;
const K_CREAT: u32 = 1 << 6;
const K_TRUNC: u32 = 1 << 9;
const AT_FDCWD: isize = -100;

/// Build a null-terminated C string from a &str
fn cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// 通过系统调用把脚本内容完整写入 /tmp 下的文件。
fn inject_script(path: &str, content: &str) -> bool {
    let path_c = cstr(path);
    let fd = sys_openat(
        AT_FDCWD as usize,
        // user_lib 的 sys_openat 直接传指针，内核按 C 字符串解析，必须带 NUL。
        unsafe { core::str::from_utf8_unchecked(&path_c) },
        K_WRONLY | K_CREAT | K_TRUNC,
        0o755,
    );
    if fd < 0 {
        println!("[init] inject open {} failed: {}", path, fd);
        return false;
    }
    let bytes = content.as_bytes();
    let mut off = 0;
    while off < bytes.len() {
        let n = sys_write(fd as usize, &bytes[off..]);
        if n <= 0 {
            println!("[init] inject write {} failed: {}", path, n);
            sys_close(fd as usize);
            return false;
        }
        off += n as usize;
    }
    sys_close(fd as usize);
    true
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
    let cagent_ok = inject_script(
        "/tmp/cagent_testcode.sh",
        include_str!("scripts/cagent_testcode.sh"),
    );
    let buildstorm_ok = inject_script(
        "/tmp/buildstorm_testcode.sh",
        include_str!("scripts/buildstorm_testcode.sh"),
    );
    println!(
        "[init] injected scripts: cagent={} buildstorm={}",
        cagent_ok, buildstorm_ok
    );

    let cagent_cmd = if cagent_ok {
        "cd /glibc && bash /tmp/cagent_testcode.sh"
    } else {
        "cd /glibc && ./cagent_testcode.sh"
    };
    let buildstorm_cmd = if buildstorm_ok {
        "cd /glibc && bash /tmp/buildstorm_testcode.sh"
    } else {
        "cd /glibc && ./buildstorm_testcode.sh"
    };

    // 依次运行两个测试脚本：即使第一个失败，第二个也必须执行。
    println!("[init] running cagent_testcode.sh ...");
    let status1 = run_bash(cagent_cmd);
    println!("[init] cagent_testcode.sh exited with status {}", status1);

    println!("[init] running buildstorm_testcode.sh ...");
    let status2 = run_bash(buildstorm_cmd);
    println!("[init] buildstorm_testcode.sh exited with status {}", status2);

    if status1 == 0 && status2 == 0 { 0 } else { 1 }
}
