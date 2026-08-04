use alloc::{sync::Arc, vec::Vec};
use alloc::vec;
use alloc::string::String;
use xmas_elf::header;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
use crate::ext4fs::BLOCK_SZ;
use super::{ext4::Ext4FS, ext4_dir_entry::Ext4DirEntry, block_modify_inode, get_block_cache};
use crate::fs::VfsInode;

use core::arch::asm;

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
    // 定义为u8数组便于后续的引用（否则会导致对齐问题）
    pub i_block: [u8; 60], // Pointers to blocks
    pub i_generation: u32,  // File version (for NFS)
    pub i_file_acl_lo: u32, // File ACL
    pub i_size_high: u32,
    pub i_obso_faddr: u32,
    pub l_i_osd2: [u8; 12],
}
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, IntoBytes)]
pub struct Ext4ExtentHeader {
    pub eh_magic: u16,       // 魔数 0xF30A
    pub eh_entries: u16,     // 当前节点中的有效条目数
    pub eh_max: u16,         // 本节点最多能容纳的条目数
    pub eh_depth: u16,       // 树的深度（0 = 叶子节点）
    pub eh_generation: u32,  // 版本号
}
/// Ext4 Extent 叶子节点条目 (12 bytes)
/// 定义了一个连续物理块区间的映射关系
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, IntoBytes)]
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
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, IntoBytes)]
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

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, IntoBytes)]
struct Ext4ExtentEntry {
    pub _bytes: [u8; 12],
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
    /// 文件大小（Atomic 以支持通过 &self 在 write 后更新缓存）
    pub size: AtomicU64,
    /// 写入锁：原子化文件内容的写入操作，避免多个线程同时修改文件数据
    pub write_lock: Mutex<()>,
    /// 块映射锁：原子化逻辑块到物理块映射的查找、分配和删除操作
    pub block_map_lock: Mutex<()>,
    /// 标志位 (例如是否使用 Extents)
    pub flags: u32,
    /// 数据块指针（直接块、间接块等）
    pub i_block: [u8; 60],
    /// inode 代数：记录创建时的磁盘代数，防止 ino 被释放并复用后，
    /// 失效的旧对象误释放新文件
    generation: u32,
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
            size: AtomicU64::new(disk_inode.size()), // 使用 DiskInode 已有的方法计算大小
            write_lock: Mutex::new(()),
            block_map_lock: Mutex::new(()),
            flags: disk_inode.i_flags,
            i_block: disk_inode.i_block,
            generation: disk_inode.i_generation,
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

        if self.is_symlink() && (disk_inode.size() as usize) <= 60 {
            return 0;
        }

