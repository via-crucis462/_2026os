#[cfg(board = "2k1000")]
pub mod la2k1000;
#[cfg(board = "2k1000")]
pub use la2k1000::*;

#[cfg(board = "virt")]
pub mod virt;
#[cfg(board = "virt")]
pub use virt::*;