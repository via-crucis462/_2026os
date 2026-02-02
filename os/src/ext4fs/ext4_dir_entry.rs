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
        if entry.rec_len < 8 { return None; }
        Some(entry)
    }
}