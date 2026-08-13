use alloc::sync::{Arc, Weak};
use spin::Mutex;
use alloc::vec::Vec;

use crate::mm::{frame_alloc, FrameTracker, PageSize};
use crate::auth::{PermStat, FileMode};
use super::VfsInode;
use super::ino::get_next_ino;

// 内存文件，后续用户可以mmap到用户空间
pub struct MemFdInode {
    ino: u64,
    /// 物理页帧列表，需保证顺序
    phys_pages: Mutex<Vec<FrameTracker>>,
    /// 每个物理页的大小
    page_size: PageSize,
    /// 当前文件的逻辑大小
    file_size: Mutex<usize>,
    perms: Mutex<PermStat>,
}

impl MemFdInode {
    pub fn new(page_size: PageSize) -> Self {
        Self {
            ino: get_next_ino(),
            phys_pages: Mutex::new(Vec::new()),
            page_size,
            file_size: Mutex::new(0),
            perms: Mutex::new(PermStat::new(FileMode::from_bits_truncate(0o100777), 0, 0)),
        }
    }
    /// 获取页大小
    pub fn page_size(&self) -> PageSize {
        self.page_size
    }
    /// 获取物理页列表（用于 mmap 建立映射）
    pub fn get_phys_pages(&self) -> Vec<FrameTracker> {
        self.phys_pages.lock().iter().map(|f| f.clone()).collect()
    }
    /// 确保文件至少有 new_size 字节，必要时分配物理页
    fn ensure_size(&self, new_size: usize) {
        let ps = self.page_size.size();
        let needed_pages = (new_size + ps - 1) / ps;
        let mut phys_pages = self.phys_pages.lock();

        while phys_pages.len() < needed_pages {
            let frame = frame_alloc(self.page_size)
                .expect("MemFd: failed to allocate physical page");
            phys_pages.push(frame);
        }

        let mut file_size = self.file_size.lock();
        if new_size > *file_size {
            *file_size = new_size;
        }
    }
    fn do_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let file_size = *self.file_size.lock();
        if offset >= file_size || buf.is_empty() {
            return 0;
        }
        // 获取物理页
        let ps = self.page_size.size();
        let phys_pages = self.phys_pages.lock();
        // 计算读取长度，取文件剩余大小和缓冲区大小的最小值
        let max_read = core::cmp::min(buf.len(), file_size - offset);
        let mut remaining = max_read;
        let mut buf_offset = 0;
        // 拷贝数据
        while remaining > 0 {
            let page_idx = (offset + buf_offset) / ps;
            let page_offset = (offset + buf_offset) % ps;

            if page_idx >= phys_pages.len() {
                panic!("MemFd: page index out of bounds during read");
            }

            let page_bytes = phys_pages[page_idx].ppn.get_bytes_array_with_size(self.page_size);
            let copy_len = core::cmp::min(remaining, ps - page_offset);
            buf[buf_offset..buf_offset + copy_len].copy_from_slice(
                &page_bytes[page_offset..page_offset + copy_len]
            );

            remaining -= copy_len;
            buf_offset += copy_len;
        }

        max_read
    }
    fn do_write_at(&self, offset: usize, buf: &[u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        // 确保文件大小足够
        self.ensure_size(offset + buf.len());
        // 获取物理页
        let ps = self.page_size.size();
        let phys_pages = self.phys_pages.lock();
        // 初始化
        let mut remaining = buf.len();
        let mut buf_offset = 0;
        // 拷贝数据
        while remaining > 0 {
            let page_idx = (offset + buf_offset) / ps;
            let page_offset = (offset + buf_offset) % ps;

            let page_bytes = phys_pages[page_idx].ppn.get_bytes_array_with_size(self.page_size);
            let copy_len = core::cmp::min(remaining, ps - page_offset);
            page_bytes[page_offset..page_offset + copy_len]
                .copy_from_slice(&buf[buf_offset..buf_offset + copy_len]);

            remaining -= copy_len;
            buf_offset += copy_len;
        }
        buf.len()
    }
}

impl super::VfsInode for MemFdInode {
    fn raw_read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.do_read_at(offset, buf)
    }

    fn raw_write_at(&self, offset: usize, buf: &[u8]) -> usize {
        self.do_write_at(offset, buf)
    }

    fn get_size(&self) -> usize {
        *self.file_size.lock()
    }

    fn truncate(&self, len: usize) -> bool {
        let ps = self.page_size.size();
        let needed_pages = (len + ps - 1) / ps;
        let mut phys_pages = self.phys_pages.lock();
        let mut file_size = self.file_size.lock();

        let old_size = *file_size;
        *file_size = len;

        if len < old_size {
            // 收缩：释放超出部分的物理页
            phys_pages.truncate(needed_pages);
        }
        // 扩张：惰性分配，不预分配物理页（后续 write 时会通过 ensure_size 分配）
        true
    }

    fn ino(&self) -> u64 { self.ino }

    fn get_perm(&self) -> PermStat {
        self.perms.lock().clone()
    }

    fn set_perm(&self, perm: PermStat) -> bool {
        *self.perms.lock() = perm;
        true
    }

    fn get_stat(&self) -> super::Stat {
        let perms = self.get_perm();
        let size = self.get_size();
        super::Stat {
            dev: 0,
            ino: self.ino,
            mode: perms.mode.bits() as u32,
            nlink: 1,
            uid: perms.uid,
            gid: perms.gid,
            rdev: 0,
            __pad: 0,
            size: size as i64,
            blksize: self.page_size.size() as i32,
            __pad2: 0,
            blocks: ((size as i64) + 511) / 512,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2],
        }
    }

    fn get_statx(&self) -> super::Statx {
        let perms = self.get_perm();
        let size = self.get_size();
        super::Statx {
            stx_mask: 0,
            stx_blksize: self.page_size.size() as u32,
            stx_attributes: 0,
            stx_nlink: 1,
            stx_uid: perms.uid,
            stx_gid: perms.gid,
            stx_mode: perms.mode.bits(),
            __spare0: [0; 1],
            stx_ino: self.ino as u64,
            stx_size: size as u64,
            stx_blocks: ((size as u64) + 511) / 512,
            stx_attributes_mask: 0,
            ..Default::default()
        }
    }

    // memfd 是匿名文件，不支持目录操作
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

use super::{Dentry, OSInode};

/// Create an anonymous memfd and return its file description.
///
/// A memfd name is descriptive only. Publishing it below a shared directory
/// would turn equal names into the same file and incorrectly make anonymous
/// files visible during directory enumeration.
pub fn create_memfd(name: &str, page_size: PageSize) -> Arc<OSInode> {
    let readable = true;
    let writable = true;
    let vfs_inode: Arc<dyn VfsInode> = Arc::new(MemFdInode::new(page_size));
    let dentry = Dentry::new(name.into(), vfs_inode, Weak::new());
    Arc::new(OSInode::new(readable, writable, false, dentry))
}
