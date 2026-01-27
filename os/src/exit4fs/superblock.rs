use super::{
    block_cache_sync_all, get_block_cache, Bitmap, BlockDevice, DiskInode, DiskInodeType, Inode,
    SuperBlock,
};
use super::BLOCK_SZ;
use alloc::sync::Arc;
use spin::Mutex;

pub struct Ext4_SuperBlock {
    pub total_blocks: u32,
    pub total_inodes: u32,
    pub block_size: u32,
    pub inode_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub first_data_block: u32,
}

impl Ext4_SuperBlock {
    pub fn new(total_blocks: u32, total_inodes: u32) -> Self {
        
    }
}