        if flags & 0x80000 != 0 {
            // Extents 模式 (EXT4_EXTENTS_FL = 0x80000)
            // i_block 已经是 [u8; 60]，直接用作字节切片
            let mut current_block_data = alloc::boxed::Box::new([0u8; 4096]);
            let mut data_ptr: &[u8] = &i_block;

            loop {
                // Extent Header (12 bytes)
                if data_ptr.len() < 12 { return 0; }
                let header = Ext4ExtentHeader::ref_from_bytes(&data_ptr[0..12]).unwrap();
                if header.eh_magic != 0xF30A { return 0; }

                if header.eh_depth == 0 {
                    // 叶子节点 (Leaf Node)
                    // 在 Inode 中最多只有 4 个 entry
                    let max_entries = if data_ptr.len() == 60 { 4 } else { 340 };
                    let actual_entries = header.eh_entries.min(max_entries) as usize;

                    for i in 0..actual_entries {
                        let off = 12 + i * 12;
                        if off + 12 > data_ptr.len() { break; }
                        let leaf = Ext4ExtentLeaf::ref_from_bytes(&data_ptr[off..off + 12]).unwrap();

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
                        let idx = Ext4ExtentIndex::ref_from_bytes(&data_ptr[off..off + 12]).unwrap();
                        if logical_block_id >= idx.ei_block {
                            found_index = i;
                        } else {
                            break;
                        }
                    }

                    let off = 12 + found_index * 12;
                    let idx = Ext4ExtentIndex::ref_from_bytes(&data_ptr[off..off + 12]).unwrap();
                    let next_block = idx.leaf_phys();

                    // 加载下一层级的数据块并继续搜索
                    self.fs.block_dev.read_block(next_block as usize, current_block_data.as_mut_slice());
                    data_ptr = current_block_data.as_slice();
                }
            }
        } else {
            // 传统的直接块模式
            if (logical_block_id as usize) < 12 {
                let base = (logical_block_id as usize) * 4;
                u32::from_le_bytes(i_block[base..base + 4].try_into().unwrap())
            } else {
                0 // 目前尚不支持一级/二级/三级间接块
            }
        }
    }

    pub fn add_extent_entry(&self, logical_block_id: u32, physical_block_id: u32) -> Option<u32> {
        info!("add_extent_entry: ino={} logical={} physical={}", self.inode_id, logical_block_id, physical_block_id);
        let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
        let inode_table_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        let mut inode_table = inode_table_cache.lock();
        let disk_inode = inode_table.get_mut::<Ext4InodeDisk>(inode_offset);

        let result: Option<u32> = 'body: {
            // 用 raw pointer 读魔数（i_block 是 [u8;60]，对齐=1，无 UB）
            if unsafe { &*(disk_inode.i_block.as_ptr() as *const Ext4ExtentHeader) }.eh_magic != 0xF30A {
                error!("VFS: add_extent_entry - invalid magic 0x{:X}",
                    unsafe { &*(disk_inode.i_block.as_ptr() as *const Ext4ExtentHeader) }.eh_magic as usize);
                break 'body None;
            }

            // 在节点条目中二分查找 logical_block 所属的条目
            // 返回的条目是满足 block <= logical_block 的最后一个条目
            // (一个 extent 条目覆盖 [ee_block, 同级下一个条目的 ee_block) 的逻辑块范围)
            // 返回裸指针，需要调用者自行转换
            let find_aim = |data: &[u8], eh_entries: u16, logical_block: u32|
                -> Option<*mut u8>
            {
                let max_entries: u16 = if data.len() == 60 { 4 } else { 340 };
                let entries = eh_entries.min(max_entries) as usize;
                if entries == 0 {
                    return None;
                }
                let mut lo: isize = 0;
                let mut hi: isize = entries as isize;
                while lo < hi {
                    let mid = ((lo + hi) / 2) as usize;
                    let off = 12 + mid * 12;
                    let block = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
                    if block <= logical_block {
                        lo = mid as isize + 1;
                    } else {
                        hi = mid as isize;
                    }
                }
                let idx = lo - 1;
                if idx < 0 { None }
                else { Some(unsafe { data.as_ptr().add(12 + (idx as usize) * 12) as *mut u8 }) }
            };

            // 将子节点 buffer 写回磁盘
            let write_back_block = |phys_block: u32, ptr: *const u8, len| {
                if phys_block != 0 {
                    let buf = unsafe { core::slice::from_raw_parts(ptr, len) };
                    self.fs.block_dev.write_block(phys_block as usize, buf);
                }
            };

            // 递归处理插入逻辑
            // 返回值: Option<(分裂产生的新块的物理块号, 新块的第一个逻辑块号)>
            // Option表示子节点是否发生了分裂
            // 
            // 注：“节点（block）” 包含多个 “条目（index / leaf）”，
            // “条目” 指向下一级 “节点”，
            // 类似页表 page 和 entry 的关系。
            fn add_extent_in_tree (
                ext4_inode: &Ext4Inode,
                write_back_block: &dyn Fn(u32, *const u8, usize),
                find_aim: &dyn Fn(&[u8], u16, u32) -> Option<*mut u8>,
                logical_block_id: u32, 
                physical_block_id: u32,
                current_block_phys: u32, // 当前节点的物理块号，0=in-inode
                current_data_ptr: *mut u8,
            ) -> Option<(u32, u32)> {

                let header = unsafe { &mut *(current_data_ptr as *mut Ext4ExtentHeader) };
                let is_leaf = header.eh_depth == 0;
                let max_entries = header.eh_max as usize;

                // find_aim 返回满足 block <= logical_block 的最后一个条目指针
                let aim_ptr = find_aim(
                    unsafe { core::slice::from_raw_parts(current_data_ptr, 4096) },
                    header.eh_entries,
                    logical_block_id,
                );

                // new_entry_for_split: 当本节点需要分裂时，保存待插入的 12 字节条目
                let mut new_entry_for_split: Option<[u8; 12]> = None;
                // split_info: 目标条目指针相对 cur_data_ptr 的偏移
                let split_info: Option<usize> = if !is_leaf { // 索引节点，访问下一级

                     // 子节点数据缓冲区
                    let mut child_buf_box = alloc::boxed::Box::new([0u8; 4096]);

                    let aim_ptr = aim_ptr.expect("non-leaf must have at least one index entry");
                    let (child_phys, child_data_ptr) = {
                        let aim_idx = Ext4ExtentIndex::mut_from_bytes(
                            unsafe { core::slice::from_raw_parts_mut(aim_ptr, 12) }
                        ).unwrap();
                        let next_block = aim_idx.leaf_phys();
                        ext4_inode.fs.block_dev.read_block(next_block as usize, child_buf_box.as_mut_slice());
                        let ptr = child_buf_box.as_mut_ptr();
                        (next_block as u32, ptr)
                    };

                    let child_result = add_extent_in_tree(
                        ext4_inode,
                        write_back_block,
                        find_aim,
                        logical_block_id,
                        physical_block_id,
                        child_phys,
                        child_data_ptr,
                    );

                    // 子节点在函数调用中已经写回了磁盘，释放box
                    unsafe { drop(child_buf_box); }

                    if let Some((new_child_blk, new_child_first_logical)) = child_result {
                        // 子节点希望分裂，需要在当前节点插入其返回的新索引条目
                        let new_index = Ext4ExtentIndex {
                            ei_block: new_child_first_logical,
                            ei_leaf_lo: new_child_blk as u32,
                            ei_leaf_hi: 0,
                            ei_unused: 0,
                        };

                        // 计算偏移量
                        let aim_offset = unsafe { aim_ptr.offset_from(current_data_ptr) } as usize;
                        // 插入到当前条目之后
                        // 这里解释一下：B+树插入时，选择的idx应该是 逻辑块号 <= aim 的最后一个idx，
                        // 因此子节点分裂时，子节点的全部条目都应该比父节点的block大。
                        let insert_offset = aim_offset + 12;

                        if (header.eh_entries as usize) < max_entries {
                            // 空间充足，直接插入
                            let entries_after = header.eh_entries as usize - aim_offset / 12;
                            // 平移插入位置之后的条目
                            unsafe {
                                let insert_ptr = current_data_ptr.add(insert_offset);
                                core::ptr::copy(
                                    insert_ptr,
                                    insert_ptr.add(12),
                                    entries_after * 12,
                                );
                                core::ptr::copy_nonoverlapping(
                                    &new_index as *const Ext4ExtentIndex as *const u8,
                                    insert_ptr,
                                    12,
                                );
                            }
                            header.eh_entries += 1;
                            // 写回当前节点
                            write_back_block(current_block_phys, current_data_ptr, 4096);
                            None
                        } else { // 本节点空间不足，保存希望插入条目的信息，交由后续分裂逻辑处理
                            new_entry_for_split = Some(new_index.as_bytes().try_into().unwrap());
                            Some(aim_offset) // 分裂信息
                        }
                    } else {
                        None
                    }
                } else { // 当前层级是叶子节点
                    let aim_offset = match &aim_ptr {
                        Some(p) => unsafe { (*p).offset_from(current_data_ptr) as usize },
                        None => 12, // 没有条目时插入到 header 之后
                    };
                    // 有 aim 条目时插入其后(+12)，否则插入到条目区起始(12)
                    let insert_offset = if aim_ptr.is_some() { aim_offset + 12 } else { 12 };

                    // 尝试和现有 leaf 合并（仅当 aim_ptr 存在且块号匹配时）
                    if let Some(ap) = aim_ptr {
                        let aim_leaf = Ext4ExtentLeaf::mut_from_bytes(
                            unsafe { core::slice::from_raw_parts_mut(ap, 12) }
                        ).unwrap();
                        let is_unwritten = aim_leaf.ee_len > 32768;
                        let max_len = if is_unwritten { 32767 } else { 32768 };
                        if aim_leaf.actual_len() < max_len
                            && aim_leaf.ee_block + aim_leaf.actual_len() == logical_block_id
                            && aim_leaf.phys_end() == (physical_block_id as u64)
                        {
                            let new_len = aim_leaf.actual_len() as u16 + 1;
                            aim_leaf.ee_len = if is_unwritten {
                                new_len + 32768
                            } else {
                                new_len
                            };
                            write_back_block(current_block_phys, current_data_ptr, 4096);
                            return None;
                        }
                    }

                    let new_leaf = Ext4ExtentLeaf {
                        ee_block: logical_block_id,
                        ee_len: 1,
                        ee_start_hi: 0 as u16,
                        ee_start_lo: physical_block_id as u32,
                    };

                    if (header.eh_entries as usize) < max_entries {
                        // 空间充足，直接插入（插入到 aim 条目之后的位置）
                        let insert_ptr = unsafe { current_data_ptr.add(insert_offset) };
                        let entrys_end_offset = 12 + (header.eh_entries as usize) * 12;
                        let bytes_after = entrys_end_offset - insert_offset;
                        unsafe {
                            core::ptr::copy(
                                insert_ptr,
                                insert_ptr.add(12),
                                bytes_after,
                            );
                            core::ptr::copy_nonoverlapping(
                                &new_leaf as *const Ext4ExtentLeaf as *const u8,
                                insert_ptr,
                                12,
                            );
                        }
                        header.eh_entries += 1;
                        write_back_block(current_block_phys, current_data_ptr, 4096);
                        None
                    } else {
                        new_entry_for_split = Some(new_leaf.as_bytes().try_into().unwrap());
                        Some(aim_offset) // 分裂信息
                    }
                };

                // 本级节点空间不足，分裂本级，将右半部分条目写入新块，并将新块的索引条目返回给上级
                // 上级需要重新索引该节点，以及分裂出的新右节点
                if let Some(aim_offset) = split_info {
                    let new_bytes = new_entry_for_split.unwrap();
                    // 这里的 idx 计算包含了header
                    let insert_offset = aim_offset + 12;

                    // 分裂前的条目数
                    let entry_count = header.eh_entries as usize;
                    // 满了才会分裂
                    assert!(entry_count == max_entries, "Spliting when not full!"); 
                    // 当前节点的有效数据切片（仅 header + 有效条目，不含尾部 padding）
                    let current_bytes = unsafe {
                        core::slice::from_raw_parts(current_data_ptr, 12 + entry_count * 12)
                    };

                    // 构建合并后的条目列表：[0..insert_idx] + [new] + [insert_idx..]
                    let total = entry_count + 1;
                    let mid = total / 2; // 左半保留条目数
                    let mut combined: Vec<u8> = Vec::with_capacity(total * 12);
                    combined.extend_from_slice(&current_bytes[..insert_offset]);
                    combined.extend_from_slice(&new_bytes);
                    combined.extend_from_slice(&current_bytes[insert_offset..]);

                    // 左半写入当前节点（仅当新条目落在左半时才需要搬移条目数据）
                    // aim_offset 是 aim 条目在块内的字节偏移，新条目插入后位于 aim_offset 处（条目区）
                    if aim_offset < mid * 12 {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                combined.as_ptr().add(12), // 跳过 combined 中的 header
                                current_data_ptr.add(12),  // 跳过当前块的 header
                                mid * 12,
                            );
                        }
                    }
                    header.eh_entries = mid as u16;
                    write_back_block(current_block_phys, current_data_ptr, 4096);

                    // 右半写入新块
                    let new_block_id = ext4_inode.fs.alloc_block().unwrap();
                    let mut new_buf = [0u8; 4096];
                    let right_len = total - mid;
                    // 新增的块一定是一个完整的磁盘块，最多可容纳 340 个条目
                    // 不应复制当前层的最大条目数（否则从根开始向上传递一直是4）
                    let new_header = Ext4ExtentHeader {
                        eh_magic: 0xF30A,
                        eh_entries: right_len as u16,
                        eh_max: ((BLOCK_SZ - 12) / 12) as u16, // 340
                        eh_depth: header.eh_depth,
                        eh_generation: 0,
                    };
                    new_buf[..12].copy_from_slice(new_header.as_bytes());
                    new_buf[12..12 + right_len * 12]
                        .copy_from_slice(&combined[12 + mid * 12..]);
                    write_back_block(new_block_id, new_buf.as_ptr(), 4096);

                    // 返回分裂信息
                    let right_first_block = u32::from_le_bytes(
                        combined[12 + mid * 12..12 + mid * 12 + 4].try_into().unwrap()
                    );
                    Some((new_block_id, right_first_block))
                } else {
                    None
                }
            };
            // 初始调用：从 inode 的 i_block 开始遍历 extent 树
            let current_data_ptr = disk_inode.i_block.as_mut_ptr();
            let result = add_extent_in_tree(
                self,
                &write_back_block,
                &find_aim,
                logical_block_id,
                physical_block_id,
                0, // current_block_phys=0 表示 in-inode
                current_data_ptr,
            );

            // 处理根节点分裂：in-inode 需要从叶子/索引节点升级为索引节点
            if let Some((right_blk, right_first_logical)) = result {
                let old_header = unsafe { &*(current_data_ptr as *const Ext4ExtentHeader) };
                let old_depth = old_header.eh_depth;

                // 将左半（当前在 in-inode 中）移入新块
                let left_blk = self.fs.alloc_block().unwrap();
                let mut left_buf = [0u8; 4096];
                // 新块一定是一个完整的磁盘块，最多可容纳 340 个条目
                let left_header = Ext4ExtentHeader {
                    eh_magic: 0xF30A,
                    eh_entries: old_header.eh_entries,
                    eh_max: ((BLOCK_SZ - 12) / 12) as u16, // 340
                    eh_depth: old_depth,
                    eh_generation: 0,
                };
                left_buf[..12].copy_from_slice(left_header.as_bytes());
                // 复制条目数据
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        current_data_ptr.add(12),
                        left_buf.as_mut_ptr().add(12),
                        old_header.eh_entries as usize * 12,
                    );
                }
                write_back_block(left_blk, left_buf.as_ptr(), 4096);

                // 在 in-inode 中构建新的索引根节点
                let new_root_header = Ext4ExtentHeader {
                    eh_magic: 0xF30A,
                    eh_entries: 2,
                    eh_max: 4, // in-inode 最多 4 个索引条目
                    eh_depth: old_depth + 1,
                    eh_generation: 0,
                };
                unsafe { (current_data_ptr as *mut Ext4ExtentHeader).write_unaligned(new_root_header); }

                // 索引条目 0 → 左子块（取左半的首个逻辑块号）
                let left_first_logical = u32::from_le_bytes(
                    left_buf[12..16].try_into().unwrap()
                );
                let idx0 = Ext4ExtentIndex {
                    ei_block: left_first_logical,
                    ei_leaf_lo: left_blk as u32,
                    ei_leaf_hi: 0,
                    ei_unused: 0,
                };
                let idx1 = Ext4ExtentIndex {
                    ei_block: right_first_logical,
                    ei_leaf_lo: right_blk as u32,
                    ei_leaf_hi: 0,
                    ei_unused: 0,
                };
                unsafe {
                    let idx_base = current_data_ptr.add(12) as *mut Ext4ExtentIndex;
                    idx_base.write_unaligned(idx0);
                    idx_base.add(1).write_unaligned(idx1);
                }
            }

            disk_inode.i_blocks_lo = disk_inode
                .i_blocks_lo
                .saturating_add((BLOCK_SZ / 512) as u32);
            Some(physical_block_id)
        };

        result
    }

    pub fn read_dirents(&self) {
        if !self.is_dir() {
            return;
        }
        let mut offset = 0;
        let file_size = self.size.load(Ordering::Relaxed) as usize;

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

        if self.is_symlink() && disk_size_bytes <= 60 {
            // i_block 已经是 [u8; 60]，直接复制
            let i_block_bytes = disk_inode.i_block;
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
                self.fs.block_dev.read_data_block(physical_block_id as usize, &mut temp_buf);
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
        info!("raw_write_at: ino={} offset={} len={} old_size={}", self.inode_id, offset, buf.len(), old_size_bytes);
        if self.is_symlink() && old_size_bytes <= 60 && end <= 60 {
            block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
                let mut i_block_bytes = disk_inode.i_block;
                i_block_bytes[offset..end].copy_from_slice(buf);
                disk_inode.i_block = i_block_bytes;

                if end > old_size_bytes {
                    disk_inode.i_size_lo = end as u32;
                    disk_inode.i_size_high = 0;
                }
            });
            if !buf.is_empty() && end > old_size_bytes {
                self.size.fetch_max(end as u64, Ordering::Release);
            }
            return buf.len();
        }
        while curr_offset < end {
            let inner_block_id = (curr_offset / block_size) as u32;
            let block_pos = curr_offset % block_size;

            let mut physical_block_id = self.find_physical_block(inner_block_id);
            if physical_block_id == 0 {
                // 如果块不存在，尝试分配
                if let Some(new_block_id) = self.fs.alloc_block() {
                    info!("raw_write_at: alloc block {} for logical block {}", new_block_id, inner_block_id);
                    let disk_inode = self.fs.get_disk_inode(self.inode_id);
                    if disk_inode.i_flags & EXT4_EXTENTS_FL == 0 {
                        // 传统的直接块模式
                        if inner_block_id < 12 {
                            block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
                                let base = (inner_block_id as usize) * 4;
                                disk_inode.i_block[base..base + 4].copy_from_slice(&new_block_id.to_le_bytes());
                                disk_inode.i_blocks_lo += (block_size / 512) as u32;
                            });
                            physical_block_id = new_block_id;
                        } else {
                            trace!("VFS: write_at - indirect blocks not supported");
                            break;
                        }
                    } else {
                        // Extent 模式下的块分配
                        info!("raw_write_at: calling add_extent_entry({}, {})", inner_block_id, new_block_id);
                        if let Some(phys) = self.add_extent_entry(inner_block_id, new_block_id) {
                            physical_block_id = phys;
                            info!("raw_write_at: add_extent_entry OK, phys={}", phys);
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

            info!("raw_write_at: write block {} offset={} len={}", physical_block_id, block_pos, core::cmp::min(block_size - block_pos, end - curr_offset));

            let mut temp_buf = alloc::vec![0u8; 4096];
            self.fs.block_dev.read_data_block(physical_block_id as usize, &mut temp_buf);

            let write_len = core::cmp::min(block_size - block_pos, end - curr_offset);
            temp_buf[block_pos..block_pos + write_len].copy_from_slice(&buf[actual_write..actual_write + write_len]);

            self.fs.block_dev.write_data_block(physical_block_id as usize, &temp_buf);

            actual_write += write_len;
            curr_offset += write_len;
        }

        // 更新磁盘 Inode 的 size (以字节为单位)
        let new_size_bytes = offset + actual_write;
        info!("raw_write_at: done actual_write={} new_size={}", actual_write, new_size_bytes);
        
        if actual_write > 0 && new_size_bytes > old_size_bytes {
            block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
                disk_inode.i_size_lo = new_size_bytes as u32;
                disk_inode.i_size_high = (new_size_bytes >> 32) as u32;
            });
            self.size
                .fetch_max(new_size_bytes as u64, Ordering::Release);
        }

        actual_write
    }

    pub fn add_dir_entry(&self, name: &str, inode_id: u32, file_type: u8) -> bool {
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let file_size_bytes = disk_inode.size() as usize;
        let needed_len = ((8 + name.len() + 3) & !3) as usize;

        // 检查是否有同名目录项
        let mut offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            self.read_at(offset, &mut buf);
            let mut block_offset = 0;
            while block_offset < BLOCK_SZ {
                let dirent = unsafe { &*(buf[block_offset..].as_ptr() as *const Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                if rec_len == 0 { break; }
                if dirent.inode != 0 && dirent.name() == name {
                    return false;
                }
                block_offset += rec_len;
                if block_offset >= BLOCK_SZ { break; }
            }
            offset += BLOCK_SZ;
        }

        // 尝试插入
        offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            self.read_at(offset, &mut buf);
            
            let mut block_offset = 0;
            while block_offset < BLOCK_SZ {
                let dirent = unsafe { &mut *(buf[block_offset..].as_ptr() as *mut Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                
                if rec_len == 0 { break; }
                
                let real_len = dirent.real_len() as usize;
                
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
                    
                    self.write_at(offset, &buf);
                    return true;
                }
                
                block_offset += rec_len;
                if block_offset >= BLOCK_SZ { break; }
            }
            offset += BLOCK_SZ;
        }

        // 所有现有块都已满，分配新块，扩展目录
        // 写入一个 rec_len = BLOCK_SZ 的目录项作为新块的哨兵条目
        let mut new_buf = alloc::vec![0u8; BLOCK_SZ];
        let new_dirent = Ext4DirEntry::new_disk(inode_id, BLOCK_SZ as u16, name, file_type);
        let new_dirent_bytes = unsafe {
            core::slice::from_raw_parts(&new_dirent as *const _ as *const u8, 8 + new_dirent.name_len as usize)
        };
        new_buf[..new_dirent_bytes.len()].copy_from_slice(new_dirent_bytes);
        self.update_dir_block_checksum_if_needed(&mut new_buf);
        // raw_write_at 会自动分配新块、插入 extent、更新 inode size
        let written = self.write_at(file_size_bytes, &new_buf);
        written == BLOCK_SZ
    }

    pub fn lookup_dir_entry(&self, name: &str) -> Option<(u32, u8)> {
        let file_size_bytes = self.fs.get_disk_inode(self.inode_id).size() as usize;
        let mut offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            let read_len = self.read_at(offset, &mut buf);
            let mut block_offset = 0;
            while block_offset + 8 <= read_len {
                let dirent = Ext4DirEntry::from_bytes(&buf[block_offset..read_len])?;
                let rec_len = dirent.rec_len() as usize;
                if rec_len == 0 || block_offset + rec_len > read_len {
                    break;
                }
                if dirent.inode() != 0 && dirent.safe_name() == name {
                    return Some((dirent.inode(), dirent.file_type));
                }
                block_offset += rec_len;
            }
            offset += BLOCK_SZ;
        }
        None
    }

    /// 移除目录项，但不修改 inode 链接计数
    pub fn remove_dir_entry_only(&self, name: &str) -> Option<(u32, u8)> {
        let file_size_bytes = self.fs.get_disk_inode(self.inode_id).size() as usize;
        let mut offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            let read_len = self.read_at(offset, &mut buf);
            let mut block_offset = 0;
            let mut prev_offset = None;
            while block_offset + 8 <= read_len {
                let dirent = unsafe { &mut *(buf[block_offset..].as_mut_ptr() as *mut Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                if rec_len < 8 || block_offset + rec_len > read_len {
                    break;
                }
                if dirent.inode != 0 && dirent.safe_name() == name {
                    let removed = (dirent.inode, dirent.file_type);
                    if let Some(previous) = prev_offset {
                        let prev = unsafe { &mut *(buf[previous..].as_mut_ptr() as *mut Ext4DirEntry) };
                        prev.rec_len = prev.rec_len.saturating_add(dirent.rec_len);
                    } else {
                        dirent.inode = 0;
                    }
                    self.update_dir_block_checksum_if_needed(&mut buf);
                    if self.write_at(offset, &buf) == BLOCK_SZ {
                        return Some(removed);
                    }
                    return None;
                }
                prev_offset = Some(block_offset);
                block_offset += rec_len;
            }
            offset += BLOCK_SZ;
        }
        None
    }

    /// Change the inode referenced by an existing name, returning the old target.
    pub fn replace_dir_entry(
        &self,
        name: &str,
        inode_id: u32,
        file_type: u8,
    ) -> Option<(u32, u8)> {
        let file_size_bytes = self.fs.get_disk_inode(self.inode_id).size() as usize;
        let mut offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            let read_len = self.read_at(offset, &mut buf);
            let mut block_offset = 0;
            while block_offset + 8 <= read_len {
                let dirent = unsafe { &mut *(buf[block_offset..].as_mut_ptr() as *mut Ext4DirEntry) };
                let rec_len = dirent.rec_len as usize;
                if rec_len < 8 || block_offset + rec_len > read_len {
                    break;
                }
                if dirent.inode != 0 && dirent.safe_name() == name {
                    let replaced = (dirent.inode, dirent.file_type);
                    dirent.inode = inode_id;
                    dirent.file_type = file_type;
                    self.update_dir_block_checksum_if_needed(&mut buf);
                    if self.write_at(offset, &buf) == BLOCK_SZ {
                        return Some(replaced);
                    }
                    return None;
                }
                block_offset += rec_len;
            }
            offset += BLOCK_SZ;
        }
        None
    }

    pub fn directory_is_empty(&self) -> bool {
        let file_size_bytes = self.fs.get_disk_inode(self.inode_id).size() as usize;
        let mut offset = 0;
        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; BLOCK_SZ];
            let read_len = self.read_at(offset, &mut buf);
            let mut block_offset = 0;
            while block_offset + 8 <= read_len {
                let Some(dirent) = Ext4DirEntry::from_bytes(&buf[block_offset..read_len]) else {
                    return false;
                };
                let rec_len = dirent.rec_len() as usize;
                if rec_len == 0 || block_offset + rec_len > read_len {
                    return false;
                }
                if dirent.inode() != 0 {
                    let entry_name = dirent.safe_name();
                    if entry_name != "." && entry_name != ".." {
                        return false;
                    }
                }
                block_offset += rec_len;
            }
            offset += BLOCK_SZ;
        }
        true
    }

    pub fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        let (target_inode_id, _) = self.remove_dir_entry_only(name)?;
        let links = self.fs.decrease_link_count(target_inode_id);
        if links == 0 {
            // 磁盘链接数归零：真正的回收（数据块 + inode 位图）由 Ext4Inode::drop
            // 在最后一个引用释放时执行。这里强制实例化一次并立即丢弃，确保即使
            // 该 ino 此前从未被打开过（内存中没有对象），也会触发 Drop 完成回收。
            let _orphan = self.fs.get_inode(target_inode_id);
            info!("Inode {} link count is zero; data blocks and inode slot will be released when the last reference is dropped", target_inode_id);
        }
        Some(target_inode_id)
    }

    /// 截断/扩展文件到指定大小
    /// - len < 当前大小：释放超出部分的物理块，更新 extent 树
    /// - len > 当前大小：只更新 i_size（稀疏文件，空洞读为零）
    pub fn truncate(&self, len: usize) -> bool {
        let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
        let mut buf = [0u8; BLOCK_SZ];
        self.fs.block_dev.read_block(block_id as usize, &mut buf);
        let disk_inode: &mut Ext4InodeDisk = unsafe { &mut *(buf.as_mut_ptr().add(inode_offset) as *mut Ext4InodeDisk) };

        let old_size = disk_inode.size() as usize;

        if len > old_size {
            disk_inode.i_size_lo = len as u32;
            disk_inode.i_size_high = (len >> 32) as u32;
            self.fs.block_dev.write_block(block_id as usize, &buf);
            self.size.store(len as u64, Ordering::Release);
            return true;
        }

        if len == old_size {
            self.size.store(len as u64, Ordering::Release);
            return true;
        }

        // 缩小：第一阶段 — 收集要释放的物理块
        let block_size = BLOCK_SZ;
        let new_end_block = if len == 0 { 0 } else { ((len - 1) / block_size) as u32 + 1 };
        let mut blocks_to_free: alloc::vec::Vec<u32> = alloc::vec::Vec::new();

        let flags = disk_inode.i_flags;

        if flags & EXT4_EXTENTS_FL != 0 {
            let header_ptr = disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentHeader;
            let mut header: Ext4ExtentHeader = unsafe { header_ptr.read_unaligned() };

            if header.eh_magic != 0xF30A {
                disk_inode.i_size_lo = len as u32;
                disk_inode.i_size_high = (len >> 32) as u32;
                self.fs.block_dev.write_block(block_id as usize, &buf);
                self.size.store(len as u64, Ordering::Release);
                return true;
            }
            if header.eh_depth != 0 {
                warn!("[ext4 truncate] depth > 0 not supported, only updating size");
                disk_inode.i_size_lo = len as u32;
                disk_inode.i_size_high = (len >> 32) as u32;
            } else {
                let old_blocks = disk_inode.i_blocks_lo;
                let max_entries: u16 = 4;
                let actual_entries = header.eh_entries.min(max_entries) as usize;
                let leaf_base = disk_inode.i_block.as_ptr() as *const Ext4ExtentLeaf;
                let leaf_base_mut = disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentLeaf;

                // 跳过 header 的 12 字节，entries 从 index 1 开始
                let entry_ptr = unsafe { leaf_base.add(1) };
                let entry_ptr_mut = unsafe { leaf_base_mut.add(1) };

                let mut kept_count: u16 = 0;
                let mut freed_blocks: u32 = 0;

                for i in 0..actual_entries {
                    let leaf: Ext4ExtentLeaf = unsafe { entry_ptr.add(i).read_unaligned() };

                    let ee_end = leaf.ee_block + leaf.actual_len();

                    if ee_end <= new_end_block {
                        // 整个 extent 保留
                        unsafe {
                            entry_ptr_mut.add(kept_count as usize).write_unaligned(leaf);
                        }
                        kept_count += 1;
                    } else if leaf.ee_block < new_end_block {
                        // extent 跨越新边界，截断
                        let new_len = new_end_block - leaf.ee_block;
                        let new_len_raw = if leaf.ee_len > 32768 {
                            new_len as u16 + 32768
                        } else {
                            new_len as u16
                        };
                        let trimmed_leaf = Ext4ExtentLeaf {
                            ee_len: new_len_raw,
                            ..leaf
                        };
                        unsafe {
                            entry_ptr_mut.add(kept_count as usize).write_unaligned(trimmed_leaf);
                        }
                        kept_count += 1;

                        // 收集被截断部分的物理块
                        let actual_len = leaf.actual_len();
                        let trimmed = actual_len - new_len;
                        for j in 0..trimmed {
                            let phys = leaf.start_phys() + (new_len + j) as u64;
                            blocks_to_free.push(phys as u32);
                        }
                        freed_blocks += trimmed;
                    } else {
                        // 整个 extent 在新范围之后，释放全部
                        let actual_len = leaf.actual_len();
                        for j in 0..actual_len {
                            let phys = leaf.start_phys() + j as u64;
                            blocks_to_free.push(phys as u32);
                        }
                        freed_blocks += actual_len;
                    }
                }

                // 清零多余的 entry 位置
                let zero_leaf = Ext4ExtentLeaf {
                    ee_block: 0,
                    ee_len: 0,
                    ee_start_hi: 0,
                    ee_start_lo: 0,
                };
                for i in kept_count as usize..max_entries as usize {
                    unsafe {
                        entry_ptr_mut.add(i).write_unaligned(zero_leaf);
                    }
                }

                // 更新 header 的 eh_entries
                header.eh_entries = kept_count;
                unsafe { header_ptr.write_unaligned(header); }

                let blocks_per_sector = (block_size / 512) as u32;
                disk_inode.i_blocks_lo = old_blocks.saturating_sub(freed_blocks * blocks_per_sector);
            }
        } else {
            // 传统直接块模式
            for i in new_end_block as usize..12 {
                let base = i * 4;
                let val = u32::from_le_bytes(disk_inode.i_block[base..base + 4].try_into().unwrap());
                if val != 0 {
                    blocks_to_free.push(val);
                    disk_inode.i_block[base..base + 4].fill(0);
                    disk_inode.i_blocks_lo = disk_inode
                        .i_blocks_lo
                        .saturating_sub((block_size / 512) as u32);
                }
            }
        }

        disk_inode.i_size_lo = len as u32;
        disk_inode.i_size_high = (len >> 32) as u32;

        self.fs.block_dev.write_block(block_id as usize, &buf);
        self.size.store(len as u64, Ordering::Release);

        // 第二阶段：释放收集到的物理块
        for phys in blocks_to_free {
            self.fs.dealloc_block(phys);
        }

        true
    }
    
}

