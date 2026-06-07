use alloc::sync::Arc;
use alloc::vec;
use alloc::string::String;
use crate::ext4fs::BLOCK_SZ;

use super::{ext4::Ext4FS, ext4_dir_entry::Ext4DirEntry, block_cache::get_block_cache};
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4InodeDisk {
    pub i_mode: u16,        // File mode
    pub i_uid: u16,         // Low 16 bits of Owner Uid
    pub i_size_lo: u32,     // Size in bytes
    pub i_atime: u32,       // Access time
    pub i_ctime: u32,       // Creation time
    pub i_mtime: u32,       // Modification time
    pub i_dtime: u32,       // Deletion Time
    pub i_gid: u16,         // Low 16 bits of Group Id
    pub i_links_count: u16, // Links count
    pub i_blocks_lo: u32,   // Blocks count
    pub i_flags: u32,       // File flags
    pub l_i_osd1: u32,
    pub i_block: [u32; 15], // Pointers to blocks
    pub i_generation: u32,  // File version (for NFS)
    pub i_file_acl_lo: u32, // File ACL
    pub i_size_high: u32,
    pub i_obso_faddr: u32,
    pub l_i_osd2: [u8; 12],
}
#[repr(C, packed)]
struct Ext4ExtentHeader {
    pub eh_magic: u16,       // 魔数 0xF30A
    pub eh_entries: u16,     // 当前节点中的有效条目数
    pub eh_max: u16,         // 本节点最多能容纳的条目数
    pub eh_depth: u16,       // 树的深度（0 = 叶子节点）
    pub eh_generation: u32,  // 版本号
}
/// Ext4 Extent 叶子节点条目 (12 bytes)
/// 定义了一个连续物理块区间的映射关系
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentLeaf {
    pub ee_block: u32,      // 该 extent 覆盖的首个逻辑块号
    pub ee_len: u16,        // 覆盖的块数（>32768 表示未初始化）
    pub ee_start_hi: u16,   // 物理块号的高 16 位
    pub ee_start_lo: u32,   // 物理块号的低 32 位
}
impl Ext4ExtentLeaf {
    /// 返回实际块数（去掉未初始化标记位）
    pub fn actual_len(&self) -> u32 {
        if self.ee_len > 32768 {
            (self.ee_len - 32768) as u32
        } else {
            self.ee_len as u32
        }
    }

    /// 起始物理块号（48 位）
    pub fn start_phys(&self) -> u64 {
        ((self.ee_start_hi as u64) << 32) | (self.ee_start_lo as u64)
    }

    /// 物理结束位置（不含）
    pub fn phys_end(&self) -> u64 {
        self.start_phys() + self.actual_len() as u64
    }

    /// 逻辑结束位置（不含）
    pub fn logical_end(&self) -> u32 {
        self.ee_block + self.actual_len()
    }

    /// 序列化为 12 字节数组（小端），可直接写回 i_block
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&self.ee_block.to_le_bytes());
        buf[4..6].copy_from_slice(&self.ee_len.to_le_bytes());
        buf[6..8].copy_from_slice(&self.ee_start_hi.to_le_bytes());
        buf[8..12].copy_from_slice(&self.ee_start_lo.to_le_bytes());
        buf
    }
}
/// Ext4 Extent 索引节点条目 (12 bytes)
/// 指向下一层级的 extent 节点块
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentIndex {
    pub ei_block: u32,      // 该索引覆盖的首个逻辑块号
    pub ei_leaf_lo: u32,    // 下一级节点物理块号的低 32 位
    pub ei_leaf_hi: u16,    // 下一级节点物理块号的高 16 位
    pub ei_unused: u16,     // 未使用（填充）
}
impl Ext4ExtentIndex {
    /// 下一级节点的物理块号（48 位）
    pub fn leaf_phys(&self) -> u64 {
        ((self.ei_leaf_hi as u64) << 32) | (self.ei_leaf_lo as u64)
    }
}

