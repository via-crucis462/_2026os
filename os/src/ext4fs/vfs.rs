use super::ext4inode::Ext4Inode;
use super::ext4_dir_entry::Ext4DirEntry;
use crate::fs::DirEntry;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;
use alloc::sync::Arc;
lazy_static! {
    pub static ref ENTRIES_TABLE: Mutex<Vec<DirEntry>> = Mutex::new(Vec::new());
}
use crate::fs::VfsInode;
impl VfsInode for Ext4Inode {
    fn ls<'a>(&'a self) -> Box<dyn Iterator<Item = DirEntry> + 'a> {
        if !self.is_dir() {
            return Box::new(core::iter::empty());
        }
        let mut entries = alloc::vec::Vec::new();
        let mut offset = 0;
        let file_size = self.size as usize;

        while offset < file_size {
            let mut buf = [0u8; 4096];
            let read_len = self.read_at(offset, &mut buf);
            if read_len == 0 { break; }

            let mut block_offset = 0;
            while block_offset < read_len {
                if let Some(dirent) = Ext4DirEntry::from_bytes(&buf[block_offset..]) {
                    if dirent.inode() != 0 && dirent.name_len() > 0 {
                        entries.push(DirEntry::new(dirent.name().to_string(), dirent.inode()));
                    }
                    let rec_len = dirent.rec_len() as usize;
                    if rec_len == 0 { break; }
                    block_offset += rec_len;
                } else {
                    break;
                }
            }
            offset += read_len;
        }
        Box::new(entries.into_iter())
    }

    fn init(&self) {
        let mut table = ENTRIES_TABLE.lock();
        for entry in self.ls() {
            table.push(entry);
        }
    }

    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        if !self.is_dir() {
            return None;
        }
        // 遍历目录项迭代器
        for entry in self.ls() {
            if entry.name == name {
                // 找到了名称匹配的项，去磁盘读它的 Inode
                let disk_inode = self.fs.get_disk_inode(entry.inode_id);
                return Some(Arc::new(Ext4Inode::new(
                    entry.inode_id,
                    &disk_inode,
                    self.fs.clone(),
                    Some(self.inode_id),
                )));
            }
        }
        None
    }

    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.read_at(offset, buf) // 调用 Ext4Inode 自身的方法
    }

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize {
        // ext4 写入目前可以先返回 0 或 panic，等后续实现
        0
    }
    
    fn get_size(&self) -> usize {
        self.size as usize
    }


}