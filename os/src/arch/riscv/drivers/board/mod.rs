#[cfg(board = "visionfive2")]
pub mod visionfive2;
#[cfg(board = "visionfive2")]
pub use visionfive2::*;

#[cfg(board = "virt")]
pub mod virt;
#[cfg(board = "virt")]
pub use virt::*;
