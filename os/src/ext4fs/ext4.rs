use super::*;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

#[allow(dead_code)]
pub struct Ext4FS{
    block_dev: Arc<dyn BlockDevice>,
    superblock: Ext4SuperBlock,
    block_groups: Vec<Arc<Mutex<Ext4Group>>>,
}

impl Ext4FS {
    pub fn open(block_dev: Arc<dyn BlockDevice>) -> Self {
        let superblock = Ext4SuperBlock::new(Ext4SuperBlockDisk::new(block_dev.clone()));
        let group_num = superblock.group_num();
        let mut block_groups = Vec::new();

        let block_cache1 = get_block_cache(1, block_dev.clone());
        let block_cache1 = block_cache1.lock();
        
        for i in 0..group_num {
            // 一个 Ext4 组描述符是 32 字节
            // 从块缓存的第 (i * 32) 个字节开始，读取接下来的 32 字节
            let group = block_cache1.read((i * 32) as usize, |x: &Ext4GroupDescDisk| {
                Ext4Group::new(x)
            });
            println!("[Ext4] Group {}: block_bitmap={}, inode_bitmap={}, inode_table={}, free_blocks={}",
                i, group.block_bitmap_id, group.inode_bitmap_id, group.inode_table_id, group.free_blocks_count);
            block_groups.push(Arc::new(Mutex::new(group)));
        }

        Self {
            block_dev,
            superblock,
            block_groups,
        }
    }
}