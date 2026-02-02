use alloc::string::String;

pub struct DirEntry {
    pub name: String,
    pub inode_id: u32,
    pub name_hash: u64,
}

impl DirEntry {
    pub fn new(name: String, inode_id: u32) -> Self {
        let mut name_hash: u64 = 0;
        for c in name.as_bytes() {
            name_hash = (name_hash * 131).wrapping_add(*c as u64);
        }
        Self {
            name,
            inode_id,
            name_hash,
        }
    }
}
