pub mod la2k1000;
pub mod lavirt;
pub mod rvvirt;

#[cfg(board = "2k1000")]
pub use la2k1000::*;

#[cfg(board = "virt")]
#[cfg(target_arch = "loongarch64")]
pub use lavirt::*;

#[cfg(board = "virt")]
#[cfg(target_arch = "riscv64")]
pub use rvvirt::*;