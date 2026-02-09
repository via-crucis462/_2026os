use super::*;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

#[allow(dead_code)]
pub struct Ext4FS{
    pub block_dev: Arc<dyn BlockDevice>,
    pub superblock: Ext4SuperBlock,
    pub block_groups: Vec<Arc<Mutex<Ext4Group>>>,
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
            let group = block_cache1.read((i * 32) as *const () as usize, |x: &Ext4GroupDescDisk| {
                Ext4Group::new(x)
            });
            debug!(
                "[Ext4] Group {}: block_bitmap={}, inode_bitmap={}, inode_table={}, free_blocks={}",
                i, group.block_bitmap_id, group.inode_bitmap_id, group.inode_table_id, group.free_blocks_count
            );
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

        let group = self.block_groups[group_idx as *const () as usize].lock();//获取块组
        let inode_table_start = group.inode_table_id;//获取 inode 表起始块号 注意：这里不是组内偏移，而是全局块号

        let byte_offset = (inode_idx as u32) * inode_size;//计算 inode 在 inode 表中的字节偏移
        let block_offset = byte_offset / block_size;//计算 inode 所在的块偏移，即第几个块
        let offset_in_block = byte_offset % block_size;//计算 inode 在块内的偏移

        (inode_table_start + block_offset, offset_in_block as *const () as usize)
    }
    pub fn get_disk_inode(&self, inode_id: u32) -> Ext4InodeDisk {
        let (block_id, offset) = self.get_inode_pos(inode_id);
        let block_cache = get_block_cache(block_id as *const () as usize, self.block_dev.clone());
        let block_cache = block_cache.lock();
        block_cache.read(offset, |disk_inode: &Ext4InodeDisk| {
            disk_inode.clone()
        })
    }
    pub fn get_inode(self: &Arc<Self>, inode_id: u32) -> Arc<Ext4Inode> {
        let disk_inode = self.get_disk_inode(inode_id);
        Arc::new(Ext4Inode::new(inode_id, &disk_inode, self.clone(), None))
    }

    pub fn alloc_inode(&self) -> Option<u32> {
        // 1. 遍历块组，找到有空闲 Inode 的组
        for (group_id, group_mutex) in self.block_groups.iter().enumerate() {
            let mut group = group_mutex.lock();
            if group.free_inodes_count > 0 {
                // 读取 Inode 位图块
                let bitmap_block = group.inode_bitmap_id;
                let block_cache = get_block_cache(bitmap_block as *const () as usize, self.block_dev.clone());
                let mut bitmap_cache = block_cache.lock();

                // 在位图中查找第一个空闲位 (0)
                let res = bitmap_cache.modify(0, |bitmap: &mut [u8; 4096]| {
                    for byte_idx in 0..4096 {
                        if bitmap[byte_idx] != 0xFF {
                            for bit_idx in 0..8 {
                                if (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                                    bitmap[byte_idx] |= 1 << bit_idx;
                                    return Some((byte_idx, bit_idx));
                                }
                            }
                        }
                    }
                    None
                });

                if let Some((byte_idx, bit_idx)) = res {
                    // 更新组描述符（内存中）
                    group.free_inodes_count -= 1;
                    // 计算全局 Inode ID
                    let inode_per_group = self.superblock.inodes_per_group;
                    let inode_id = (group_id as u32) * inode_per_group + (byte_idx as u32 * 8) + bit_idx as u32 + 1;
                    return Some(inode_id);
                }
            }
        }
        None
    }

    pub fn alloc_block(&self) -> Option<u32> {
        for (group_id, group_mutex) in self.block_groups.iter().enumerate() {
            let mut group = group_mutex.lock();
            if group.free_blocks_count > 0 {
                let bitmap_block = group.block_bitmap_id;
                let block_cache = get_block_cache(bitmap_block as *const () as usize, self.block_dev.clone());
                let mut bitmap_cache = block_cache.lock();

                let res = bitmap_cache.modify(0, |bitmap: &mut [u8; 4096]| {
                    for byte_idx in 0..4096 {
                        if bitmap[byte_idx] != 0xFF {
                            for bit_idx in 0..8 {
                                if (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                                    bitmap[byte_idx] |= 1 << bit_idx;
                                    return Some((byte_idx, bit_idx));
                                }
                            }
                        }
                    }
                    None
                });

                if let Some((byte_idx, bit_idx)) = res {
                    group.free_blocks_count -= 1;
                    let blocks_per_group = self.superblock.blocks_per_group;
                    let first_data_block = self.superblock.first_data_block;
                    let block_id = (group_id as u32) * blocks_per_group + (byte_idx as u32 * 8) + bit_idx as u32 + first_data_block;
                    return Some(block_id);
                }
            }
        }
        None
    }

    pub fn dealloc_inode(&self, inode_id: u32) {
        let inodes_per_group = self.superblock.inodes_per_group;
        let group_idx = (inode_id - 1) / inodes_per_group;
        let inode_idx = (inode_id - 1) % inodes_per_group;

        let mut group = self.block_groups[group_idx as usize].lock();
        let bitmap_block = group.inode_bitmap_id;
        let block_cache = get_block_cache(bitmap_block as usize, self.block_dev.clone());
        let mut bitmap_cache = block_cache.lock();
        
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = inode_idx % 8;
        
        bitmap_cache.modify(0, |bitmap: &mut [u8; 4096]| {
            bitmap[byte_idx] &= !(1 << bit_idx);
        });

        group.free_inodes_count += 1;
    }

    pub fn dealloc_block(&self, block_id: u32) {
        if block_id == 0 { return; }
        let blocks_per_group = self.superblock.blocks_per_group;
        let first_data_block = self.superblock.first_data_block;

        let relative_block_id = block_id - first_data_block;
        let group_idx = relative_block_id / blocks_per_group;
        let block_idx = relative_block_id % blocks_per_group;

        let mut group = self.block_groups[group_idx as usize].lock();
        let bitmap_block = group.block_bitmap_id;
        let block_cache = get_block_cache(bitmap_block as usize, self.block_dev.clone());
        let mut bitmap_cache = block_cache.lock();

        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = block_idx % 8;

        bitmap_cache.modify(0, |bitmap: &mut [u8; 4096]| {
            bitmap[byte_idx] &= !(1 << bit_idx);
        });

        group.free_blocks_count += 1;
    }

    pub fn decrease_link_count(&self, inode_id: u32) -> u16 {
        let (block_id, offset) = self.get_inode_pos(inode_id);
        let block_cache = get_block_cache(block_id as usize, self.block_dev.clone());
        let mut cache = block_cache.lock();
        cache.modify(offset, |disk_inode: &mut Ext4InodeDisk| {
            if disk_inode.i_links_count > 0 {
                disk_inode.i_links_count -= 1;
            }
            disk_inode.i_links_count
        })
    }
}
