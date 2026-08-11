use super::*;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

const EXT4_BG_INODE_UNINIT: u16 = 0x0001;
const EXT4_BG_BLOCK_UNINIT: u16 = 0x0002;

#[allow(dead_code)]
pub struct Ext4FS{
    pub block_dev: Arc<dyn BlockDevice>,
    pub superblock: Ext4SuperBlock,
    pub block_groups: Vec<Arc<Mutex<Ext4Group>>>,
    /// Serialize allocation and raw metadata updates.  ext4 metadata is a
    /// cross-block transaction even when this implementation has no journal.
    pub metadata_lock: Mutex<()>,
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
        if superblock.has_bigalloc() {
            panic!("[ext4] bigalloc filesystems are not supported for writable mounts");
        }
        if superblock.block_size as usize != BLOCK_SZ {
            panic!(
                "[ext4] unsupported block size {}; this driver requires {}",
                superblock.block_size,
                BLOCK_SZ,
            );
        }
        if superblock.incompat_features & 0x0010 != 0 {
            panic!("[ext4] META_BG filesystems are not supported for writable mounts");
        }
        // This filesystem updates metadata directly and deliberately does not
        // implement JBD2.  Replaying an older journal after our writes could
        // restore stale allocation metadata over the new contents.
        if superblock.has_journal() && superblock.needs_recovery() {
            panic!(
                "[ext4] refusing writable mount with an unrecovered journal; run fsck first"
            );
        }
        let group_num = superblock.group_num();
        let mut block_groups = Vec::new();

        let desc_size = superblock.desc_size as usize;
        if desc_size < 32 || desc_size > BLOCK_SZ || BLOCK_SZ % desc_size != 0 {
            panic!("[ext4] unsupported group descriptor size {}", desc_size);
        }
        // 组描述符表可能跨多个块，全部读入内核缓冲区再解析
        let desc_blocks = (group_num as usize * desc_size + BLOCK_SZ - 1) / BLOCK_SZ;
        let mut buf = alloc::vec![0u8; desc_blocks * BLOCK_SZ];
        let gdt_start_block = superblock.first_data_block as usize + 1;
        for b in 0..desc_blocks {
            block_dev.read_block(
                gdt_start_block + b,
                &mut buf[b * BLOCK_SZ..(b + 1) * BLOCK_SZ],
            );
        }

