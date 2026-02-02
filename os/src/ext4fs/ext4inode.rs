use alloc::sync::Arc;
use alloc::vec;
use alloc::string::String;
use super::{ext4::Ext4FS};
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

impl Ext4Inode {
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

    /// 根据逻辑块号寻找对应的物理块号 (支持 Extents 和直接块)
    pub fn find_physical_block(&self, logical_block_id: u32) -> u32 {
        if self.flags & 0x80000 != 0 {
            // Extents 模式 (EXT4_EXTENTS_FL = 0x80000)
            let magic = (self.i_block[0] & 0xFFFF) as u16;
            if magic != 0xF30A { 
                return 0; 
            }
            let entries = ((self.i_block[0] >> 16) & 0xFFFF) as u16;
            let depth = ((self.i_block[1] >> 16) & 0xFFFF) as u16;
            
            // 目前仅处理叶子节点 (depth == 0)
            if depth == 0 {
                for i in 0..entries as usize {
                    let base = 3 + i * 3;
                    if base + 2 >= 15 { break; }
                    let ee_block = self.i_block[base];
                    let ee_len = (self.i_block[base + 1] & 0xFFFF) as u16;
                    let ee_start_lo = self.i_block[base + 2];
                    
                    // ee_len 如果大于 32768 表示未填充，实际长度需要减去 32768
                    let actual_len = if ee_len > 32768 { ee_len - 32768 } else { ee_len } as u32;
                    if logical_block_id >= ee_block && logical_block_id < ee_block + actual_len {
                        return ee_start_lo + (logical_block_id - ee_block);
                    }
                }
            }
            0
        } else {
            // 传统的直接块模式
            if (logical_block_id as usize) < 12 {
                self.i_block[logical_block_id as usize]
            } else {
                0 // 目前尚不支持一级/二级/三级间接块
            }
        }
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
            self.read_at(offset, &mut header_buf);
            
            let inode_id = u32::from_le_bytes(header_buf[0..4].try_into().unwrap());
            let rec_len = u16::from_le_bytes(header_buf[4..6].try_into().unwrap()) as usize;
            let name_len = header_buf[6] as usize;

            if rec_len == 0 { break; } // 防止死循环

            // 2. 如果 inode_id 不为 0，说明这是一个有效的项
            if inode_id != 0 {
                // 读取文件名
                let mut name_buf = vec![0u8; name_len];
                self.read_at(offset + 8, &mut name_buf);
                let name = String::from_utf8_lossy(&name_buf);
                trace!("Found: {}", name);
            }

            // 3. 将偏移量顺移到下一个目录项的起始位置
            // 注意：ext4 用 rec_len 来跳跃，这对应了线性表的逻辑
            offset += rec_len;
        }
    }
    /// 从文件的 offset 字节处开始，读取数据到 buf 中，返回实际读取长度
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let block_size = self.fs.superblock.block_size as usize;
        let mut actual_read = 0;
        let mut curr_offset = offset;

        // 不能超过文件总大小
        let end = core::cmp::min(offset + buf.len(), self.size as usize);
        if curr_offset >= end { return 0; }

        while curr_offset < end {
            // 1. 计算当前逻辑块号 (文件中的第几块)
            let inner_block_id = (curr_offset / block_size) as u32;
            let block_pos = curr_offset % block_size;
            
            // 2. 找到该逻辑块对应的全局物理块号
            let physical_block_id = self.find_physical_block(inner_block_id);
            if physical_block_id == 0 { break; } // 空洞文件或超出范围

            // 3. 读取块数据
            let mut temp_buf = alloc::vec![0u8; 4096];
            self.fs.block_dev.read_block(physical_block_id as usize, &mut temp_buf);

            // 4. 拷贝到输出 buf
            let read_len = core::cmp::min(block_size - block_pos, end - curr_offset);
            buf[actual_read..actual_read + read_len].copy_from_slice(&temp_buf[block_pos..block_pos + read_len]);
            
            actual_read += read_len;
            curr_offset += read_len;
        }

        actual_read
    }

    /// 从文件的 offset 字节处开始，将数据写入 buf 中，返回实际写入长度
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let block_size = self.fs.superblock.block_size as usize;
        let mut actual_write = 0;
        let mut curr_offset = offset;

        // 目前仅支持对已有数据块的覆盖写入，不支持自动增长文件大小
        let end = core::cmp::min(offset + buf.len(), self.size as usize);
        if curr_offset >= end {
            return 0;
        }

        while curr_offset < end {
            // 1. 计算逻辑块号
            let inner_block_id = (curr_offset / block_size) as u32;
            let block_pos = curr_offset % block_size;

            // 2. 查找物理块号
            let physical_block_id = self.find_physical_block(inner_block_id);
            if physical_block_id == 0 {
                break;
            }

            // 3. 读-改-写 (暂未实现更高效的缓存部分写入)
            let mut temp_buf = alloc::vec![0u8; 4096];
            self.fs.block_dev.read_block(physical_block_id as usize, &mut temp_buf);

            let write_len = core::cmp::min(block_size - block_pos, end - curr_offset);
            temp_buf[block_pos..block_pos + write_len].copy_from_slice(&buf[actual_write..actual_write + write_len]);

            self.fs.block_dev.write_block(physical_block_id as usize, &temp_buf);

            actual_write += write_len;
            curr_offset += write_len;
        }

        actual_write
    }
}
