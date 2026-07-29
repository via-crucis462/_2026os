pub mod rvvirt;
pub mod rvvisionfive2;

#[cfg(board = "virt")]
pub use rvvirt::*;

#[cfg(board = "visionfive2")]
pub use rvvisionfive2::*;