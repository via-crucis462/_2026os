//! 块设备驱动模块

// 传递 arch 中的 block 驱动
pub use crate::arch::drivers::*;

pub mod block_cache;
pub mod block_dev;

pub const BLOCK_SZ: usize = 4096;

use crate::ext4fs::{get_block_cache, BlockDevice};
