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
pub use cache::{block_cache_sync_all, get_block_cache};
pub use ext4::Ext4FS;
pub use crate::ext4fs::ext4inode::{Ext4InodeDisk, Ext4Inode};

pub const BLOCK_SZ: usize = 4096;

/// 下面三个辅助函数基于以前显式访问块缓存时的实现修改而来，
/// 保留的目的是不大幅改动上层代码的同时，让所有块设备访问都经过块缓存

/// 从块设备读T
pub fn block_read<T: Clone + Sized>(
    dev: &Arc<dyn BlockDevice>, block_id: usize, offset: usize
) -> T {
    let mut buf = [0u8; BLOCK_SZ];
    dev.read_block(block_id, &mut buf);
    unsafe { (&*(buf.as_ptr().add(offset) as *const T)).clone() }
}
/// 从块设备读，改，写T
pub fn block_modify<T: Sized>(
    dev: &Arc<dyn BlockDevice>, 
    block_id: usize, 
    offset: usize, 
    f: impl FnOnce(&mut T)
) {
    let mut buf = [0u8; BLOCK_SZ];
    dev.read_block(block_id, &mut buf);
    f(unsafe { &mut *(buf.as_mut_ptr().add(offset) as *mut T) });
    dev.write_block(block_id, &buf);
}
/// 从Ext4FS读，改，写inode
pub fn block_modify_inode(
    fs: &Arc<Ext4FS>, 
    inode_id: u32, 
    f: impl FnOnce(&mut Ext4InodeDisk)
) {
    let (block_id, offset) = fs.get_inode_pos(inode_id);
    let mut buf = [0u8; BLOCK_SZ];
    fs.block_dev.read_block(block_id as usize, &mut buf);
    f(unsafe { &mut *(buf.as_mut_ptr().add(offset) as *mut Ext4InodeDisk) });
    fs.block_dev.write_block(block_id as usize, &buf);
}