impl From<[u8; 12]> for Ext4ExtentHeader {
    fn from(bytes: [u8; 12]) -> Self {
        Self {
            eh_magic: u16::from_le_bytes(bytes[0..2].try_into().unwrap()),
            eh_entries: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            eh_max: u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
            eh_depth: u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            eh_generation: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        }
    }
}
impl From<[u8; 12]> for Ext4ExtentLeaf {
    fn from(bytes: [u8; 12]) -> Self {
        Self {
            ee_block: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            ee_len: u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
            ee_start_hi: u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            ee_start_lo: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        }
    }
}
impl From<[u8; 12]> for Ext4ExtentIndex {
    fn from(bytes: [u8; 12]) -> Self {
        Self {
            ei_block: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            ei_leaf_lo: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            ei_leaf_hi: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            ei_unused: u16::from_le_bytes(bytes[10..12].try_into().unwrap()),
        }
    }
}


impl Ext4InodeDisk {
    pub fn is_dir(&self) -> bool {
        self.i_mode & 0xF000 == 0x4000
    }
    
    pub fn is_file(&self) -> bool {
        self.i_mode & 0xF000 == 0x8000
    }
    
    pub fn size(&self) -> u64 {
        ((self.i_size_high as u64) << 32) | (self.i_size_lo as u64)
    }
}
pub struct Ext4Inode {
    /// Inode 编号
    pub inode_id: u32,
    /// 文件类型（目录/普通文件）
    pub mode: u16,
    /// 文件大小
    pub size: u64,
    /// 标志位 (例如是否使用 Extents)
    pub flags: u32,
    /// 数据块指针（直接块、间接块等）
    pub i_block: [u32; 15],
    /// 块设备
    pub fs: Arc<Ext4FS>,
    /// 父目录 Inode 编号（可选）
    pub parent: Option<u32>,
}

pub const EXT4_EXTENTS_FL: u32 = 0x80000;

impl Ext4Inode {
    const EXT4_FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
    const EXT4_FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;

    fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0x82F63B78 & mask);
            }
        }
        crc
    }

    fn ext4_checksum_seed(&self) -> u32 {
        let sb = &self.fs.superblock;
        if (sb.incompat_features & Self::EXT4_FEATURE_INCOMPAT_CSUM_SEED) != 0 {
            sb.checksum_seed
        } else {
            Self::crc32c_update(!0u32, &sb.uuid)
        }
    }

    pub fn update_dir_block_checksum_if_needed(&self, block_buf: &mut [u8]) {
        if !self.is_dir() {
            return;
        }
        if (self.fs.superblock.ro_compat_features & Self::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM) == 0 {
            return;
        }
        if block_buf.len() < BLOCK_SZ {
            return;
        }

        let tail_off = BLOCK_SZ - 12;
        let rec_len = u16::from_le_bytes([block_buf[tail_off + 4], block_buf[tail_off + 5]]);
        let reserved_zero2 = block_buf[tail_off + 6];
        let reserved_ft = block_buf[tail_off + 7];
        if rec_len != 12 || reserved_zero2 != 0 || reserved_ft != 0xDE {
            return;
        }

        // 清零 checksum 字段后重新计算，避免把旧值带入。
        block_buf[tail_off + 8..tail_off + 12].fill(0);

        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let mut crc = self.ext4_checksum_seed();
        crc = Self::crc32c_update(crc, &self.inode_id.to_le_bytes());
        crc = Self::crc32c_update(crc, &disk_inode.i_generation.to_le_bytes());
        // ext4 dir block checksum covers bytes before the 12-byte fake tail.
        crc = Self::crc32c_update(crc, &block_buf[..tail_off]);

        block_buf[tail_off + 8..tail_off + 12].copy_from_slice(&crc.to_le_bytes());
    }

    pub fn new(inode_id: u32, disk_inode: &Ext4InodeDisk, fs: Arc<Ext4FS>, parent: Option<u32>) -> Self {
        Self {
            inode_id,
            mode: disk_inode.i_mode,
            size: disk_inode.size(), // 使用 DiskInode 已有的方法计算大小
            flags: disk_inode.i_flags,
            i_block: disk_inode.i_block,
            fs,
            parent,
        }
    }    /// 定义一个高层接口，专门用于解析目录项
    pub fn is_dir(&self) -> bool {
        self.mode & 0xF000 == 0x4000
    }

    pub fn is_file(&self) -> bool {
        self.mode & 0xF000 == 0x8000
    }

    pub fn is_symlink(&self) -> bool {
        self.mode & 0xF000 == 0xA000
    }

    /// 根据逻辑块号寻找对应的物理块号 (支持 Extents 和直接块)
    pub fn find_physical_block(&self, logical_block_id: u32) -> u32 {
        // 实时获取磁盘 Inode，避免 self.i_block 与磁盘不同步
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let flags = disk_inode.i_flags;
        let i_block = disk_inode.i_block;

        if self.is_symlink() && (disk_inode.size() as usize) < 60 {
            return 0;
        }

        if flags & 0x80000 != 0 {
            // Extents 模式 (EXT4_EXTENTS_FL = 0x80000)

            // 将 i_block 转为字节数组以便统一处理逻辑
            let mut header_buf = [0u8; 60];
            for i in 0..15 {
                let bytes = i_block[i].to_le_bytes();
                header_buf[i * 4..(i + 1) * 4].copy_from_slice(&bytes);
            }

            // 使用堆分配 buffer 避免 kernel stack 溢出 (8KB 栈下 4KB 数组非常危险)
            let mut current_block_data = alloc::boxed::Box::new([0u8; 4096]);
            let mut data_ptr: &[u8] = &header_buf;

            loop {
                // Extent Header (12 bytes)
                if data_ptr.len() < 12 { return 0; }
                let header = Ext4ExtentHeader::from(<[u8; 12]>::try_from(&data_ptr[0..12]).unwrap());
                if header.eh_magic != 0xF30A { return 0; }

                if header.eh_depth == 0 {
                    // 叶子节点 (Leaf Node)
                    // 在 Inode 中最多只有 4 个 entry
                    let max_entries = if data_ptr.len() == 60 { 4 } else { 340 };
                    let actual_entries = header.eh_entries.min(max_entries) as usize;

                    for i in 0..actual_entries {
                        let off = 12 + i * 12;
                        if off + 12 > data_ptr.len() { break; }
                        let leaf = Ext4ExtentLeaf::from(<[u8; 12]>::try_from(&data_ptr[off..off + 12]).unwrap());

                        if logical_block_id >= leaf.ee_block
                            && logical_block_id < leaf.logical_end()
                        {
                            let offset = (logical_block_id - leaf.ee_block) as u64;
                            return (leaf.start_phys() + offset) as u32;
                        }
                    }
                    return 0; // 未在 Extents 中找到该逻辑块
                } else {
                    // 索引节点 (Index Node)
                    let max_entries = if data_ptr.len() == 60 { 4 } else { 340 };
                    let mut found_index: usize = 0;
                    let actual_entries = header.eh_entries.min(max_entries) as usize;

                    for i in 0..actual_entries {
                        let off = 12 + i * 12;
                        let idx = Ext4ExtentIndex::from(<[u8; 12]>::try_from(&data_ptr[off..off + 12]).unwrap());
                        if logical_block_id >= idx.ei_block {
                            found_index = i;
                        } else {
                            break;
                        }
                    }

                    let off = 12 + found_index * 12;
                    let idx = Ext4ExtentIndex::from(<[u8; 12]>::try_from(&data_ptr[off..off + 12]).unwrap());
                    let next_block = idx.leaf_phys();

                    // 加载下一层级的数据块并继续搜索
                    self.fs.block_dev.read_block(next_block as usize, current_block_data.as_mut_slice());
                    data_ptr = current_block_data.as_slice();
                }
            }
        } else {
            // 传统的直接块模式
            if (logical_block_id as usize) < 12 {
                i_block[logical_block_id as usize]
            } else {
                0 // 目前尚不支持一级/二级/三级间接块
            }
        }
    }

    pub fn add_extent_entry(&self, logical_block_id: u32, physical_block_id: u32) -> Option<u32> {
        let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
        let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        let mut cache = block_cache.lock();
        
        cache.modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
            let mut extent_header = Ext4ExtentHeader::from({
                let mut buf = [0u8; 12];
                for i in 0..3 {
                    buf[i * 4..(i + 1) * 4].copy_from_slice(&disk_inode.i_block[i].to_le_bytes());
                }
                buf
            });

            if extent_header.eh_magic != 0xF30A {
                trace!("VFS: add_extent_entry - invalid magic 0x{:X}", extent_header.eh_magic as usize);
                return None;
            }

            let mut cur_depth = 0;
            const MAX_DEPTH: u16 = 4; // 目前仅支持单层 Extent，超过深度限制则不处理
            while cur_depth <= MAX_DEPTH {
                // 尝试合并到最后一个 extent（逻辑块和物理块都连续时扩展 ee_len）
                if cur_depth == 0 {
                    // 叶子节点，直接在 i_block 中寻找
                    if extent_header.eh_entries > 0 {
                        let last_idx = 3 + (extent_header.eh_entries as usize - 1) * 3;
                        let last_leaf = Ext4ExtentLeaf::from({
                            let mut buf = [0u8; 12];
                            for i in 0..3 {
                                buf[i * 4..(i + 1) * 4]
                                    .copy_from_slice(&disk_inode.i_block[last_idx + i].to_le_bytes());
                            }
                            buf
                        });

                        if logical_block_id == last_leaf.logical_end()
                            && physical_block_id as u64 == last_leaf.phys_end()
                        {
                            // 逻辑块和物理块都连续，直接扩展 ee_len
                            let new_len = last_leaf.actual_len() + 1;
                            let new_len_raw = if last_leaf.ee_len > 32768 {
                                (new_len as u16) + 32768
                            } else {
                                new_len as u16
                            };
                            let updated = Ext4ExtentLeaf {
                                ee_len: new_len_raw,
                                ..last_leaf
                            };
                            let bytes = updated.to_bytes();
                            for i in 0..3 {
                                disk_inode.i_block[last_idx + i] =
                                    u32::from_le_bytes(bytes[i * 4..(i + 1) * 4].try_into().unwrap());
                            }
                            disk_inode.i_blocks_lo += 8; // 4096 / 512
                            return Some(physical_block_id);
                        }
                    }
                } else {
                    
                }

                // 无法合并，创建新的 extent entry
                if extent_header.eh_entries < extent_header.eh_max {
                    let new_leaf = Ext4ExtentLeaf {
                        ee_block: logical_block_id,
                        ee_len: 1,
                        ee_start_hi: 0,
                        ee_start_lo: physical_block_id,
                    };
                    let entry_idx = 3 + (extent_header.eh_entries as usize) * 3;
                    if entry_idx + 3 > 15 { return None; }
                    let bytes = new_leaf.to_bytes();
                    for i in 0..3 {
                        disk_inode.i_block[entry_idx + i] =
                            u32::from_le_bytes(bytes[i * 4..(i + 1) * 4].try_into().unwrap());
                    }

                    extent_header.eh_entries += 1;
                    disk_inode.i_block[0] = (disk_inode.i_block[0] & 0xFFFF) | ((extent_header.eh_entries as u32) << 16);
                    disk_inode.i_blocks_lo += 8; // 4096 / 512
                    return Some(physical_block_id);
                } else {
                    trace!("VFS: add_extent_entry - no space for more entries, eh_entries={}, eh_max={}", extent_header.eh_entries as usize, extent_header.eh_max as usize);
                    cur_depth += 1;
                }
            }
            None
        })
    }

    pub fn read_dirents(&self) {
        if !self.is_dir() {
            return;
        }
        let mut offset = 0;
        let file_size = self.size as usize;

        while offset < file_size {
            // 1. 先读 8 个字节拿到头部 (inode, rec_len, name_len, file_type)
            let mut header_buf = [0u8; 8];
            self.raw_read_at(offset, &mut header_buf);
            
            let inode_id = u32::from_le_bytes(header_buf[0..4].try_into().unwrap());
            let rec_len = u16::from_le_bytes(header_buf[4..6].try_into().unwrap()) as usize;
            let name_len = header_buf[6] as usize;

            if rec_len == 0 { break; } // 防止死循环

            // 2. 如果 inode_id 不为 0，说明这是一个有效的项
            if inode_id != 0 {
                // 读取文件名
                let mut name_buf = vec![0u8; name_len];
                self.raw_read_at(offset + 8, &mut name_buf);
                let name = String::from_utf8_lossy(&name_buf);
                trace!("Found: {}", name);
            }

            // 3. 将偏移量顺移到下一个目录项的起始位置
            // 注意：ext4 用 rec_len 来跳跃，这对应了线性表的逻辑
            offset += rec_len;
        }
    }
    /// 底层原始读取：直接从磁盘块设备读取数据，不经过页缓存
    pub fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let block_size = BLOCK_SZ as usize;
        let mut actual_read = 0;
        let mut curr_offset = offset;

        // 实时获取磁盘 Inode 信息以获取准备的大小
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let disk_size_bytes = disk_inode.size() as usize;
        
        let end = core::cmp::min(offset + buf.len(), disk_size_bytes);
        if curr_offset >= end { return 0; }

        if self.is_symlink() && disk_size_bytes < 60 {
            let mut i_block_bytes = [0u8; 60];
            for i in 0..15 {
                i_block_bytes[i * 4..(i + 1) * 4].copy_from_slice(&disk_inode.i_block[i].to_le_bytes());
            }
            let read_len = end - curr_offset;
            buf[0..read_len].copy_from_slice(&i_block_bytes[curr_offset..end]);
            return read_len;
        }

        while curr_offset < end {
            let inner_block_id = (curr_offset / block_size) as u32;
            let block_pos = curr_offset % block_size;
            
            let physical_block_id = self.find_physical_block(inner_block_id);
            let read_len = core::cmp::min(block_size - block_pos, end - curr_offset);

            if physical_block_id == 0 {
                // 如果是空洞 (Hole)，填充 0 而不是停止读取
                // 这样能确保返回给上层的 buffer 长度符合预期，不会因截断导致 ELF 解析失败
                buf[actual_read..actual_read + read_len].fill(0);
            } else {
                let mut temp_buf = alloc::vec![0u8; 4096];
                self.fs.block_dev.read_block(physical_block_id as usize, &mut temp_buf);
                buf[actual_read..actual_read + read_len].copy_from_slice(&temp_buf[block_pos..block_pos + read_len]);
            }
            
            actual_read += read_len;
            curr_offset += read_len;
        }

        actual_read
    }

    /// 底层原始写入：直接写入磁盘块设备，不经过页缓存
    pub fn raw_write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let block_size = BLOCK_SZ as usize;
        let mut actual_write = 0;
        let mut curr_offset = offset;

        let old_size_bytes = self.fs.get_disk_inode(self.inode_id).size() as usize;
        let end = offset + buf.len();
        if self.is_symlink() && end <= 60 {
            let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
            let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
            block_cache.lock().modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
                let mut i_block_bytes = [0u8; 60];
                // 1. 先把原有的 i_block 数据读出来
                for i in 0..15 {
                    i_block_bytes[i * 4..(i + 1) * 4].copy_from_slice(&disk_inode.i_block[i].to_le_bytes());
                }
                // 2. 将新的字符串覆盖进去
                i_block_bytes[offset..end].copy_from_slice(buf);
                // 3. 写回 i_block (转换回 u32 数组)
                for i in 0..15 {
                    disk_inode.i_block[i] = u32::from_le_bytes(i_block_bytes[i * 4..(i + 1) * 4].try_into().unwrap());
                }
                
                // 4. 更新磁盘 Inode 大小
                if end > old_size_bytes {
                    disk_inode.i_size_lo = end as u32;
                    disk_inode.i_size_high = 0;
                }
            });
            return buf.len(); // 写入成功，直接返回
        }
        while curr_offset < end {
            let inner_block_id = (curr_offset / block_size) as u32;
            let block_pos = curr_offset % block_size;

            let mut physical_block_id = self.find_physical_block(inner_block_id);
            if physical_block_id == 0 {
                // 如果块不存在，尝试分配
                if let Some(new_block_id) = self.fs.alloc_block() {
                    let disk_inode = self.fs.get_disk_inode(self.inode_id);
                    if disk_inode.i_flags & EXT4_EXTENTS_FL == 0 {
                        // 传统的直接块模式
                        if inner_block_id < 12 {
                            let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
                            let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
                            block_cache.lock().modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
                                disk_inode.i_block[inner_block_id as usize] = new_block_id;
                                disk_inode.i_blocks_lo += (block_size / 512) as u32;
                            });
                            physical_block_id = new_block_id;
                        } else {
                            trace!("VFS: write_at - indirect blocks not supported");
                            break;
                        }
                    } else {
                        // Extent 模式下的块分配
                        if let Some(phys) = self.add_extent_entry(inner_block_id, new_block_id) {
                            physical_block_id = phys;
                        } else {
                            error!("VFS: write_at - extent allocation failed or not supported for depth > 0");
                            break;
                        }
                    }
                } else {
                    trace!("VFS: write_at - no free blocks");
                    break;
                }
            }

            let mut temp_buf = alloc::vec![0u8; 4096];
            self.fs.block_dev.read_block(physical_block_id as usize, &mut temp_buf);

            let write_len = core::cmp::min(block_size - block_pos, end - curr_offset);
            temp_buf[block_pos..block_pos + write_len].copy_from_slice(&buf[actual_write..actual_write + write_len]);

            self.fs.block_dev.write_block(physical_block_id as usize, &temp_buf);

            actual_write += write_len;
            curr_offset += write_len;
        }

        // 更新磁盘 Inode 的 size (以字节为单位)
        let new_size_bytes = offset + actual_write;
        
        if new_size_bytes > old_size_bytes {
            let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
            let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
            block_cache.lock().modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
                disk_inode.i_size_lo = new_size_bytes as u32;
                disk_inode.i_size_high = (new_size_bytes >> 32) as u32;
            });
        }

        actual_write
    }

    pub fn add_dir_entry(&self, name: &str, inode_id: u32, file_type: u8) -> bool {
        let mut offset = 0;
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let file_size_bytes = disk_inode.size() as usize;
        
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            self.raw_read_at(offset, &mut buf);
            
            let mut block_offset = 0;
            while block_offset < BLOCK_SZ {
                let dirent = unsafe { &mut *(buf[block_offset..].as_ptr() as *mut Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                
                if rec_len == 0 { break; } 
                
                let real_len = dirent.real_len() as usize;
                let needed_len = ((8 + name.len() + 3) & !3) as usize;
                
                if rec_len >= real_len + needed_len {
                    let old_rec_len = dirent.rec_len;
                    dirent.rec_len = real_len as u16;
                    
                    let new_offset = block_offset + real_len;
                    let new_rec_len = old_rec_len - (real_len as u16);
                    let new_dirent = Ext4DirEntry::new_disk(inode_id, new_rec_len, name, file_type);
                    
                    let new_dirent_bytes = unsafe {
                        core::slice::from_raw_parts(&new_dirent as *const _ as *const u8, 8 + new_dirent.name_len as usize)
                    };
                    buf[new_offset..new_offset + new_dirent_bytes.len()].copy_from_slice(new_dirent_bytes);

                    self.update_dir_block_checksum_if_needed(&mut buf);
                    
                    self.raw_write_at(offset, &buf); 
                    return true;
                }
                
                block_offset += rec_len;
                if block_offset >= BLOCK_SZ { break; }
            }
            offset += BLOCK_SZ;
        }
        false
    }

    pub fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        let mut offset = 0;
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let file_size_bytes = disk_inode.size() as usize;

        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            self.raw_read_at(offset, &mut buf);

            let mut block_offset = 0;
            let mut prev_offset = 0;
            while block_offset < BLOCK_SZ {
                let dirent = unsafe { &mut *(buf[block_offset..].as_ptr() as *mut Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                
                if rec_len == 0 { break; } 

                if dirent.inode != 0 && dirent.name() == name {
                    let target_inode_id = dirent.inode;
                    if block_offset == 0 {
                        dirent.inode = 0;
                    } else {
                        let prev_dirent = unsafe { &mut *(buf[prev_offset..].as_ptr() as *mut Ext4DirEntry) };
                        prev_dirent.rec_len += rec_len as u16;
                    }

                    self.update_dir_block_checksum_if_needed(&mut buf);
                    self.raw_write_at(offset, &buf);
                    
                    // 递减链接数并检查是否需要回收
                    let links = self.fs.decrease_link_count(target_inode_id);
                    if links == 0 {
                        // 如果链接数为0，回收 Inode (目前暂不递归回收数据块，以防复杂性)
                        self.fs.dealloc_inode(target_inode_id);
                    }
                    
                    return Some(target_inode_id);
                }

                prev_offset = block_offset;
                block_offset += rec_len;
                if block_offset >= BLOCK_SZ { break; }
            }
            offset += BLOCK_SZ;
        }
        None
    }

    /// 截断/扩展文件到指定大小
    /// - len < 当前大小：释放超出部分的物理块，更新 extent 树
    /// - len > 当前大小：只更新 i_size（稀疏文件，空洞读为零）
    pub fn truncate(&self, len: usize) -> bool {
        let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
        let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        let mut cache = block_cache.lock();

        let old_size = cache.read(inode_offset, |disk_inode: &Ext4InodeDisk| {
            disk_inode.size() as usize
        });

        if len > old_size {
            // 扩展：仅更新 i_size，不分配块（稀疏文件）
            cache.modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
                disk_inode.i_size_lo = len as u32;
                disk_inode.i_size_high = (len >> 32) as u32;
            });
            return true;
        }

        if len == old_size {
            return true;
        }

        // 缩小：第一阶段 — 收集要释放的物理块
        let block_size = BLOCK_SZ;
        let new_end_block = if len == 0 { 0 } else { ((len - 1) / block_size) as u32 + 1 };
        let mut blocks_to_free: alloc::vec::Vec<u32> = alloc::vec::Vec::new();

        cache.modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
            let flags = disk_inode.i_flags;
            let old_blocks = disk_inode.i_blocks_lo;

            if flags & EXT4_EXTENTS_FL != 0 {
                let eh_magic = (disk_inode.i_block[0] & 0xFFFF) as u16;
                if eh_magic != 0xF30A {
                    disk_inode.i_size_lo = len as u32;
                    disk_inode.i_size_high = (len >> 32) as u32;
                    return;
                }
                let eh_depth = (disk_inode.i_block[1] >> 16) as u16;
                if eh_depth != 0 {
                    warn!("[ext4 truncate] depth > 0 not supported, only updating size");
                    disk_inode.i_size_lo = len as u32;
                    disk_inode.i_size_high = (len >> 32) as u32;
                    return;
                }

                let max_entries = 4;
                let eh_entries = (disk_inode.i_block[0] >> 16) as u16;
                let mut kept_count: u16 = 0;
                let mut freed_blocks: u32 = 0;

                for i in 0..eh_entries.min(max_entries) {
                    let idx = 3 + i as usize * 3;
                    let ee_block = disk_inode.i_block[idx];
                    let ee_len_raw = (disk_inode.i_block[idx + 1] & 0xFFFF) as u16;
                    let ee_start_hi = ((disk_inode.i_block[idx + 1] >> 16) & 0xFFFF) as u16;
                    let ee_start_lo = disk_inode.i_block[idx + 2];

                    let actual_len = if ee_len_raw > 32768 {
                        ee_len_raw - 32768
                    } else {
                        ee_len_raw
                    } as u32;
                    let ee_end = ee_block + actual_len;

                    if ee_end <= new_end_block {
                        // 整个 extent 保留
                        let dst = 3 + kept_count as usize * 3;
                        disk_inode.i_block[dst] = ee_block;
                        disk_inode.i_block[dst + 1] = disk_inode.i_block[idx + 1]; // 保留原始 ee_len + ee_start_hi
                        disk_inode.i_block[dst + 2] = ee_start_lo;
                        kept_count += 1;
                    } else if ee_block < new_end_block {
                        // extent 跨越新边界，截断
                        let new_len = new_end_block - ee_block;
                        let new_len_raw = if ee_len_raw > 32768 {
                            new_len as u16 + 32768
                        } else {
                            new_len as u16
                        };
                        let dst = 3 + kept_count as usize * 3;
                        disk_inode.i_block[dst] = ee_block;
                        // 保持 ee_start_hi 不变，只更新 ee_len
                        disk_inode.i_block[dst + 1] = (disk_inode.i_block[idx + 1] & 0xFFFF0000) | (new_len_raw as u32);
                        disk_inode.i_block[dst + 2] = ee_start_lo;
                        kept_count += 1;

                        // 收集被截断部分的物理块
                        let trimmed = actual_len - new_len;
                        for j in 0..trimmed {
                            let phys = ((ee_start_hi as u64) << 32)
                                | (ee_start_lo as u64)
                                | ((new_len + j) as u64);
                            blocks_to_free.push(phys as u32);
                        }
                        freed_blocks += trimmed;
                    } else {
                        // 整个 extent 在新范围之后，释放全部
                        for j in 0..actual_len {
                            let phys = ((ee_start_hi as u64) << 32)
                                | (ee_start_lo as u64)
                                | (j as u64);
                            blocks_to_free.push(phys as u32);
                        }
                        freed_blocks += actual_len;
                    }
                }

                // 清零多余的 entry 位置
                for i in kept_count as usize..max_entries as usize {
                    let dst = 3 + i * 3;
                    disk_inode.i_block[dst] = 0;
                    disk_inode.i_block[dst + 1] = 0;
                    disk_inode.i_block[dst + 2] = 0;
                }
                disk_inode.i_block[0] = (disk_inode.i_block[0] & 0xFFFF) | ((kept_count as u32) << 16);

                let blocks_per_sector = (block_size / 512) as u32;
                disk_inode.i_blocks_lo = old_blocks.saturating_sub(freed_blocks * blocks_per_sector);
            } else {
                // 传统直接块模式
                for i in new_end_block as usize..12 {
                    if disk_inode.i_block[i] != 0 {
                        blocks_to_free.push(disk_inode.i_block[i]);
                        disk_inode.i_block[i] = 0;
                        disk_inode.i_blocks_lo = disk_inode
                            .i_blocks_lo
                            .saturating_sub((block_size / 512) as u32);
                    }
                }
            }

            disk_inode.i_size_lo = len as u32;
            disk_inode.i_size_high = (len >> 32) as u32;
        });

        // 第二阶段：释放收集到的物理块（在 cache 锁外进行，避免死锁）
        for phys in blocks_to_free {
            self.fs.dealloc_block(phys);
        }

        true
    }
    
}
