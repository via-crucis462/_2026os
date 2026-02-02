
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
}

/// 内存中的块组结构 (对应 efs.rs 中的 BlockGroup)
/// 这里只保留了核心字段，方便内存中访问
#[derive(Debug)]
pub struct Ext4Group {
    pub block_bitmap_id: u32,   // 块位图所在的块号
    pub inode_bitmap_id: u32,   // Inode位图所在的块号
    pub inode_table_id: u32,    // Inode表起始块号
    pub free_blocks_count: u16, // 本组空闲块数
    pub free_inodes_count: u16, // 本组空闲Inode数
}
    
impl Ext4Group {
    pub fn new(desc: &Ext4GroupDescDisk) -> Self {
        // 转换为内存结构
        Self { 
            block_bitmap_id: desc.bg_block_bitmap_lo,
            inode_bitmap_id: desc.bg_inode_bitmap_lo,
            inode_table_id: desc.bg_inode_table_lo,
            free_blocks_count: desc.bg_free_blocks_count_lo,
            free_inodes_count: desc.bg_free_inodes_count_lo,
        }
    }
}