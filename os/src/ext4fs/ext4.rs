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

    pub fn get_inode_pos(&self, inode_id: u32) -> (u32, usize) {
        // 获取参数
        let inode_size = self.superblock.inode_size;
        let inodes_per_group = self.superblock.inodes_per_group;
        let block_size = self.superblock.block_size;
        // 计算块组号和组内索引
        let group_idx = (inode_id - 1) / inodes_per_group;//组号（节点x在第几个块组）
        let inode_idx = (inode_id - 1) % inodes_per_group;//组内偏移（节点x在该块组所有节点中排第几个）

        let group = self.block_groups[group_idx as usize].lock();//获取块组
        let inode_table_start = group.inode_table_id;//获取 inode 表起始块号 注意：这里不是组内偏移，而是全局块号

        let byte_offset = (inode_idx as u32) * inode_size;//计算 inode 在 inode 表中的字节偏移
        let block_offset = byte_offset / block_size;//计算 inode 所在的块偏移，即第几个块
        let offset_in_block = byte_offset % block_size;//计算 inode 在块内的偏移

        (inode_table_start + block_offset, offset_in_block as usize)
    }
    pub fn get_disk_inode(&self, inode_id: u32) -> Ext4InodeDisk {
        let (block_id, offset) = self.get_inode_pos(inode_id);
        let block_cache = get_block_cache(block_id as usize, self.block_dev.clone());
        let block_cache = block_cache.lock();
        block_cache.read(offset, |disk_inode: &Ext4InodeDisk| {
            disk_inode.clone()
        })
    }
}