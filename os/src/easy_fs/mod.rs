mod bitmap;
mod efs;
mod layout;
mod vfs;

pub const BLOCK_SZ: usize = 512;
use bitmap::Bitmap;
pub use crate::ext4fs::block_dev::BlockDevice;
pub use efs::EasyFileSystem;
use layout::*;
pub use vfs::Inode;
