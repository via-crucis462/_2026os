#![allow(dead_code)]

#[cfg(target_arch = "riscv64")]
pub mod riscv;
#[cfg(target_arch = "loongarch64")]
pub mod loongarch;

#[cfg(target_arch = "riscv64")]
pub use riscv::*;
#[cfg(target_arch = "loongarch64")]
pub use loongarch::*;

// 其他架构
#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
compile_error!("Unsupported target arch");