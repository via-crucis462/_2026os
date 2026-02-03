#![allow(dead_code)]

#[cfg(target_arch = "riscv64")]
pub mod riscv;
#[cfg(target_arch = "la64")]
pub mod la;

#[cfg(target_arch = "riscv64")]
pub use riscv::*;
#[cfg(target_arch = "la64")]
pub use la::*;

// 其他架构
#[cfg(not(any(target_arch = "riscv64", target_arch = "la64")))]
compile_error!("Unsupported target arch");