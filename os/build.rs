use std::env;

static TARGET_PATH: &str = "../user/target/riscv64gc-unknown-none-elf/release/";

fn main() {
    let log_level = env::var("LOG").unwrap_or_else(|_| "ERROR".to_string());
    let initproc = env::var("INIT")
        .unwrap_or_else(|_| "default".to_string());
    // 防止传入空字符串
    let initproc = if initproc.is_empty() { "default".to_string() } else { initproc };

    println!("cargo::rustc-check-cfg=cfg(initproc, values(\"default\", \"sh\", \"ltp\"))");

    println!("cargo:rerun-if-changed=../user/src/");
    println!("cargo:rerun-if-changed={}", TARGET_PATH);
    println!("cargo:rerun-if-env-changed=LOG");
    println!("cargo:rerun-if-env-changed=INITPROC");
    println!("cargo:rustc-env=LOG={}", log_level);
    println!("cargo:rustc-cfg=initproc=\"{}\"", initproc);
}