        for i in 0..group_num {
            let offset = i as usize * desc_size;
            let group = Ext4Group::new(i, &buf[offset..offset + desc_size]);
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
            metadata_lock: Mutex::new(()),
            inodes: Mutex::new(BTreeMap::new()),
        }
    }

    fn group_desc_pos(&self, group_id: u32) -> (usize, usize) {
        let byte_offset = group_id as usize * self.superblock.desc_size as usize;
        (
            self.superblock.first_data_block as usize + 1 + byte_offset / BLOCK_SZ,
            byte_offset % BLOCK_SZ,
        )
    }

    fn update_group_desc(&self, group_id: u32, f: impl FnOnce(&mut [u8])) {
        let (block_id, offset) = self.group_desc_pos(group_id);
        let cache = get_block_cache(block_id, self.block_dev.clone());
        let mut block = cache.lock();
        let bytes = block.frame.get_bytes_array();
        let desc_size = self.superblock.desc_size as usize;
        let desc = &mut bytes[offset..offset + desc_size];
        f(desc);
        if self.superblock.has_metadata_csum() {
            let _ = checksum::set_group_desc_checksum(
                self.superblock.metadata_checksum_seed(),
                group_id,
                desc,
            );
        }
        block.dirty = true;
        block.state = crate::drivers::block::cache::CacheState::Dirty;
    }

    fn adjust_superblock_counts(&self, free_blocks_delta: i64, free_inodes_delta: i64) {
        let cache = get_block_cache(0, self.block_dev.clone());
        let mut block = cache.lock();
        let bytes = block.frame.get_bytes_array();
        let sb = &mut bytes[superblock::EXT4_SUPERBLOCK_OFFSET
            ..superblock::EXT4_SUPERBLOCK_OFFSET + checksum::EXT4_SUPERBLOCK_SIZE];

        fn read_u32(raw: &[u8], off: usize) -> u32 {
            u32::from_le_bytes(raw[off..off + 4].try_into().unwrap())
        }
        fn write_u32(raw: &mut [u8], off: usize, value: u32) {
            raw[off..off + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn adjust_u64(raw: &mut [u8], lo_off: usize, hi_off: usize, delta: i64) {
            let value = read_u32(raw, lo_off) as u64 | ((read_u32(raw, hi_off) as u64) << 32);
            let value = if delta >= 0 {
                value.saturating_add(delta as u64)
            } else {
                value.saturating_sub((-delta) as u64)
            };
            write_u32(raw, lo_off, value as u32);
            write_u32(raw, hi_off, (value >> 32) as u32);
        }

        // ext4 superblock offsets: s_free_blocks_count_lo=0x0c,
        // s_free_inodes_count=0x10, s_free_blocks_count_hi=0x158.
        adjust_u64(sb, 0x0c, 0x158, free_blocks_delta);
        let free_inodes = read_u32(sb, 0x10);
        let free_inodes = if free_inodes_delta >= 0 {
            free_inodes.saturating_add(free_inodes_delta as u32)
        } else {
            free_inodes.saturating_sub((-free_inodes_delta) as u32)
        };
        write_u32(sb, 0x10, free_inodes);
        if self.superblock.has_metadata_csum() {
            let _ = checksum::set_superblock_checksum(sb);
        }
        block.dirty = true;
        block.state = crate::drivers::block::cache::CacheState::Dirty;
    }

    fn group_inode_count(&self, group_id: u32) -> u32 {
        let first = group_id.saturating_mul(self.superblock.inodes_per_group);
        self.superblock
            .total_inodes
            .saturating_sub(first)
            .min(self.superblock.inodes_per_group)
    }

    fn group_block_count(&self, group_id: u32) -> u32 {
        let first = self.superblock.first_data_block
            .saturating_add(group_id.saturating_mul(self.superblock.blocks_per_group));
        self.superblock
            .total_blocks
            .saturating_sub(first)
            .min(self.superblock.blocks_per_group)
    }

    fn bitmap_checksum_len(&self, bitmap_kind: checksum::BitmapKind) -> Option<usize> {
        let bits = match bitmap_kind {
            checksum::BitmapKind::Block => self.superblock.blocks_per_group,
            checksum::BitmapKind::Inode => self.superblock.inodes_per_group,
        };
        let bytes = (bits / 8) as usize;
        if bytes == 0 || bytes > BLOCK_SZ {
            None
        } else {
            Some(bytes)
        }
    }

    fn set_bitmap_bit(bitmap: &mut [u8], bit: u32) -> bool {
        let byte = (bit / 8) as usize;
        let mask = 1u8 << (bit % 8);
        let Some(value) = bitmap.get_mut(byte) else {
            return false;
        };
        *value |= mask;
        true
    }

    fn reserve_physical_range_in_group_bitmap(
        &self,
        group_id: u32,
        bitmap: &mut [u8],
        physical_start: u32,
        count: u32,
    ) -> bool {
        let group_start = self.superblock.first_data_block.saturating_add(
            group_id.saturating_mul(self.superblock.blocks_per_group),
        );
        let group_end = group_start.saturating_add(self.group_block_count(group_id));
        let range_end = physical_start.saturating_add(count);
        let start = core::cmp::max(group_start, physical_start);
        let end = core::cmp::min(group_end, range_end);
        for physical in start..end {
            if !Self::set_bitmap_bit(bitmap, physical - group_start) {
                return false;
            }
        }
        true
    }

    fn is_power_of_group(mut group_id: u32, factor: u32) -> bool {
        if group_id < factor {
            return false;
        }
        while group_id % factor == 0 {
            group_id /= factor;
        }
        group_id == 1
    }

    fn group_has_super_backup(&self, group_id: u32) -> bool {
        const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
        const EXT4_FEATURE_COMPAT_SPARSE_SUPER2: u32 = 0x0200;

        if group_id == 0 {
            return true;
        }
        if self.superblock.compat_features & EXT4_FEATURE_COMPAT_SPARSE_SUPER2 != 0 {
            return self.superblock.backup_bgs.contains(&group_id);
        }
        if self.superblock.ro_compat_features & EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER == 0 {
            return true;
        }
        group_id == 1
            || Self::is_power_of_group(group_id, 3)
            || Self::is_power_of_group(group_id, 5)
            || Self::is_power_of_group(group_id, 7)
    }

    fn count_free_bitmap_bits(bitmap: &[u8], bit_count: u32) -> u32 {
        let mut free = 0;
        for bit in 0..bit_count {
            if bitmap[(bit / 8) as usize] & (1 << (bit % 8)) == 0 {
                free += 1;
            }
        }
        free
    }

    /// Build an ext4 lazy-initialized block bitmap from immutable layout
    /// metadata.  A BLOCK_UNINIT group contains no allocated file data, but
    /// it can still contain backup superblocks and flex-group metadata.
    fn materialize_block_bitmap(
        &self,
        group_id: u32,
        expected_free_blocks: u32,
        bitmap: &mut [u8],
    ) -> bool {
        const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
        if self.superblock.incompat_features & EXT4_FEATURE_INCOMPAT_META_BG != 0 {
            // The descriptor placement rules change with META_BG.  Refusing
            // allocation is preferable to treating an unknown backup GDT as
            // free data space.
            return false;
        }

        let Some(checksum_bytes) = self.bitmap_checksum_len(checksum::BitmapKind::Block) else {
            return false;
        };
        let valid_bits = self.group_block_count(group_id);
        if valid_bits > (checksum_bytes * 8) as u32 {
            return false;
        }
        bitmap.fill(0);
        for bit in valid_bits..(checksum_bytes * 8) as u32 {
            if !Self::set_bitmap_bit(bitmap, bit) {
                return false;
            }
        }

        if self.group_has_super_backup(group_id) {
            let gdt_blocks = (self.superblock.group_num() as usize
                * self.superblock.desc_size as usize
                + BLOCK_SZ - 1)
                / BLOCK_SZ;
            let backup_blocks = 1u32
                .saturating_add(gdt_blocks as u32)
                .saturating_add(self.superblock.reserved_gdt_blocks as u32);
            if !self.reserve_physical_range_in_group_bitmap(
                group_id,
                bitmap,
                self.superblock.first_data_block.saturating_add(
                    group_id.saturating_mul(self.superblock.blocks_per_group),
                ),
                backup_blocks,
            ) {
                return false;
            }
        }

        let inode_table_blocks = (self.superblock.inodes_per_group as u64
            * self.superblock.inode_size as u64
            + self.superblock.block_size as u64
            - 1)
            / self.superblock.block_size as u64;
        if inode_table_blocks > u32::MAX as u64 {
            return false;
        }
        for group_mutex in &self.block_groups {
            let group = group_mutex.lock();
            if !self.reserve_physical_range_in_group_bitmap(
                group_id,
                bitmap,
                group.block_bitmap_id,
                1,
            ) || !self.reserve_physical_range_in_group_bitmap(
                group_id,
                bitmap,
                group.inode_bitmap_id,
                1,
            ) || !self.reserve_physical_range_in_group_bitmap(
                group_id,
                bitmap,
                group.inode_table_id,
                inode_table_blocks as u32,
            ) {
                return false;
            }
        }

        Self::count_free_bitmap_bits(bitmap, valid_bits) == expected_free_blocks
    }

    fn materialize_inode_bitmap(
        &self,
        group_id: u32,
        expected_free_inodes: u32,
        bitmap: &mut [u8],
    ) -> bool {
        let Some(checksum_bytes) = self.bitmap_checksum_len(checksum::BitmapKind::Inode) else {
            return false;
        };
        let valid_bits = self.group_inode_count(group_id);
        if valid_bits > (checksum_bytes * 8) as u32 {
            return false;
        }
        bitmap.fill(0);
        for bit in valid_bits..(checksum_bytes * 8) as u32 {
            if !Self::set_bitmap_bit(bitmap, bit) {
                return false;
            }
        }
        if group_id == 0 {
            for bit in 0..self.superblock.first_ino.saturating_sub(1).min(valid_bits) {
                if !Self::set_bitmap_bit(bitmap, bit) {
                    return false;
                }
            }
        }
        Self::count_free_bitmap_bits(bitmap, valid_bits) == expected_free_inodes
    }

    fn update_group_counts(
        &self,
        group_id: u32,
        free_blocks: u32,
        free_inodes: u32,
        used_dirs: u32,
        itable_unused: u32,
        flags: u16,
        bitmap: &[u8],
        bitmap_kind: checksum::BitmapKind,
    ) {
        self.update_group_desc(group_id, |desc| {
            fn put_u16(raw: &mut [u8], off: usize, value: u16) {
                raw[off..off + 2].copy_from_slice(&value.to_le_bytes());
            }
            let (free_blocks_lo, free_blocks_hi) = (0x0c, 0x2c);
            let (free_inodes_lo, free_inodes_hi) = (0x0e, 0x2e);
            let (used_dirs_lo, used_dirs_hi) = (0x10, 0x30);
            let flags_off = 0x12;
            let (itable_lo, itable_hi) = (0x1c, 0x32);
            put_u16(desc, free_blocks_lo, free_blocks as u16);
            put_u16(desc, free_inodes_lo, free_inodes as u16);
            put_u16(desc, used_dirs_lo, used_dirs as u16);
            put_u16(desc, flags_off, flags);
            put_u16(desc, itable_lo, itable_unused as u16);
            if desc.len() >= 64 {
                put_u16(desc, free_blocks_hi, (free_blocks >> 16) as u16);
                put_u16(desc, free_inodes_hi, (free_inodes >> 16) as u16);
                put_u16(desc, used_dirs_hi, (used_dirs >> 16) as u16);
                put_u16(desc, itable_hi, (itable_unused >> 16) as u16);
            }
            if self.superblock.has_metadata_csum() {
                if let Some(checksum_bytes) = self.bitmap_checksum_len(bitmap_kind) {
                    let _ = checksum::set_bitmap_checksum(
                        self.superblock.metadata_checksum_seed(),
                        &bitmap[..checksum_bytes],
                        desc,
                        bitmap_kind,
                    );
                }
            }
        });
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
        let _metadata_guard = self.metadata_lock.lock();
        for (group_id, group_mutex) in self.block_groups.iter().enumerate() {
            let group_id = group_id as u32;
            let (needs_initialization, expected_free_inodes) = {
                let group = group_mutex.lock();
                (
                    group.flags & EXT4_BG_INODE_UNINIT != 0,
                    group.free_inodes_count,
                )
            };
            if expected_free_inodes == 0 {
                continue;
            }
            let mut initialized_bitmap = [0u8; BLOCK_SZ];
            if needs_initialization
                && !self.materialize_inode_bitmap(
                    group_id,
                    expected_free_inodes,
                    &mut initialized_bitmap,
                )
            {
                warn!(
                    "[ext4] refusing inode allocation from uninitialized group {} with an unknown bitmap layout",
                    group_id,
                );
                continue;
            }
            let mut group = group_mutex.lock();
            if group.free_inodes_count > 0 {
                let bitmap_block = group.inode_bitmap_id;
                let mut buf = [0u8; BLOCK_SZ];
                if needs_initialization {
                    buf.copy_from_slice(&initialized_bitmap);
                } else {
                    self.block_dev.read_block(bitmap_block as usize, &mut buf);
                }
                let valid_bits = self.group_inode_count(group_id);

                let mut found = None;
                for bit in 0..valid_bits {
                    let byte_idx = (bit / 8) as usize;
                    let bit_idx = (bit % 8) as u8;
                    if (buf[byte_idx] & (1 << bit_idx)) == 0 {
                        buf[byte_idx] |= 1 << bit_idx;
                        found = Some((byte_idx, bit_idx));
                        break;
                    }
                }

                if let Some((byte_idx, bit_idx)) = found {
                    self.block_dev.write_block(bitmap_block as usize, &buf);
                    group.free_inodes_count -= 1;
                    let bit = byte_idx as u32 * 8 + bit_idx as u32;
                    let used_before = valid_bits.saturating_sub(group.itable_unused.min(valid_bits));
                    if bit >= used_before {
                        group.itable_unused = valid_bits.saturating_sub(bit + 1);
                    }
                    if needs_initialization {
                        group.flags &= !EXT4_BG_INODE_UNINIT;
                    }
                    self.update_group_counts(
                        group_id,
                        group.free_blocks_count,
                        group.free_inodes_count,
                        group.used_dirs_count,
                        group.itable_unused,
                        group.flags,
                        &buf,
                        checksum::BitmapKind::Inode,
                    );
                    self.adjust_superblock_counts(0, -1);
                    let inode_id = group_id * self.superblock.inodes_per_group + bit + 1;
                    return Some(inode_id);
                }
            }
        }
        None
    }

    pub fn alloc_block(&self) -> Option<u32> {
        let _metadata_guard = self.metadata_lock.lock();
        for (group_id, group_mutex) in self.block_groups.iter().enumerate() {
            let group_id = group_id as u32;
            let (needs_initialization, expected_free_blocks) = {
                let group = group_mutex.lock();
                (
                    group.flags & EXT4_BG_BLOCK_UNINIT != 0,
                    group.free_blocks_count,
                )
            };
            if expected_free_blocks == 0 {
                continue;
            }
            let mut initialized_bitmap = [0u8; BLOCK_SZ];
            if needs_initialization
                && !self.materialize_block_bitmap(
                    group_id,
                    expected_free_blocks,
                    &mut initialized_bitmap,
                )
            {
                warn!(
                    "[ext4] refusing block allocation from uninitialized group {} with an unknown bitmap layout",
                    group_id,
                );
                continue;
            }
            let mut group = group_mutex.lock();
            if group.free_blocks_count > 0 {
                let bitmap_block = group.block_bitmap_id;
                let mut buf = [0u8; BLOCK_SZ];
                if needs_initialization {
                    buf.copy_from_slice(&initialized_bitmap);
                } else {
                    self.block_dev.read_block(bitmap_block as usize, &mut buf);
                }
                let valid_bits = self.group_block_count(group_id);

                let mut found = None;
                for bit in 0..valid_bits {
                    let byte_idx = (bit / 8) as usize;
                    let bit_idx = (bit % 8) as u8;
                    if (buf[byte_idx] & (1 << bit_idx)) == 0 {
                        buf[byte_idx] |= 1 << bit_idx;
                        found = Some((byte_idx, bit_idx));
                        break;
                    }
                }

                if let Some((byte_idx, bit_idx)) = found {
                    self.block_dev.write_block(bitmap_block as usize, &buf);
                    group.free_blocks_count -= 1;
                    let bit = byte_idx as u32 * 8 + bit_idx as u32;
                    if needs_initialization {
                        group.flags &= !EXT4_BG_BLOCK_UNINIT;
                    }
                    self.update_group_counts(
                        group_id,
                        group.free_blocks_count,
                        group.free_inodes_count,
                        group.used_dirs_count,
                        group.itable_unused,
                        group.flags,
                        &buf,
                        checksum::BitmapKind::Block,
                    );
                    self.adjust_superblock_counts(-1, 0);
                    let block_id = group_id * self.superblock.blocks_per_group
                        + bit
                        + self.superblock.first_data_block;
                    return Some(block_id);
                }
            }
        }
        None
    }

    pub fn dealloc_inode(&self, inode_id: u32) {
        let _metadata_guard = self.metadata_lock.lock();
        if inode_id == 0 || inode_id > self.superblock.total_inodes {
            return;
        }
        let inodes_per_group = self.superblock.inodes_per_group;
        let group_idx = (inode_id - 1) / inodes_per_group;
        let inode_idx = (inode_id - 1) % inodes_per_group;

        let mut group = self.block_groups[group_idx as usize].lock();
        let bitmap_block = group.inode_bitmap_id;
        let mut buf = [0u8; BLOCK_SZ];
        self.block_dev.read_block(bitmap_block as usize, &mut buf);
        let valid_bits = self.group_inode_count(group_idx);
        if inode_idx >= valid_bits {
            return;
        }
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = inode_idx % 8;
        let bit = 1u8 << bit_idx;
        if buf[byte_idx] & bit == 0 {
            // 该 inode 已被释放过（例如失效对象重复 Drop），幂等返回，
            // 避免 free_inodes_count 重复统计
            return;
        }
        buf[byte_idx] &= !bit;
        self.block_dev.write_block(bitmap_block as usize, &buf);

        group.free_inodes_count += 1;
        self.update_group_counts(
            group_idx,
            group.free_blocks_count,
            group.free_inodes_count,
            group.used_dirs_count,
            group.itable_unused,
            group.flags,
            &buf,
            checksum::BitmapKind::Inode,
        );
        self.adjust_superblock_counts(0, 1);
    }

    pub fn dealloc_block(&self, block_id: u32) {
        let _metadata_guard = self.metadata_lock.lock();
        let blocks_per_group = self.superblock.blocks_per_group;
        let first_data_block = self.superblock.first_data_block;

        if block_id < first_data_block || block_id >= self.superblock.total_blocks {
            return;
        }

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
        let valid_bits = self.group_block_count(group_idx);
        if block_idx >= valid_bits {
            return;
        }
        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = block_idx % 8;
        let bit = 1u8 << bit_idx;
        if buf[byte_idx] & bit == 0 {
            return;
        }
        buf[byte_idx] &= !bit;
        self.block_dev.write_block(bitmap_block as usize, &buf);

        group.free_blocks_count += 1;
        self.update_group_counts(
            group_idx,
            group.free_blocks_count,
            group.free_inodes_count,
            group.used_dirs_count,
            group.itable_unused,
            group.flags,
            &buf,
            checksum::BitmapKind::Block,
        );
        self.adjust_superblock_counts(1, 0);

        // 物理块已释放：作废并丢弃它的缓存，不回写。
        // 否则旧文件的数据会留在缓存里，重新分配后新所有者会读到旧数据，
        // 脏缓存还可能把旧数据写回磁盘。
        invalidate_block_cache(block_id as usize);
    }

    /// Update the per-group directory count when a directory inode is linked
    /// or unlinked.  The bitmap is unchanged, so only descriptor fields and
    /// the descriptor checksum need to be rewritten here.
    pub fn adjust_used_dirs(&self, inode_id: u32, delta: i32) {
        if inode_id == 0 || inode_id > self.superblock.total_inodes {
            return;
        }
        let _metadata_guard = self.metadata_lock.lock();
        let group_id = (inode_id - 1) / self.superblock.inodes_per_group;
        let mut group = self.block_groups[group_id as usize].lock();
        group.used_dirs_count = if delta >= 0 {
            group.used_dirs_count.saturating_add(delta as u32)
        } else {
            group.used_dirs_count.saturating_sub((-delta) as u32)
        };
        let used_dirs = group.used_dirs_count;
        self.update_group_desc(group_id, |desc| {
            desc[0x10..0x12].copy_from_slice(&(used_dirs as u16).to_le_bytes());
            if desc.len() >= 64 {
                desc[0x30..0x32].copy_from_slice(&((used_dirs >> 16) as u16).to_le_bytes());
            }
        });
    }

    pub fn decrease_link_count(self: &Arc<Self>, inode_id: u32) -> u16 {
        block_modify_inode(self, inode_id, |disk_inode: &mut Ext4InodeDisk| {
            if disk_inode.i_links_count > 0 {
                disk_inode.i_links_count -= 1;
            }
        });
        self.get_disk_inode(inode_id).i_links_count
    }

    pub fn adjust_link_count(self: &Arc<Self>, inode_id: u32, delta: i16) {
        block_modify_inode(self, inode_id, |disk_inode: &mut Ext4InodeDisk| {
            if delta >= 0 {
                disk_inode.i_links_count = disk_inode.i_links_count.saturating_add(delta as u16);
            } else {
                disk_inode.i_links_count = disk_inode.i_links_count.saturating_sub((-delta) as u16);
            }
        });
    }
}
