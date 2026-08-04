use super::*;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

#[allow(dead_code)]
pub struct Ext4FS{
    pub block_dev: Arc<dyn BlockDevice>,
    pub superblock: Ext4SuperBlock,
    pub block_groups: Vec<Arc<Mutex<Ext4Group>>>,
    /// inode 缓存表：ino -> Weak<Ext4Inode>
    /// 
    /// 从磁盘读取 inode 前先查表；
    /// 从磁盘中读取一个 inode 时，会将其注册（缓存）到表中；
    /// 后续访问时，若缓存中存在则直接使用。
    /// 所有已注册表项不应当被主动删除。
    pub inodes: Mutex<BTreeMap<u32, Weak<Ext4Inode>>>,
}

impl Ext4FS {
    pub fn open(block_dev: Arc<dyn BlockDevice>) -> Self {
        let superblock = Ext4SuperBlock::new(Ext4SuperBlockDisk::new(block_dev.clone()));
        let group_num = superblock.group_num();
        let mut block_groups = Vec::new();

        let desc_size = superblock.desc_size as usize;
        // 组描述符表可能跨多个块，全部读入内核缓冲区再解析
        let desc_blocks = (group_num as usize * desc_size + BLOCK_SZ - 1) / BLOCK_SZ;
        let mut buf = alloc::vec![0u8; desc_blocks * BLOCK_SZ];
        for b in 0..desc_blocks {
            block_dev.read_block(1 + b, &mut buf[b * BLOCK_SZ..(b + 1) * BLOCK_SZ]);
        }

        for i in 0..group_num {
            let x = unsafe { &*(buf.as_ptr().add(i as usize * desc_size) as *const Ext4GroupDescDisk) };
            let group = Ext4Group::new(x);
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
            inodes: Mutex::new(BTreeMap::new()),
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
        block_read(&self.block_dev, block_id as usize, offset)
    }
    /// 获取 inode 对象，若缓存中不存在则从磁盘读取并创建新对象
    pub fn get_inode(self: &Arc<Self>, inode_id: u32) -> Arc<Ext4Inode> {
        let mut inodes = self.inodes.lock();
        // 命中缓存：所有引用者共享同一个 Arc，Arc 引用计数即该 ino 的内存引用数
        if let Some(arc) = inodes.get(&inode_id).and_then(|w| w.upgrade()) {
            return arc;
        }
        // 缓存缺失或 Weak 已失效：在锁内读盘并重建，避免并发 miss 产生重复对象
        let disk_inode = self.get_disk_inode(inode_id);
        let arc = Arc::new(Ext4Inode::new(inode_id, &disk_inode, self.clone(), None));
        inodes.insert(inode_id, Arc::downgrade(&arc));
        arc
    }

    pub fn alloc_inode(&self) -> Option<u32> {
        for (group_id, group_mutex) in self.block_groups.iter().enumerate() {
            let mut group = group_mutex.lock();
            if group.free_inodes_count > 0 {
                let bitmap_block = group.inode_bitmap_id;
                let mut buf = [0u8; BLOCK_SZ];
                self.block_dev.read_block(bitmap_block as usize, &mut buf);
                let bitmap: &mut [u8; 4096] = unsafe { &mut *(buf.as_mut_ptr() as *mut [u8; 4096]) };

                let mut found = None;
                for byte_idx in 0..4096 {
                    if bitmap[byte_idx] != 0xFF {
                        for bit_idx in 0..8 {
                            if (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                                bitmap[byte_idx] |= 1 << bit_idx;
                                found = Some((byte_idx, bit_idx));
                                break;
                            }
                        }
                        if found.is_some() { break; }
                    }
                }

                if let Some((byte_idx, bit_idx)) = found {
                    self.block_dev.write_block(bitmap_block as usize, &buf);
                    group.free_inodes_count -= 1;
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
                let mut buf = [0u8; BLOCK_SZ];
                self.block_dev.read_block(bitmap_block as usize, &mut buf);
                let bitmap: &mut [u8; 4096] = unsafe { &mut *(buf.as_mut_ptr() as *mut [u8; 4096]) };

                let mut found = None;
                for byte_idx in 0..4096 {
                    if bitmap[byte_idx] != 0xFF {
                        for bit_idx in 0..8 {
                            if (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                                bitmap[byte_idx] |= 1 << bit_idx;
                                found = Some((byte_idx, bit_idx));
                                break;
                            }
                        }
                        if found.is_some() { break; }
                    }
                }

                if let Some((byte_idx, bit_idx)) = found {
                    self.block_dev.write_block(bitmap_block as usize, &buf);
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
        let mut buf = [0u8; BLOCK_SZ];
        self.block_dev.read_block(bitmap_block as usize, &mut buf);
        let bitmap: &mut [u8; 4096] = unsafe { &mut *(buf.as_mut_ptr() as *mut [u8; 4096]) };

        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = inode_idx % 8;
        let bit = 1u8 << bit_idx;
        if bitmap[byte_idx] & bit == 0 {
            // 该 inode 已被释放过（例如失效对象重复 Drop），幂等返回，
            // 避免 free_inodes_count 重复统计
            return;
        }
        bitmap[byte_idx] &= !bit;
        self.block_dev.write_block(bitmap_block as usize, &buf);

        group.free_inodes_count += 1;
    }

    pub fn dealloc_block(&self, block_id: u32) {
        if block_id == 0 { return; }
        let blocks_per_group = self.superblock.blocks_per_group;
        let first_data_block = self.superblock.first_data_block;

        let relative_block_id = block_id - first_data_block;
        let group_idx = relative_block_id / blocks_per_group;
        let block_idx = relative_block_id % blocks_per_group;
        if group_idx as usize >= self.block_groups.len() {
            panic!(
                "dealloc_block out of range: block_id={} first_data_block={} blocks_per_group={} group_idx={} groups={}",
                block_id,
                first_data_block,
                blocks_per_group,
                group_idx,
                self.block_groups.len()
            );
        }

        let mut group = self.block_groups[group_idx as usize].lock();
        let bitmap_block = group.block_bitmap_id;
        let mut buf = [0u8; BLOCK_SZ];
        self.block_dev.read_block(bitmap_block as usize, &mut buf);
        let bitmap: &mut [u8; 4096] = unsafe { &mut *(buf.as_mut_ptr() as *mut [u8; 4096]) };

        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = block_idx % 8;
        bitmap[byte_idx] &= !(1 << bit_idx);
        self.block_dev.write_block(bitmap_block as usize, &buf);

        group.free_blocks_count += 1;

        // 物理块已释放：作废并丢弃它的缓存，不回写。
        // 否则旧文件的数据会留在缓存里，重新分配后新所有者会读到旧数据，
        // 脏缓存还可能把旧数据写回磁盘。
        invalidate_block_cache(block_id as usize);
    }

    pub fn decrease_link_count(&self, inode_id: u32) -> u16 {
        let (block_id, offset) = self.get_inode_pos(inode_id);
        block_modify(&self.block_dev, block_id as usize, offset, |disk_inode: &mut Ext4InodeDisk| {
            if disk_inode.i_links_count > 0 {
                disk_inode.i_links_count -= 1;
            }
        });
        // 读取写回后的值
        block_read::<Ext4InodeDisk>(&self.block_dev, block_id as usize, offset).i_links_count
    }

    pub fn adjust_link_count(&self, inode_id: u32, delta: i16) {
        let (block_id, offset) = self.get_inode_pos(inode_id);
        block_modify(&self.block_dev, block_id as usize, offset, |disk_inode: &mut Ext4InodeDisk| {
            if delta >= 0 {
                disk_inode.i_links_count = disk_inode.i_links_count.saturating_add(delta as u16);
            } else {
                disk_inode.i_links_count = disk_inode.i_links_count.saturating_sub((-delta) as u16);
            }
        });
    }
}
