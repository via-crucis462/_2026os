use std::env;

static TARGET_PATH: &str = "../user/target/riscv64gc-unknown-none-elf/release/";

fn main() {
    let log_level = env::var("LOG").unwrap_or_else(|_| "ERROR".to_string());

    println!("cargo:rerun-if-changed=../user/src/");
    println!("cargo:rerun-if-changed={}", TARGET_PATH);
    println!("cargo:rerun-if-env-changed=LOG");
    println!("cargo:rustc-env=LOG={}", log_level);
}
