pub mod superblock;
pub mod blockgroup;
pub mod ext4inode;
pub mod ext4;
pub mod vfs;
pub mod ext4_dir_entry;
pub use crate::drivers::block::*;

use core::sync::atomic::{AtomicBool, Ordering};
use alloc::sync::Arc;

pub use block_dev::BlockDevice;
pub use superblock::{Ext4SuperBlock, Ext4SuperBlockDisk};
pub use blockgroup::{Ext4GroupDescDisk, Ext4Group};
pub use cache::{block_cache_sync_all, get_block_cache, invalidate_block_cache};
pub use ext4::Ext4FS;
pub use crate::ext4fs::ext4inode::{Ext4InodeDisk, Ext4Inode};

pub const BLOCK_SZ: usize = 4096;

/// 下面三个辅助函数基于以前显式访问块缓存时的实现修改而来，
/// 保留的目的是不大幅改动上层代码的同时，让所有块设备访问都经过块缓存

/// 从块设备读T
pub fn block_read<T: Clone + Sized>(
    dev: &Arc<dyn BlockDevice>, block_id: usize, offset: usize
) -> T {
    let cache = get_block_cache(block_id, dev.clone());
    let block = cache.lock();
    block.read(offset, Clone::clone)
}
/// 从块设备（经过页缓存）改T
pub fn block_modify<T: Sized>(
    dev: &Arc<dyn BlockDevice>, 
    block_id: usize, 
    offset: usize, 
    f: impl FnOnce(&mut T)
) {
    let cache = get_block_cache(block_id, dev.clone());
    cache.lock().modify(offset, f);
}
/// 从Ext4FS（经过页缓存）改inode
pub fn block_modify_inode(
    fs: &Arc<Ext4FS>, 
    inode_id: u32, 
    f: impl FnOnce(&mut Ext4InodeDisk)
) {
    let (block_id, offset) = fs.get_inode_pos(inode_id);
    block_modify(&fs.block_dev, block_id as usize, offset, f);
}
