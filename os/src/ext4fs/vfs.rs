use super::ext4inode::{Ext4Inode,Ext4InodeDisk};
use super::ext4_dir_entry::Ext4DirEntry;
use super::block_cache::get_block_cache;
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
        let file_size_bytes = self.size as usize;

        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; 4096];
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

    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        self.write_at(offset, buf) // 调用 Ext4Inode 自身的方法
    }
    
    fn get_size(&self) -> usize {
        self.size as usize
    }

    fn get_stat(&self) -> crate::fs::Stat {
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        crate::fs::Stat {
            dev: 0,
            ino: self.inode_id as u64,
            mode: disk_inode.i_mode as u32,
            nlink: disk_inode.i_links_count as u32,
            uid: disk_inode.i_uid as u32,
            gid: disk_inode.i_gid as u32,
            rdev: 0,
            size: disk_inode.size() as i64,
            blksize: 512,
            blocks: disk_inode.i_blocks_lo as i64,
            atime_sec: disk_inode.i_atime as i64,
            mtime_sec: disk_inode.i_mtime as i64,
            ctime_sec: disk_inode.i_ctime as i64,
            ..Default::default()
        }
    }
    fn create_file(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>> {
        if !self.is_dir() {
            return None;
        }
        // 1. 判断目录项
        if self.find(name).is_some() {
            return None;
        }
        // 2. 分配 Inode_id
        let new_inode_id = self.fs.alloc_inode()?;
        
        // 3. 在磁盘上初始化该 Inode 结构
        let (block_id, offset) = self.fs.get_inode_pos(new_inode_id);
        let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        block_cache.lock().modify(offset, |disk_inode: &mut Ext4InodeDisk| {
            // 设置基本信息
            disk_inode.i_mode = mode as u16; 
            disk_inode.i_size_lo = 0;
            disk_inode.i_size_high = 0;
            disk_inode.i_links_count = 1;
            disk_inode.i_blocks_lo = 0;
            disk_inode.i_flags = 0;
            for i in 0..15 { disk_inode.i_block[i] = 0; }
        });

        // 4. 在父目录的数据块中写入目录项 (文件类型 1)
        if !self.add_dir_entry(name, new_inode_id, 1) {
            // 失败处理（简化：返回 None，实际上可能需要回滚分配）
            return None;
        }

        // 5. 更新父目录（当前 Inode）的元数据：确保 size 至少占用了1个块
        let (p_block_id, p_offset) = self.fs.get_inode_pos(self.inode_id);
        let p_block_cache = get_block_cache(p_block_id as usize, self.fs.block_dev.clone());
        p_block_cache.lock().modify(p_offset, |p_disk_inode: &mut Ext4InodeDisk| {
            if p_disk_inode.i_size_lo < 4096 {
                p_disk_inode.i_size_lo = 4096;
            }
        });

        Some(self.fs.get_inode(new_inode_id))
    }

    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>> {
        println!("VFS: Creating directory '{}' in inode {}", name, self.inode_id);
        if !self.is_dir() {
            println!("VFS: create_dir failed - inode {} is not a directory", self.inode_id);
            return None;
        }
        // 1. 判断目录项
        if self.find(name).is_some() {
            println!("VFS: Directory '{}' already exists in inode {}", name, self.inode_id);
            return None;
        }
        // 2. 分配 Inode_id
        let new_inode_id = self.fs.alloc_inode()?;
        println!("VFS: Creating directory '{}' with inode id {}", name, new_inode_id);
        // 3. 在磁盘上初始化该 Inode 结构
        let (block_id, offset) = self.fs.get_inode_pos(new_inode_id);
        let block_cache = get_block_cache(block_id as usize, self.fs.block_dev.clone());
        block_cache.lock().modify(offset, |disk_inode: &mut Ext4InodeDisk| {
            // 设置基本信息
            disk_inode.i_mode = mode as u16; 
            disk_inode.i_size_lo = 0;
            disk_inode.i_size_high = 0;
            disk_inode.i_links_count = 2; // 目录初始链接数为2 (self + .)
            disk_inode.i_blocks_lo = 0;
            disk_inode.i_flags = 0;
            for i in 0..15 { disk_inode.i_block[i] = 0; }
        });

        // 4. 在父目录的数据块中写入目录项 (文件类型 2)
        if !self.add_dir_entry(name, new_inode_id, 2) {
            // 失败处理（简化：返回 None，实际上可能需要回滚分配）
            return None;
        }

        // 5. 更新父目录（当前 Inode）的元数据：链接数 +1，且确保 size 至少占用了1个块
        let (p_block_id, p_offset) = self.fs.get_inode_pos(self.inode_id);
        let p_block_cache = get_block_cache(p_block_id as usize, self.fs.block_dev.clone());
        p_block_cache.lock().modify(p_offset, |p_disk_inode: &mut Ext4InodeDisk| {
            p_disk_inode.i_links_count += 1;
            // 确保目录大小至少为1个块 (size 这里的单位保持为字节，即 4096)
            if p_disk_inode.i_size_lo < 4096 {
                p_disk_inode.i_size_lo = 4096;
            }
        });

        Some(self.fs.get_inode(new_inode_id))
    }
}