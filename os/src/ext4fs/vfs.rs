use super::ext4inode::{Ext4Inode, Ext4InodeDisk, Ext4ExtentHeader, EXT4_EXTENTS_FL};
use crate::ext4fs::BLOCK_SZ;
use super::ext4_dir_entry::Ext4DirEntry;
use alloc::sync::Arc;
use alloc::vec;
use alloc::string::String;
use core::sync::atomic::Ordering;
use crate::fs::TimeSpec;
use crate::fs::{RenameError, VfsInode};
use crate::syscall::fs::Statfs;
use super::block_modify_inode;

impl VfsInode for Ext4Inode {
     fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        if !self.is_dir() {
            return None;
        }
        let mut offset = 0;
        let file_size_bytes = self.size.load(Ordering::Acquire) as usize;

        while offset < file_size_bytes {
            let mut buf = alloc::vec![0u8; 4096];
            let read_len = self.read_at(offset, &mut buf);
            if read_len == 0 { break; }

            let mut block_offset = 0;
            while block_offset < read_len {
                // 剩余数据不足以解析最小 ext4 目录项头部
                if read_len - block_offset < 8 {
                    break;
                }
                if let Some(dirent) = Ext4DirEntry::from_bytes(&buf[block_offset..]) {
                    let rec_len = dirent.rec_len() as usize;
                    // 防御：rec_len 不能为 0，也不能超出当前读取范围
                    if rec_len == 0 || rec_len > read_len - block_offset {
                        break;
                    }
                    if dirent.inode() != 0 && dirent.name_len() > 0 {
                        if dirent.safe_name() == name {
                            // 找到名称匹配的目录项，统一走 inode 映射表，
                            // 保证同一 ino 全局只有一个 Ext4Inode 对象
                            return Some(self.fs.get_inode(dirent.inode()));
                        }
                    }
                    block_offset += rec_len;
                } else {
                    break;
                }
            }
            offset += read_len;
        }
        None
    }

    fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        Ext4Inode::raw_read_at(self, offset, buf)
    }

    fn raw_write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let _write_guard = self.write_lock.lock();
        let _block_map_guard = self.block_map_lock.lock();
        let written = self.raw_write_at(offset, buf); // 调用 Ext4Inode 的底层磁盘写入
        written
    }

    fn get_shared_page(&self, logical_block: usize) -> Option<Arc<crate::mm::mmap::PageCache>> {
        let _block_map_guard = self.block_map_lock.lock();
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        if self.is_symlink() && disk_inode.size() <= 60 {
            // Fast symlinks keep their target directly in i_block. Treating a
            // missing data block as a hole would allocate a block and then
            // parse the target bytes as an extent header.
            return None;
        }

        if let Some(cache) = crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER
            .get_cached_file_page(self.inode_id as u64, logical_block)
        {
            return Some(cache);
        }

        let mut physical_block = self.find_physical_block(logical_block as u32);
        if physical_block == 0 {
            if disk_inode.i_flags & EXT4_EXTENTS_FL == 0 {
                // Newly created inodes can use classic direct blocks.  Match
                // raw_write_at here instead of unconditionally treating
                // i_block as an extent tree.
                if logical_block >= 12 {
                    return None;
                }
                let new_block = self.fs.alloc_block()?;
                block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
                    let base = logical_block * 4;
                    disk_inode.i_block[base..base + 4].copy_from_slice(&new_block.to_le_bytes());
                    disk_inode.i_blocks_lo += (BLOCK_SZ / 512) as u32;
                });
                physical_block = new_block;
            } else {
                let new_block = self.fs.alloc_block()?;
                physical_block = match self.add_extent_entry(logical_block as u32, new_block) {
                    Some(block) => block,
                    None => {
                        self.fs.dealloc_block(new_block);
                        return None;
                    }
                };
            }
            return Some(crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER
                .get_new_page_cache(
                    self.inode_id as u64,
                    logical_block,
                    physical_block as u64,
                    self.fs.block_dev.clone(),
                ));
        }
        Some(crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER
            .get_page_cache(
                self.inode_id as u64,
                logical_block,
                physical_block as u64,
                self.fs.block_dev.clone(),
            ).0)
    }

    /// 带页缓存的读取
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        if self.is_symlink() && disk_inode.size() <= 60 {
            return self.raw_read_at(offset, buf);
        }

        let page_size = crate::PAGE_SIZE;
        let file_size = self.get_size();
        if buf.is_empty() || offset >= file_size { return 0; }
        let read_end = core::cmp::min(offset + buf.len(), file_size);
        let start_page = offset / page_size;
        let end_page = (read_end - 1) / page_size;

        let mut buf_offset = 0;
        for page_idx in start_page..=end_page {
            let page_off = if page_idx == start_page { offset % page_size } else { 0 };
            let copy_len = core::cmp::min(page_size - page_off, read_end - (page_idx * page_size + page_off));

            let cache_manager = &crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER;
            let cache = if let Some(cache) = cache_manager
                .get_cached_file_page(self.inode_id as u64, page_idx)
            {
                cache
            } else {
                let physical_block = self.find_physical_block(page_idx as u32);
                if physical_block == 0 {
                    // Sparse-file holes read as zeroes. A read must never
                    // allocate blocks or mutate the extent tree.
                    buf[buf_offset..buf_offset + copy_len].fill(0);
                    buf_offset += copy_len;
                    continue;
                }
                cache_manager
                    .get_page_cache(
                        self.inode_id as u64,
                        page_idx,
                        physical_block as u64,
                        self.fs.block_dev.clone(),
                    )
                    .0
            };
            let page = cache.lock();
            // clone FrameTracker：持有期间物理页不会被释放
            let _guard = page.frame.clone();
            let src = &page.frame.get_bytes_array()[page_off..page_off + copy_len];
            buf[buf_offset..buf_offset + copy_len].copy_from_slice(src);
            buf_offset += copy_len;
        }
        buf_offset
    }

    /// 带页缓存的写入
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let _write_guard = self.write_lock.lock();
        if buf.is_empty() {
            return 0;
        }

        let disk_inode = self.fs.get_disk_inode(self.inode_id);
        let old_size = disk_inode.size() as usize;
        if self.is_symlink() && old_size <= 60 && offset + buf.len() <= 60 {
            let written = self.raw_write_at(offset, buf);
            let new_end = offset + written;
            if new_end > old_size {
                self.size.fetch_max(new_end as u64, Ordering::Relaxed);
            }
            return written;
        }

        let page_size = crate::PAGE_SIZE;
        let write_end = offset + buf.len();
        let start_page = offset / page_size;
        let end_page = (write_end - 1) / page_size;

        let mut buf_offset = 0;
        for page_idx in start_page..=end_page {
            let page_off = if page_idx == start_page { offset % page_size } else { 0 };
            let copy_len = core::cmp::min(page_size - page_off, write_end - (page_idx * page_size + page_off));

            let Some(cache) = self.get_shared_page(page_idx) else {
                error!(
                    "VFS: failed to obtain page cache for inode {} page {} while writing",
                    self.inode_id,
                    page_idx,
                );
                break;
            };
            let mut page = cache.lock();
            // clone FrameTracker：持有期间物理页不会被释放
            let _guard = page.frame.clone();
            let dst = &mut page.frame.get_bytes_array()[page_off..page_off + copy_len];
            dst.copy_from_slice(&buf[buf_offset..buf_offset + copy_len]);
            page.dirty = true;
            buf_offset += copy_len;
        }
        

        // 更新文件大小（如果需要）
        let new_end = offset + buf_offset;
        let old_size = self.get_size();
        if new_end > old_size {
            self.size.fetch_max(new_end as u64, Ordering::Relaxed);
            block_modify_inode(&self.fs, self.inode_id, |disk_inode: &mut Ext4InodeDisk| {
                disk_inode.i_size_lo = new_end as u32;
                disk_inode.i_size_high = (new_end >> 32) as u32;
            });
        }

        buf_offset
    }

    fn get_size(&self) -> usize {
        self.size.load(Ordering::Acquire) as usize
    }

    fn truncate(&self, len: usize) -> bool {
        let _write_guard = self.write_lock.lock();
        let _block_map_guard = self.block_map_lock.lock();
        let truncated = Ext4Inode::truncate(self, len);
        if truncated {
            self.size.store(len as u64, Ordering::Relaxed);
        }
        truncated
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
    fn ino(&self) -> u64 {
        self.inode_id as u64
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
        block_modify_inode(&self.fs, new_inode_id, |disk_inode| {
            disk_inode.i_mode = mode as u16; 
            disk_inode.i_size_lo = 0;
            disk_inode.i_size_high = 0;
            disk_inode.i_dtime = 0;
            disk_inode.i_links_count = 1;
            disk_inode.i_blocks_lo = 0;
            if (self.fs.superblock.incompat_features & 0x40) != 0 {
                disk_inode.i_flags = EXT4_EXTENTS_FL;
                disk_inode.i_block.fill(0);
                let header = Ext4ExtentHeader {
                    eh_magic: 0xF30A,
                    eh_entries: 0,
                    eh_max: 4,
                    eh_depth: 0,
                    eh_generation: 0,
                };
                unsafe {
                    (disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentHeader)
                        .write_unaligned(header);
                }
            } else {
                disk_inode.i_flags = 0;
                disk_inode.i_block.fill(0);
            }
        });

        // 4. 在父目录的数据块中写入目录项 (文件类型 1)
        if !self.add_dir_entry(name, new_inode_id, 1) {
            // 失败处理（简化：返回 None，实际上可能需要回滚分配）
            return None;
        }

        // 5. 更新父目录（当前 Inode）的元数据：确保 size 至少占用了1个块
        block_modify_inode(&self.fs, self.inode_id, |p_disk_inode| {
            if p_disk_inode.i_size_lo < 4096 {
                p_disk_inode.i_size_lo = 4096;
            }
        });

        Some(self.fs.get_inode(new_inode_id))
    }

    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>> {
        info!("VFS: Creating directory '{}' in inode {}", name, self.inode_id);
        if !self.is_dir() {
            info!("VFS: create_dir failed - inode {} is not a directory", self.inode_id);
            return None;
        }
        // 1. 判断目录项
        if self.find(name).is_some() {
            info!("VFS: Directory '{}' already exists in inode {}", name, self.inode_id);
            return None;
        }
        // 2. 分配 Inode_id
        let new_inode_id = self.fs.alloc_inode()?;
        info!("VFS: Creating directory '{}' with inode id {}", name, new_inode_id);
        // 3. 在磁盘上初始化该 Inode 结构
        block_modify_inode(&self.fs, new_inode_id, |disk_inode| {
            disk_inode.i_mode = mode as u16; 
            disk_inode.i_size_lo = 0;
            disk_inode.i_size_high = 0;
            disk_inode.i_dtime = 0;
            disk_inode.i_links_count = 2;
            disk_inode.i_blocks_lo = 0;
            if (self.fs.superblock.incompat_features & 0x40) != 0 {
                disk_inode.i_flags = EXT4_EXTENTS_FL;
                disk_inode.i_block.fill(0);
                let header = Ext4ExtentHeader {
                    eh_magic: 0xF30A,
                    eh_entries: 0,
                    eh_max: 4,
                    eh_depth: 0,
                    eh_generation: 0,
                };
                unsafe {
                    (disk_inode.i_block.as_mut_ptr() as *mut Ext4ExtentHeader)
                        .write_unaligned(header);
                }
            } else {
                disk_inode.i_flags = 0;
                disk_inode.i_block.fill(0);
            }
        });

        // 4. 在父目录的数据块中写入目录项 (文件类型 2)
        if !self.add_dir_entry(name, new_inode_id, 2) {
            // 失败处理（简化：返回 None，实际上可能需要回滚分配）
            return None;
        }

        // 5. 更新父目录（当前 Inode）的元数据：链接数 +1，且确保 size 至少占用了1个块
        block_modify_inode(&self.fs, self.inode_id, |p_disk_inode| {
            p_disk_inode.i_links_count += 1;
            if p_disk_inode.i_size_lo < 4096 {
                p_disk_inode.i_size_lo = 4096;
            }
        });

        Some(self.fs.get_inode(new_inode_id))
    }

    fn delete_dir_entry(&self, name: &str) -> Option<u32> {
        self.delete_dir_entry(name)
    }

    fn dec_link_count(&self) -> bool {
        self.fs.decrease_link_count(self.inode_id);
        true
    }

    fn getdents(&self, offset: &mut usize, buf: &mut [u8]) -> isize {
        if !self.is_dir() {
            return -1;
        }
        let mut buf_offset = 0;
        let file_size_bytes = self.size.load(Ordering::Acquire) as usize;
        let buf_len = buf.len();
        let mut last_name = String::new();

        while *offset < file_size_bytes && buf_offset < buf_len {
            let mut temp_buf = vec![0u8; 4096];
            let read_len = self.read_at(*offset, &mut temp_buf);
            if read_len == 0 { break; }

            let mut block_offset = 0;
            let mut buffer_full = false;
  
            while block_offset < read_len && buf_offset < buf_len {
                // 剩余数据不足以解析最小 ext4 目录项头部
                if read_len - block_offset < 8 {
                    block_offset = read_len;
                    break;
                }
                if let Some(ext4_dirent) = Ext4DirEntry::from_bytes(&temp_buf[block_offset..]) {
                    let disk_rec_len = ext4_dirent.rec_len() as usize;
                    // 防御：rec_len 不能为 0，也不能超出当前读取范围
                    if disk_rec_len == 0 || disk_rec_len > read_len - block_offset {
                        block_offset = read_len;
                        break;
                    }

                    if ext4_dirent.inode() != 0 && ext4_dirent.name_len() > 0 {
                        // 使用 safe_name() 防止从越界位置读取文件名
                        let name = ext4_dirent.safe_name();
                        last_name = String::from(name);
                        
                        let name_bytes = name.as_bytes();
                        let name_len = name_bytes.len();
                        
                        // 跳过空文件名（无效目录项）
                        if name_len == 0 {
                            block_offset += disk_rec_len;
                            continue;
                        }
            
                        let total_len = 19 + name_len + 1;
                        
              
                        let d_reclen = (total_len + 7) & !7; 
                        
                        if buf_offset + d_reclen > buf_len {
                          //  println!("VFS: getdents buffer full, stopping read");
                            buffer_full = true;
                            break; 
                        }

        
                        let d_ino: u64 = ext4_dirent.inode() as u64;
                        buf[buf_offset..buf_offset+8].copy_from_slice(&d_ino.to_ne_bytes());
                  
                        let next_offset: i64 = (*offset + block_offset + disk_rec_len) as i64;
                        buf[buf_offset+8..buf_offset+16].copy_from_slice(&next_offset.to_ne_bytes());
                        
    
                        let reclen_u16 = d_reclen as u16;
                        buf[buf_offset+16..buf_offset+18].copy_from_slice(&reclen_u16.to_ne_bytes());
                        

                        buf[buf_offset+18] = ext4_dirent.linux_dirent_type();
                        

                        buf[buf_offset+19..buf_offset+19+name_len].copy_from_slice(name_bytes);
                        

                        for i in (buf_offset+19+name_len)..(buf_offset+d_reclen) {
                            buf[i] = 0;
                        }



                        buf_offset += d_reclen; 
                    }
                    block_offset += disk_rec_len; 
                } else {
                    // from_bytes 失败（rec_len 异常或超出缓冲区）：
                    // 跳过当前块剩余字节，推进 offset 防止死循环
                    block_offset = read_len;
                    break;
                }
            } 

            if block_offset == 0 && buffer_full {
                if buf_offset == 0 {
                    return -1;
                }
                break;
            }

            *offset += block_offset; 

            if buffer_full {
                break;
            }
        }

        if !last_name.is_empty() {
           //  println!("VFS: getdents last entry name: {}", last_name);
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
    fn set_perm(&self, perm: crate::auth::PermStat) -> bool {
        block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
            disk_inode.i_mode = perm.mode.bits() as u16;
            disk_inode.i_uid = perm.uid as u16;
            disk_inode.i_gid = perm.gid as u16;
        });
        true
    }
    fn rename_dir_entry(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn VfsInode>,
        new_name: &str,
        no_replace: bool,
    ) -> Result<(), RenameError> {
        if !self.is_dir() || (new_parent.get_stat().mode & 0o170000) != 0o040000 {
            return Err(RenameError::NotDir);
        }
        if new_parent.type_name() != "Ext4Inode" {
            return Err(RenameError::CrossDevice);
        }
        if old_name.is_empty()
            || new_name.is_empty()
            || old_name.len() > 255
            || new_name.len() > 255
            || old_name == "."
            || old_name == ".."
            || new_name == "."
            || new_name == ".."
        {
            return Err(RenameError::Invalid);
        }

        let new_parent_id = u32::try_from(new_parent.ino()).map_err(|_| RenameError::CrossDevice)?;
        let same_parent = self.inode_id == new_parent_id;
        let source = self.lookup_dir_entry(old_name).ok_or(RenameError::NotFound)?;
        let new_parent_inode = self.fs.get_inode(new_parent_id);
        let target = new_parent_inode.lookup_dir_entry(new_name);
        if no_replace && target.is_some() {
            return Err(RenameError::Exists);
        }
        if same_parent && old_name == new_name {
            return Ok(());
        }

        let source_disk_inode = self.fs.get_disk_inode(source.0);
        let source_is_dir = source_disk_inode.is_dir();
        if let Some((target_inode_id, _)) = target {
            let target_inode = self.fs.get_inode(target_inode_id);
            let target_is_dir = target_inode.is_dir();
            if source_is_dir && !target_is_dir {
                return Err(RenameError::NotDir);
            }
            if !source_is_dir && target_is_dir {
                return Err(RenameError::IsDir);
            }
            if target_is_dir && !target_inode.directory_is_empty() {
                return Err(RenameError::NotEmpty);
            }
        }

        let source_inode = self.fs.get_inode(source.0);
        let update_dotdot = source_is_dir
            && !same_parent
            && source_inode.lookup_dir_entry("..").is_some();
        if update_dotdot
            && source_inode
                .replace_dir_entry("..", new_parent_id, 2)
                .is_none()
        {
            return Err(RenameError::Io);
        }

        if let Some(_) = target {
            if new_parent_inode
                .replace_dir_entry(new_name, source.0, source.1)
                .is_none()
            {
                if update_dotdot {
                    source_inode.replace_dir_entry("..", self.inode_id, 2);
                }
                return Err(RenameError::Io);
            }
        } else if !new_parent_inode.add_dir_entry(new_name, source.0, source.1) {
            if update_dotdot {
                source_inode.replace_dir_entry("..", self.inode_id, 2);
            }
            return Err(RenameError::Io);
        }

        if self.remove_dir_entry_only(old_name).is_none() {
            if let Some((target_inode_id, target_file_type)) = target {
                new_parent_inode.replace_dir_entry(new_name, target_inode_id, target_file_type);
            } else {
                new_parent_inode.remove_dir_entry_only(new_name);
            }
            if update_dotdot {
                source_inode.replace_dir_entry("..", self.inode_id, 2);
            }
            return Err(RenameError::Io);
        }

        if source_is_dir && !same_parent {
            self.fs.adjust_link_count(self.inode_id, -1);
            if target.is_none() {
                self.fs.adjust_link_count(new_parent_id, 1);
            }
        } else if source_is_dir && target.is_some() {
            // Replacing a directory in the same parent removes one child directory.
            self.fs.adjust_link_count(self.inode_id, -1);
        }

        if let Some((target_inode_id, _)) = target {
            let target_is_dir = self.fs.get_disk_inode(target_inode_id).is_dir();
            let mut links = self.fs.decrease_link_count(target_inode_id);
            if target_is_dir {
                links = self.fs.decrease_link_count(target_inode_id);
            }
            if links == 0 {
                // 与 delete_dir_entry 相同：强制实例化一次并立即丢弃，
                // 确保即使该 ino 从未被打开过也会触发 Drop 完成回收
                let _orphan = self.fs.get_inode(target_inode_id);
                info!("Renamed-over inode {} has zero links; data blocks and inode slot will be released when the last reference is dropped", target_inode_id);
            }
        }
        Ok(())
    }
    fn set_time(&self, atime: &TimeSpec, mtime: &TimeSpec) -> isize {
        block_modify_inode(&self.fs, self.inode_id, |disk_inode| {
            let old_atime = { disk_inode.i_atime };
            let old_mtime = { disk_inode.i_mtime };
            debug!("Ext4Inode::set_time: ino={}, old_atime={}, old_mtime={}, new_atime={}, new_mtime={}", 
                self.inode_id, old_atime, old_mtime, atime.tv_sec, mtime.tv_sec);
            unsafe {
                core::ptr::addr_of_mut!(disk_inode.i_atime).write_unaligned(atime.tv_sec as u32);
                core::ptr::addr_of_mut!(disk_inode.i_mtime).write_unaligned(mtime.tv_sec as u32);
            }
        });
        0
    }
    fn type_name(&self) -> &'static str {
        "Ext4Inode"
    }
    fn statfs(&self) -> Statfs {
        // 拿到你定义的真实的超级块
        let sb = &self.fs.superblock; 
        
        Statfs {
            f_type: 0xEF53, // Ext4 的标准魔数
            f_bsize: sb.block_size as u64, // 动态获取块大小
            f_blocks: sb.total_blocks as u64, // 动态获取总块数
            
            // 临时设置为总块数的一半，实际应根据文件系统的使用情况计算
            f_bfree: (sb.total_blocks / 2) as u64, 
            f_bavail: (sb.total_blocks / 2) as u64,
            
            f_files: sb.total_inodes as u64, // 动态获取总 Inode 数
            f_ffree: (sb.total_inodes / 2) as u64, // 同理，暂时给一半
            
            f_fsid: [0, 0], 
            f_namelen: 255, 
            f_frsize: sb.block_size as u64,
            f_flags: 0,
            f_spare: [0; 4],
        }
    }
}
