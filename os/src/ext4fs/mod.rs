pub mod superblock;
pub mod blockgroup;
pub mod ext4inode;
pub mod ext4;
pub mod vfs;
pub mod ext4_dir_entry;
pub mod checksum;
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
    block_modify_inode_raw(fs, inode_id, |raw_inode| {
        let disk_inode = unsafe {
            &mut *(raw_inode.as_mut_ptr() as *mut Ext4InodeDisk)
        };
        f(disk_inode);
    });
}

/// Modify an inode through its complete on-disk byte representation and
/// refresh metadata_csum before releasing the cache lock.  The legacy typed
/// inode view is intentionally kept as a small compatibility wrapper above;
/// checksum calculation must never be limited to that 128-byte view.
pub fn block_modify_inode_raw(
    fs: &Arc<Ext4FS>,
    inode_id: u32,
    f: impl FnOnce(&mut [u8]),
) {
    let (block_id, offset) = fs.get_inode_pos(inode_id);
    let cache = get_block_cache(block_id as usize, fs.block_dev.clone());
    let mut block = cache.lock();
    let inode_size = fs.superblock.inode_size as usize;
    let bytes = block.frame.get_bytes_array();
    let raw_inode = &mut bytes[offset..offset + inode_size];
    f(raw_inode);
    if fs.superblock.has_metadata_csum() {
        let _ = checksum::set_inode_checksum(
            fs.superblock.metadata_checksum_seed(),
            inode_id,
            raw_inode,
        );
    }
    block.dirty = true;
    block.state = crate::drivers::block::cache::CacheState::Dirty;
}
