use alloc::{sync::Arc, vec::Vec};
use alloc::vec;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
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
    /// 标志位 (例如是否使用 Extents)
    pub flags: u32,
    /// 数据块指针（直接块、间接块等）
    pub i_block: [u8; 60],
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
        let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        let mut cache = block_cache.lock();
        
        cache.modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
            // 用 raw pointer 读魔数（i_block 是 [u8;60]，对齐=1，无 UB）
            if unsafe { &*(disk_inode.i_block.as_ptr() as *const Ext4ExtentHeader) }.eh_magic != 0xF30A {
                error!("VFS: add_extent_entry - invalid magic 0x{:X}",
                    unsafe { &*(disk_inode.i_block.as_ptr() as *const Ext4ExtentHeader) }.eh_magic as usize);
                return None;
            }

            struct FunctionContext {
                /// 本节点数据块起始指针（即 Ext4ExtentHeader 所在位置）
                data_ptr: *mut u8,
                /// 数据块长度：60 = i_block 根节点，4096 = 独立块子节点
                data_len: usize,
                /// 父节点中引用到本节点的那条 Ext4ExtentIndex 指针
                /// 用于节点分裂后回写父节点（根节点此字段为 None）
                parent_entry: Option<*mut Ext4ExtentIndex>,
                /// 持有子节点数据块缓冲区（根节点为 None）
                _block_buf: Option<alloc::vec::Vec<u8>>,
                /// 本节点的物理块号（根节点为 0，子节点从 alloc_block 获取）
                phys_block: u32,
            }

            let mut ctx_stack: Vec<FunctionContext> = Vec::new();
            ctx_stack.push(FunctionContext {
                data_ptr: disk_inode.i_block.as_mut_ptr(),
                data_len: 60,
                parent_entry: None,
                _block_buf: None,
                phys_block: 0, // 根节点在 i_block 中，由 block cache 管理刷盘
            });

            // 在节点条目中二分查找 logical_block 所属的条目
            // 返回指向 12 字节条目的裸指针
            // depth>0 → Ext4ExtentIndex，depth==0 → Ext4ExtentLeaf
            let find_aim = |data: &[u8], eh_entries: u16, logical_block: u32|
                -> Option<*const u8>
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
                else { Some(unsafe { data.as_ptr().add(12 + (idx as usize) * 12) }) }
            };

            // 将子节点 buffer 写回磁盘（根节点由 block cache 管理，无需手动刷）
            let flush_ctx = |ctx: &FunctionContext| {
                if ctx.phys_block != 0 {
                    let buf = unsafe { core::slice::from_raw_parts(ctx.data_ptr, ctx.data_len) };
                    self.fs.block_dev.write_block(ctx.phys_block as usize, buf);
                }
            };

            // 待向上传播的索引条目：(12 字节条目, 父节点中引用本节点的索引条目指针)
            let mut pending: Option<([u8; 12], Option<*mut Ext4ExtentIndex>)> = None;

            // ===== 阶段一：下钻到叶子 =====
            loop {
                let cur = ctx_stack.last().unwrap();
                let cur_data = unsafe { core::slice::from_raw_parts(cur.data_ptr, cur.data_len) };
                let header = unsafe { &*(cur.data_ptr as *const Ext4ExtentHeader) };
                let depth = header.eh_depth;
                let eh_entries = header.eh_entries;
                let eh_max = header.eh_max;

                let aim_ptr = find_aim(cur_data, eh_entries, logical_block_id);

                if depth == 0 {
                    // ===== 叶子节点 =====
                    if let Some(entry_ptr) = aim_ptr {
                        let leaf = unsafe { &*(entry_ptr as *const Ext4ExtentLeaf) };
                        // 尝试直接扩展：物理块连续 且 ee_len 未溢出
                        let expected_phys = leaf.start_phys() + leaf.actual_len() as u64;
                        if expected_phys == physical_block_id as u64
                            && leaf.actual_len() < 32768
                        {
                            let leaf_mut = unsafe { &mut *(entry_ptr as *mut Ext4ExtentLeaf) };
                            leaf_mut.ee_len += 1;
                            disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32;
                            flush_ctx(cur);
                            return Some(physical_block_id);
                        }
                    }

                    // 计算插入位置（供「有空位」和「分裂」共用）
                    let insert_idx = match aim_ptr {
                        Some(p) => {
                            let leaf = unsafe { &*(p as *const Ext4ExtentLeaf) };
                            if leaf.ee_block == logical_block_id {
                                error!("add_extent_entry: block {} already mapped", logical_block_id);
                                return None;
                            }
                            ((p as usize - cur.data_ptr as usize - 12) / 12 + 1) as usize
                        }
                        None => 0,
                    };

                    if eh_entries < eh_max {
                        // 有空位：直接插入
                        let entry_off = 12 + insert_idx * 12;
                        let move_len = (eh_entries as usize - insert_idx) * 12;
                        if move_len > 0 {
                            unsafe {
                                core::ptr::copy(
                                    cur.data_ptr.add(entry_off),
                                    cur.data_ptr.add(entry_off + 12),
                                    move_len,
                                );
                            }
                        }
                        let new_leaf = Ext4ExtentLeaf {
                            ee_block: logical_block_id,
                            ee_len: 1,
                            ee_start_hi: 0,
                            ee_start_lo: physical_block_id as u32,
                        };
                        unsafe {
                            core::ptr::write_unaligned(
                                cur.data_ptr.add(entry_off) as *mut Ext4ExtentLeaf,
                                new_leaf,
                            );
                        }
                        unsafe { &mut *(cur.data_ptr as *mut Ext4ExtentHeader) }.eh_entries += 1;
                        disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32;
                        flush_ctx(cur);
                        let n = unsafe { (cur.data_ptr as *const Ext4ExtentHeader).read_unaligned().eh_entries };
                        info!("add_extent_entry: simple insert OK, entries={}", n);
                        return Some(physical_block_id);
                    }

                    // ===== 叶子已满：分裂 =====
                    let max_entries: usize = if cur.data_len == 60 { 4 } else { 340 };

                    // 1) 收集所有条目（现有 + 新）排序
                    let mut all: alloc::vec::Vec<[u8; 12]> =
                        alloc::vec::Vec::with_capacity(max_entries + 1);
                    for i in 0..eh_entries as usize {
                        let mut buf = [0u8; 12];
                        unsafe {
                            buf.copy_from_slice(core::slice::from_raw_parts(
                                cur.data_ptr.add(12 + i * 12), 12));
                        }
                        all.push(buf);
                    }
                    {
                        let leaf = Ext4ExtentLeaf {
                            ee_block: logical_block_id,
                            ee_len: 1,
                            ee_start_hi: 0,
                            ee_start_lo: physical_block_id as u32,
                        };
                        all.insert(insert_idx, leaf.to_bytes());
                    }
                    let total = all.len();
                    let half = total / 2; // 留在旧节点的条目数

                    // 2) 分配新兄弟块
                    let new_phys = match self.fs.alloc_block() {
                        Some(p) => p,
                        None => { error!("add_extent_entry: no free block for split"); return None; }
                    };
                    let mut new_buf = alloc::vec![0u8; BLOCK_SZ];
                    unsafe {
                        core::ptr::write_unaligned(
                            new_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            Ext4ExtentHeader {
                                eh_magic: 0xF30A,
                                eh_entries: (total - half) as u16,
                                eh_max: 340,
                                eh_depth: 0,
                                eh_generation: 0,
                            },
                        );
                    }

                    // 3) 前半写入旧节点，后半写入新节点
                    for i in 0..half {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                all[i].as_ptr(), cur.data_ptr.add(12 + i * 12), 12);
                        }
                    }
                    for i in half..max_entries {
                        unsafe { core::ptr::write_bytes(cur.data_ptr.add(12 + i * 12), 0, 12); }
                    }
                    unsafe { &mut *(cur.data_ptr as *mut Ext4ExtentHeader) }.eh_entries = half as u16;

                    for i in half..total {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                all[i].as_ptr(),
                                new_buf.as_mut_ptr().add(12 + (i - half) * 12), 12);
                        }
                    }
                    self.fs.block_dev.write_block(new_phys as usize, &new_buf);

                    // 数据块 physical_block_id + 新兄弟元数据块 new_phys
                    disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32 * 2;

                    // 4) 若旧节点首个条目发生变化，更新父节点中的索引条目
                    let new_first_block = u32::from_le_bytes(all[0][0..4].try_into().unwrap());
                    if let Some(pe_ptr) = cur.parent_entry {
                        let old_first =
                            u32::from_le_bytes(unsafe { core::slice::from_raw_parts(pe_ptr as *const u8, 4) }
                                .try_into().unwrap());
                        if new_first_block != old_first {
                            unsafe { (pe_ptr as *mut u32).write_unaligned(new_first_block); }
                        }
                    }

                    // 5) 构造待插入父节点的索引条目（指向新兄弟）
                    let sibling_first_block = u32::from_le_bytes(all[half][0..4].try_into().unwrap());
                    let mut idx_bytes = [0u8; 12];
                    idx_bytes[0..4].copy_from_slice(&sibling_first_block.to_le_bytes());
                    idx_bytes[4..8].copy_from_slice(&(new_phys as u32).to_le_bytes());
                    idx_bytes[8..10].copy_from_slice(&0u16.to_le_bytes());

                    pending = Some((idx_bytes, cur.parent_entry));
                    // 旧叶子已修改，若为子节点则刷盘
                    flush_ctx(cur);
                    break; // 退出下钻，进入向上传播阶段
                } else {
                    // ===== 索引节点：加载子节点压栈继续下钻 =====
                    if let Some(entry_ptr) = aim_ptr {
                        let idx = unsafe { &*(entry_ptr as *const Ext4ExtentIndex) };
                        let child_phys = idx.leaf_phys();
                        let mut child_buf = alloc::vec![0u8; BLOCK_SZ];
                        self.fs.block_dev.read_block(child_phys as usize, &mut child_buf);

                        ctx_stack.push(FunctionContext {
                            data_ptr: child_buf.as_mut_ptr(),
                            data_len: BLOCK_SZ,
                            parent_entry: Some(entry_ptr as *mut Ext4ExtentIndex),
                            _block_buf: Some(child_buf),
                            phys_block: child_phys as u32,
                        });
                    } else {
                        error!("add_extent_entry: no index covers block {} at depth {}",
                            logical_block_id, depth);
                        return None;
                    }
                }
            } // end descent loop

            // ===== 阶段二：向上传播分裂 =====
            loop {
                let (idx_bytes, _parent_entry_ptr) = match pending.take() {
                    Some(v) => v,
                    None => {
                        info!("add_extent_entry: propagation done, success");
                        return Some(physical_block_id);
                    }
                };

                // 弹出当前节点（已处理完分裂），暴露父节点
                ctx_stack.pop();

                match ctx_stack.last() {
                    Some(parent_ctx) => {
                        // --- 有父节点：在父节点中插入索引条目 ---
                        let p_data = unsafe {
                            core::slice::from_raw_parts(parent_ctx.data_ptr, parent_ctx.data_len)
                        };
                        let p_header = unsafe {
                            &*(parent_ctx.data_ptr as *const Ext4ExtentHeader)
                        };
                        let p_entries = p_header.eh_entries;
                        let p_max = p_header.eh_max;

                        // 二分查找父节点中的插入位置
                        let new_ei_block = u32::from_le_bytes(idx_bytes[0..4].try_into().unwrap());
                        let p_aim = find_aim(p_data, p_entries, new_ei_block);
                        let p_insert_idx = match p_aim {
                            Some(p) => {
                                ((p as usize - parent_ctx.data_ptr as usize - 12) / 12 + 1) as usize
                            }
                            None => 0,
                        };

                        let p_max_u = if parent_ctx.data_len == 60 {
                            4usize
                        } else {
                            340usize
                        };

                        if (p_entries as usize) < p_max_u {
                            // 父节点有空位，插入后完成
                            let entry_off = 12 + p_insert_idx * 12;
                            let move_len = (p_entries as usize - p_insert_idx) * 12;
                            if move_len > 0 {
                                unsafe {
                                    core::ptr::copy(
                                        parent_ctx.data_ptr.add(entry_off),
                                        parent_ctx.data_ptr.add(entry_off + 12),
                                        move_len,
                                    );
                                }
                            }
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    idx_bytes.as_ptr(),
                                    parent_ctx.data_ptr.add(entry_off),
                                    12,
                                );
                            }
                            unsafe {
                                &mut *(parent_ctx.data_ptr as *mut Ext4ExtentHeader)
                            }.eh_entries += 1;
                            flush_ctx(parent_ctx);
                            return Some(physical_block_id);
                        }

                        // --- 父节点也满了：分裂父节点 ---
                        let mut all_idx: alloc::vec::Vec<[u8; 12]> =
                            alloc::vec::Vec::with_capacity(p_max_u + 1);
                        for i in 0..p_entries as usize {
                            let mut buf = [0u8; 12];
                            unsafe {
                                buf.copy_from_slice(core::slice::from_raw_parts(
                                    parent_ctx.data_ptr.add(12 + i * 12), 12));
                            }
                            all_idx.push(buf);
                        }
                        all_idx.insert(p_insert_idx, idx_bytes);
                        let total = all_idx.len();
                        let half = total / 2;

                        let new_idx_phys = match self.fs.alloc_block() {
                            Some(p) => p,
                            None => {
                                error!("add_extent_entry: no free block for idx split");
                                return None;
                            }
                        };
                        let mut new_idx_buf = alloc::vec![0u8; BLOCK_SZ];
                        unsafe {
                            core::ptr::write_unaligned(
                                new_idx_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                                Ext4ExtentHeader {
                                    eh_magic: 0xF30A,
                                    eh_entries: (total - half) as u16,
                                    eh_max: 340,
                                    eh_depth: p_header.eh_depth,
                                    eh_generation: 0,
                                },
                            );
                        }

                        for i in 0..half {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    all_idx[i].as_ptr(),
                                    parent_ctx.data_ptr.add(12 + i * 12), 12);
                            }
                        }
                        for i in half..p_max_u {
                            unsafe {
                                core::ptr::write_bytes(
                                    parent_ctx.data_ptr.add(12 + i * 12), 0, 12);
                            }
                        }
                        unsafe {
                            &mut *(parent_ctx.data_ptr as *mut Ext4ExtentHeader)
                        }.eh_entries = half as u16;

                        for i in half..total {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    all_idx[i].as_ptr(),
                                    new_idx_buf.as_mut_ptr().add(12 + (i - half) * 12), 12);
                            }
                        }
                        self.fs.block_dev.write_block(new_idx_phys as usize, &new_idx_buf);
                        // 新索引元数据块 + 旧父节点（前半）刷盘
                        disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32;
                        flush_ctx(parent_ctx);

                        // 若旧节点首个条目变化，更新父父节点中的索引
                        let new_first_block = u32::from_le_bytes(all_idx[0][0..4].try_into().unwrap());
                        if let Some(pe_ptr) = parent_ctx.parent_entry {
                            let old_first = u32::from_le_bytes(
                                unsafe { core::slice::from_raw_parts(pe_ptr as *const u8, 4) }
                                    .try_into().unwrap(),
                            );
                            if new_first_block != old_first {
                                unsafe { (pe_ptr as *mut u32).write_unaligned(new_first_block); }
                            }
                        }

                        let mut new_parent_idx = [0u8; 12];
                        new_parent_idx[0..4].copy_from_slice(&all_idx[half][0..4]);
                        new_parent_idx[4..8]
                            .copy_from_slice(&(new_idx_phys as u32).to_le_bytes());
                        new_parent_idx[8..10]
                            .copy_from_slice(&0u16.to_le_bytes());

                        pending = Some((new_parent_idx, parent_ctx.parent_entry));
                        // 继续向上传播
                    }
                    None => {
                        // --- 到达根节点：根分裂 ---
                        // 当前 i_block 中存的是分裂后的前半部分
                        // 把 i_block 内容复制到新块 child_0，
                        // i_block 变为深度+1 的索引节点，指向 child_0 和新兄弟
                        let root_header = unsafe {
                            &*(disk_inode.i_block.as_ptr() as *const Ext4ExtentHeader)
                        };
                        let root_entries = root_header.eh_entries as usize;
                        let root_depth = root_header.eh_depth;

                        let child0_phys = match self.fs.alloc_block() {
                            Some(p) => p,
                            None => {
                                error!("add_extent_entry: no free block for root split");
                                return None;
                            }
                        };
                        let mut child0_buf = alloc::vec![0u8; BLOCK_SZ];
                        let copy_len = 12 + root_entries * 12;
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                disk_inode.i_block.as_ptr(),
                                child0_buf.as_mut_ptr(),
                                copy_len,
                            );
                        }
                        unsafe {
                            let h = &mut *(child0_buf.as_mut_ptr() as *mut Ext4ExtentHeader);
                            h.eh_max = 340;
                        }
                        self.fs.block_dev.write_block(child0_phys as usize, &child0_buf);
                        // child_0 元数据块
                        disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32;

                        // child_1 从阶段一已写入磁盘，从 idx_bytes 提取物理块号
                        let child1_phys = {
                            let lo = u32::from_le_bytes(idx_bytes[4..8].try_into().unwrap());
                            let hi = u16::from_le_bytes(idx_bytes[8..10].try_into().unwrap());
                            ((hi as u64) << 32) | (lo as u64)
                        };

                        // 索引条目 0：指向 child_0
                        let idx0_block = u32::from_le_bytes(
                            unsafe {
                                core::slice::from_raw_parts(child0_buf.as_ptr().add(12), 4)
                            }
                            .try_into()
                            .unwrap(),
                        );
                        let mut idx0 = [0u8; 12];
                        idx0[0..4].copy_from_slice(&idx0_block.to_le_bytes());
                        idx0[4..8].copy_from_slice(&(child0_phys as u32).to_le_bytes());
                        idx0[8..10].copy_from_slice(&0u16.to_le_bytes());

                        // 重写 i_block 为新根
                        unsafe {
                            core::ptr::write_unaligned(
                                disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentHeader,
                                Ext4ExtentHeader {
                                    eh_magic: 0xF30A,
                                    eh_entries: 2,
                                    eh_max: 4,
                                    eh_depth: root_depth + 1,
                                    eh_generation: 0,
                                },
                            );
                            core::ptr::copy_nonoverlapping(
                                idx0.as_ptr(),
                                disk_inode.i_block.as_mut_ptr().add(12),
                                12,
                            );
                            core::ptr::copy_nonoverlapping(
                                idx_bytes.as_ptr(),
                                disk_inode.i_block.as_mut_ptr().add(24),
                                12,
                            );
                            core::ptr::write_bytes(
                                disk_inode.i_block.as_mut_ptr().add(36), 0, 24);
                        }
                        return Some(physical_block_id);
                    }
                }
            }
        })
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

        if self.is_symlink() && disk_size_bytes < 60 {
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
        info!("raw_write_at: ino={} offset={} len={} old_size={}", self.inode_id, offset, buf.len(), old_size_bytes);
        if self.is_symlink() && end <= 60 {
            let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
            let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
            block_cache.lock().modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
                // i_block 已经是 [u8; 60]，直接读写
                let mut i_block_bytes = disk_inode.i_block;
                i_block_bytes[offset..end].copy_from_slice(buf);
                disk_inode.i_block = i_block_bytes;
                
                if end > old_size_bytes {
                    disk_inode.i_size_lo = end as u32;
                    disk_inode.i_size_high = 0;
                }
            });
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
                            let (block_id, inode_offset) = self.fs.get_inode_pos(self.inode_id);
                            let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
                            block_cache.lock().modify(inode_offset, |disk_inode: &mut Ext4InodeDisk| {
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
            self.fs.block_dev.read_block(physical_block_id as usize, &mut temp_buf);

            let write_len = core::cmp::min(block_size - block_pos, end - curr_offset);
            temp_buf[block_pos..block_pos + write_len].copy_from_slice(&buf[actual_write..actual_write + write_len]);

            self.fs.block_dev.write_block(physical_block_id as usize, &temp_buf);

            actual_write += write_len;
            curr_offset += write_len;
        }

        // 更新磁盘 Inode 的 size (以字节为单位)
        let new_size_bytes = offset + actual_write;
        info!("raw_write_at: done actual_write={} new_size={}", actual_write, new_size_bytes);
        
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
                let header_ptr = disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentHeader;
                let mut header: Ext4ExtentHeader = unsafe { header_ptr.read_unaligned() };

                if header.eh_magic != 0xF30A {
                    disk_inode.i_size_lo = len as u32;
                    disk_inode.i_size_high = (len >> 32) as u32;
                    return;
                }
                if header.eh_depth != 0 {
                    warn!("[ext4 truncate] depth > 0 not supported, only updating size");
                    disk_inode.i_size_lo = len as u32;
                    disk_inode.i_size_high = (len >> 32) as u32;
                    return;
                }

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
        });

        // 第二阶段：释放收集到的物理块（在 cache 锁外进行，避免死锁）
        for phys in blocks_to_free {
            self.fs.dealloc_block(phys);
        }

        true
    }
    
}
