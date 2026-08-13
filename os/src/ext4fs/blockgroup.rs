
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4GroupDescDisk {
    pub bg_block_bitmap_lo: u32,      // 块位图所在的块号 (低32位)
    pub bg_inode_bitmap_lo: u32,      // Inode位图所在的块号 (低32位)
    pub bg_inode_table_lo: u32,       // Inode表起始块号 (低32位)
    pub bg_free_blocks_count_lo: u16, // 本组空闲块数 (低16位)
    pub bg_free_inodes_count_lo: u16, // 本组空闲Inode数 (低16位)
    pub bg_used_dirs_count_lo: u16,   // 本组目录数 (低16位)
    pub bg_flags: u16,                // 标志位
    pub bg_exclude_bitmap_lo: u32,    // exclude bitmap for snapshots
    pub bg_block_bitmap_csum_lo: u16, // crc32c(s_uuid+grp_num+bbitmap) LE
    pub bg_inode_bitmap_csum_lo: u16, // crc32c(s_uuid+grp_num+ibitmap) LE
    pub bg_itable_unused_lo: u16,     // Unused inodes count
    pub bg_checksum: u16,             // crc16(sb_uuid+group+desc)
    pub bg_block_bitmap_hi: u32,
    pub bg_inode_bitmap_hi: u32,
    pub bg_inode_table_hi: u32,
    pub bg_free_blocks_count_hi: u16,
    pub bg_free_inodes_count_hi: u16,
    pub bg_used_dirs_count_hi: u16,
    pub bg_itable_unused_hi: u16,
    pub bg_exclude_bitmap_hi: u32,
    pub bg_block_bitmap_csum_hi: u16,
    pub bg_inode_bitmap_csum_hi: u16,
    pub bg_reserved: u32,
}

/// 内存中的块组结构 (对应 efs.rs 中的 BlockGroup)
/// 这里只保留了核心字段，方便内存中访问
#[derive(Debug)]
pub struct Ext4Group {
    pub group_id: u32,
    pub block_bitmap_id: u32,   // 块位图所在的块号
    pub inode_bitmap_id: u32,   // Inode位图所在的块号
    pub inode_table_id: u32,    // Inode表起始块号
    pub free_blocks_count: u32, // 本组空闲块数
    pub free_inodes_count: u32, // 本组空闲Inode数
    pub used_dirs_count: u32,   // 本组目录数
    pub itable_unused: u32,     // inode table 尾部尚未使用的 inode 数
    pub flags: u16,
}
    
impl Ext4Group {
    pub fn new(group_id: u32, desc: &[u8]) -> Self {
        assert!(desc.len() >= 32);
        let read_u16 = |offset: usize| {
            u16::from_le_bytes([desc[offset], desc[offset + 1]])
        };
        let read_u32 = |offset: usize| {
            u32::from_le_bytes([
                desc[offset],
                desc[offset + 1],
                desc[offset + 2],
                desc[offset + 3],
            ])
        };
        let high = |offset: usize| {
            if desc.len() >= offset + 2 {
                read_u16(offset) as u32
            } else {
                0
            }
        };
        if desc.len() >= 64
            && (read_u32(0x20) != 0 || read_u32(0x24) != 0 || read_u32(0x28) != 0)
        {
            panic!("[ext4] 64-bit metadata block addresses are unsupported");
        }
        Self { 
            group_id,
            block_bitmap_id: read_u32(0x00),
            inode_bitmap_id: read_u32(0x04),
            inode_table_id: read_u32(0x08),
            free_blocks_count: read_u16(0x0c) as u32 | (high(0x2c) << 16),
            free_inodes_count: read_u16(0x0e) as u32 | (high(0x2e) << 16),
            used_dirs_count: read_u16(0x10) as u32 | (high(0x30) << 16),
            itable_unused: read_u16(0x1c) as u32 | (high(0x32) << 16),
            flags: read_u16(0x12),
        }
    }
}
