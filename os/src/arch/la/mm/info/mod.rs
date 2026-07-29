pub mod la2k1000;
pub mod lavirt;

#[cfg(board = "2k1000")]
pub use la2k1000::*;

#[cfg(board = "virt")]
pub use lavirt::*;