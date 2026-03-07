use super::ext4inode::{Ext4Inode,Ext4InodeDisk, EXT4_EXTENTS_FL};
use super::ext4_dir_entry::Ext4DirEntry;
use super::block_cache::get_block_cache;
use alloc::sync::Arc;
use alloc::vec;
use alloc::string::String;
use crate::fs::VfsInode;
impl VfsInode for Ext4Inode {
     fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        if !self.is_dir() {
            return None;
        }
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
                        if dirent.name() == name {
                            // 找到了名称匹配的项，去磁盘读它的 Inode
                            let disk_inode = self.fs.get_disk_inode(dirent.inode());
                            return Some(Arc::new(Ext4Inode::new(
                                dirent.inode(),
                                &disk_inode,
                                self.fs.clone(),
                                Some(self.inode_id),
                            )));
                        }
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
            // 判断是否开启 extents
            if (self.fs.superblock.incompat_features & 0x40) != 0 {
                disk_inode.i_flags = EXT4_EXTENTS_FL;
                for i in 0..15 { disk_inode.i_block[i] = 0; }
                // 初始化空的 extent header: magic=0xF30A, entries=0, max=4, depth=0
                disk_inode.i_block[0] = 0xF30A; // magic: low 16 bits, entries: high 16 bits (0)
                disk_inode.i_block[1] = 0x0004; // max: low 16 bits (4), depth: high 16 bits (0)
            } else {
                disk_inode.i_flags = 0;
                for i in 0..15 { disk_inode.i_block[i] = 0; }
            }
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

    fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        self.delete_dir_entry(name)
    }

    fn getdents(&self, offset: &mut usize, buf: &mut [u8]) -> isize {
        if !self.is_dir() {
            return -1;
        }
        let mut buf_offset = 0;
        let file_size_bytes = self.size as usize;
        let buf_len = buf.len();
        let mut last_name = String::new();

        while *offset < file_size_bytes && buf_offset < buf_len {
            let mut temp_buf = vec![0u8; 4096];
            let read_len = self.read_at(*offset, &mut temp_buf);
            if read_len == 0 { break; }

            let mut block_offset = 0;
  
            while block_offset < read_len && buf_offset < buf_len {
                if let Some(ext4_dirent) = Ext4DirEntry::from_bytes(&temp_buf[block_offset..]) {
                    let disk_rec_len = ext4_dirent.rec_len() as usize;
                    if disk_rec_len == 0 { break; } // 防止死循环

                    if ext4_dirent.inode() != 0 && ext4_dirent.name_len() > 0 {
                        let name = ext4_dirent.name();
                        last_name = String::from(name);
                        
                        let name_bytes = name.as_bytes();
                        let name_len = name_bytes.len();
                        
            
                        let total_len = 19 + name_len + 1;
                        
              
                        let d_reclen = (total_len + 7) & !7; 
                        
                        if buf_offset + d_reclen > buf_len {
                            println!("VFS: getdents buffer full, stopping read");
                            break; 
                        }

        
                        let d_ino: u64 = ext4_dirent.inode() as u64;
                        buf[buf_offset..buf_offset+8].copy_from_slice(&d_ino.to_ne_bytes());
                  
                        let next_offset: i64 = (*offset + block_offset + disk_rec_len) as i64;
                        buf[buf_offset+8..buf_offset+16].copy_from_slice(&next_offset.to_ne_bytes());
                        
    
                        let reclen_u16 = d_reclen as u16;
                        buf[buf_offset+16..buf_offset+18].copy_from_slice(&reclen_u16.to_ne_bytes());
                        

                        let d_type: u8 = ext4_dirent.file_type;
                        buf[buf_offset+18] = d_type;
                        

                        buf[buf_offset+19..buf_offset+19+name_len].copy_from_slice(name_bytes);
                        

                        for i in (buf_offset+19+name_len)..(buf_offset+d_reclen) {
                            buf[i] = 0;
                        }



                        buf_offset += d_reclen; 
                    }
                    block_offset += disk_rec_len; 
                } else {
                    break;
                }
            } 

            *offset += read_len; 
        }

        if !last_name.is_empty() {
             println!("VFS: getdents last entry name: {}", last_name);
        }

        buf_offset as isize
    }
    fn get_statx(&self) -> crate::fs::Statx {
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        crate::fs::Statx {
            stx_mask: 0,
            stx_blksize: 512,
            stx_attributes: 0,
            stx_nlink: disk_inode.i_links_count as u32,
            stx_uid: disk_inode.i_uid as u32,
            stx_gid: disk_inode.i_gid as u32,
            stx_mode: disk_inode.i_mode as u16,
            stx_ino: self.inode_id as u64,
            stx_size: disk_inode.size() as u64,
            stx_blocks: disk_inode.i_blocks_lo as u64,
            stx_attributes_mask: 0,
            stx_atime: crate::fs::StatxTimestamp {
                tv_sec: disk_inode.i_atime as i64,
                tv_nsec: 0, // ext4 Inode 没有 atime 的纳秒部分
                __reserved: 0,
            },
            stx_btime: crate::fs::StatxTimestamp {
                tv_sec: 0, // ext4 Inode 没有 btime
                tv_nsec: 0,
                __reserved: 0,
            },
            stx_ctime: crate::fs::StatxTimestamp {
                tv_sec: disk_inode.i_ctime as i64,
                tv_nsec: 0, // ext4 Inode 没有 ctime 的纳秒部分
                __reserved: 0,
            },
            stx_mtime: crate::fs::StatxTimestamp {
                tv_sec: disk_inode.i_mtime as i64,
                tv_nsec: 0, // ext4 Inode 没有 mtime 的纳秒部分
                __reserved: 0,
            },
            stx_rdev_major: 0,
            stx_rdev_minor: 0,
            stx_dev_major: 0,
            stx_dev_minor: 0,
            ..Default::default()
        }
    }
}