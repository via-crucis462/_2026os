use std::env;

static TARGET_PATH: &str = "../user/target/riscv64gc-unknown-none-elf/release/";

fn main() {
    let log_level = env::var("LOG").unwrap_or_else(|_| "ERROR".to_string());
    let initproc = env::var("INIT")
        .unwrap_or_else(|_| "default".to_string());
    let board = env::var("BOARD").unwrap_or_else(|_| "virt".to_string());

    // 防止传入空字符串
    let initproc = if initproc.is_empty() {
        "default".to_string()
    } else {
        initproc
    };

    println!("cargo::rustc-check-cfg=cfg(initproc, values(\"default\", \"sh\", \"ltp\"))");
    println!("cargo::rustc-check-cfg=cfg(log_level, values(\"OFF\", \"ERROR\", \"WARN\", \"INFO\", \"DEBUG\", \"TRACE\"))");
    println!("cargo::rustc-check-cfg=cfg(board, values(\"virt\", \"2k1000\", \"visionfive2\"))");
    println!("cargo::rustc-check-cfg=cfg(no_console_lock)");

    let no_console_lock = env::var("NO_CONSOLE_LOCK").unwrap_or_else(|_| "0".to_string());
    if no_console_lock == "1" {
        println!("cargo::rustc-cfg=no_console_lock");
    }

    println!("cargo:rerun-if-changed=../user/src/");
    println!("cargo:rerun-if-changed={}", TARGET_PATH);
    println!("cargo:rerun-if-env-changed=LOG");
    println!("cargo:rerun-if-env-changed=INIT");
    println!("cargo:rerun-if-env-changed=BOARD");
    println!("cargo:rerun-if-env-changed=NO_CONSOLE_LOCK");
    println!("cargo:rustc-env=LOG={}", log_level);
    println!("cargo:rustc-cfg=log_level=\"{}\"", log_level);
    println!("cargo:rustc-cfg=initproc=\"{}\"", initproc);
    println!("cargo:rustc-cfg=board=\"{}\"", board);
}
