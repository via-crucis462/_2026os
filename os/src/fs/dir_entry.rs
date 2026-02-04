use alloc::string::String;

#[repr(C)]
pub struct DirEntry {
    pub d_ino: u64,      // 索引结点号
    pub d_off: i64,      // 到下一个 dirent 的偏移
    pub d_reclen: u16,   // 当前 dirent 的长度
    pub d_type: u8,      // 文件类型
    pub d_name: [u8; 256], // 文件名
}

impl DirEntry {
    pub fn new(name: String, inode_id: u32, d_type: u8) -> Self {
        let mut name_bytes = [0u8; 256];
        let len = name.len().min(255);
        name_bytes[..len].copy_from_slice(&name.as_bytes()[..len]);
        
        // 计算实际占据的字节数：8(ino) + 8(off) + 2(reclen) + 1(type) + len + 1(null)
        // 然后向上对齐到 8 字节边界（Linux 标准做法）
        let reclen = (8 + 8 + 2 + 1 + len + 1 + 7) & !7;

        Self {
            d_ino: inode_id as u64,
            d_off: 0, // 由调用者在遍历时填充
            d_reclen: reclen as u16,
            d_type,
            d_name: name_bytes,
        }
    }
}