impl Drop for Ext4Inode {
    fn drop(&mut self) {
        // 在 inode 缓存（ino -> Weak）下，本对象是全局唯一的，因此 drop 一定发生在
        // 最后一个引用释放时。此时只有当磁盘链接数已经为 0（unlink/rmdir/rename-over）
        // 且该磁盘 inode 仍属于本对象创建时的代数时才真正回收：
        // - 链接数不为 0：文件仍被目录引用，不能释放；
        // - 代数不匹配：同一 ino 已经被释放并重新分配（或已被另一个对象回收），
        //   失效的旧对象不能释放新文件，也不能重复归还位图。
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        if disk_inode.i_links_count != 0 || disk_inode.i_generation != self.generation {
            return;
        }

        // Fast symlinks store their target in i_block and own no data blocks.
        // Running truncate on one would interpret target text as block IDs.
        let inline_symlink = self.is_symlink()
            && disk_inode.size() <= 60
            && disk_inode.i_blocks_lo == 0;
        if !inline_symlink {
            // 释放全部数据块并清空 extent 树（i_size / i_blocks 一并归零）
            self.truncate(0);
        }
        info!("Ext4Inode::drop: ino={} links=0, releasing data blocks and inode slot", self.inode_id);

        // 标记为已删除并推进代数，保证后续即使有同 ino 的失效对象 drop 也会跳过
        block_modify_inode(&self.fs, self.inode_id, |d| {
            d.i_dtime = 1; // 简化标记：非零即可，仅用于观察/调试
            d.i_generation = d.i_generation.wrapping_add(1);
            d.i_mode = 0;
        });

        // 归还 inode 位图
        self.fs.dealloc_inode(self.inode_id);
    }
}
