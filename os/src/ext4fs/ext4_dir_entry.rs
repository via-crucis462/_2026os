#[repr(C, packed)]
pub struct Ext4DirEntry {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
    pub name: [u8; 255],
}

impl Ext4DirEntry {
    pub fn inode(&self) -> u32 { self.inode }
    pub fn name_len(&self) -> u8 { self.name_len }
    pub fn rec_len(&self) -> u16 { self.rec_len }

    /// 将 ext4 文件类型转换为 Linux dirent 文件类型
    /// 
    /// 用于返回正确的文件类型给用户空间
    pub fn linux_dirent_type(&self) -> u8 {
        match self.file_type {
            1 => 8,  // EXT4_FT_REG_FILE -> DT_REG
            2 => 4,  // EXT4_FT_DIR      -> DT_DIR
            3 => 2,  // EXT4_FT_CHRDEV   -> DT_CHR
            4 => 6,  // EXT4_FT_BLKDEV   -> DT_BLK
            5 => 1,  // EXT4_FT_FIFO     -> DT_FIFO
            6 => 12, // EXT4_FT_SOCK     -> DT_SOCK
            7 => 10, // EXT4_FT_SYMLINK  -> DT_LNK
            _ => 0,  // EXT4_FT_UNKNOWN  -> DT_UNKNOWN
        }
    }

    /// 计算目录项实际需要的最小长度（头部8字节 + 文件名长度，4字节对齐）
    pub fn real_len(&self) -> u16 {
        let len = 8 + self.name_len as u16;
        (len + 3) & !3
    }

    /// 构造一个新的目录项
    pub fn new_disk(inode: u32, rec_len: u16, name: &str, file_type: u8) -> Self {
        let mut name_bytes = [0u8; 255];
        let len = name.len().min(255);
        name_bytes[..len].copy_from_slice(&name.as_bytes()[..len]);
        Self {
            inode,
            rec_len,
            name_len: len as u8,
            file_type, // 已经改为使用调用者传入的类型 (1=文件, 2=目录)
            name: name_bytes,
        }
    }

    /// 获取当前目录项中的文件名字符串
    pub fn name(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[0..len]).unwrap_or("")
    }

    /// 从字节流中安全创建一个目录项的引用（或者拷贝）
    /// 注意：由于磁盘上是变长的，这里的 buf 长度通常建议至少为一个块大小或 rec_len 大小
    pub fn from_bytes(buf: &[u8]) -> Option<&Self> {
        if buf.len() < 8 { return None; } // 最小头部长度 (4+2+1+1)
        let entry = unsafe { &*(buf.as_ptr() as *const Self) };
        let rec_len = entry.rec_len as usize;
        // 防御性检查：rec_len 必须 >= 8 且不能超出缓冲区
        if rec_len < 8 || rec_len > buf.len() {
            return None;
        }
        Some(entry)
    }

    /// 安全获取文件名，限制长度不超过 rec_len 允许的范围
    pub fn safe_name(&self) -> &str {
        let max_name_len = (self.rec_len as usize).saturating_sub(8);
        let len = core::cmp::min(self.name_len as usize, max_name_len);
        if len == 0 {
            return "";
        }
        core::str::from_utf8(&self.name[0..len]).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::Ext4DirEntry;

    #[test]
    fn converts_ext4_file_types_to_linux_dirent_types() {
        let expected = [0, 8, 4, 2, 6, 1, 12, 10];

        for (ext4_type, linux_type) in expected.iter().copied().enumerate() {
            let entry = Ext4DirEntry::new_disk(1, 12, "x", ext4_type as u8);
            assert_eq!(entry.linux_dirent_type(), linux_type);
        }

        let invalid = Ext4DirEntry::new_disk(1, 12, "x", u8::MAX);
        assert_eq!(invalid.linux_dirent_type(), 0);
    }
}
