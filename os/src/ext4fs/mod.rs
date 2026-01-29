mod bitmap;
// mod block_cache; // 重复定义，已注释
//mod efs;
mod layout;
//mod vfs;
pub mod superblock;
pub mod block_cache;
pub mod block_dev;
pub const BLOCK_SZ: usize = 4096;
//use bitmap::Bitmap;
pub use block_dev::BlockDevice;
//pub use efs::EasyFileSystem;
//use layout::*;
//pub use vfs::Inode;
pub use superblock::{Ext4SuperBlock, Ext4SuperBlockDisk};
pub use block_cache::{block_cache_sync_all , get_block_cache};