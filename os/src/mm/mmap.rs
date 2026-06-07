//! mmap syscall
//! 还包含页缓存管理器

#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use crate::{mm::{FrameTracker, MapArea, PhysPageNum, UserBuffer, frame_alloc}, task::processor::*};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use crate::fs::File;

use spin::Mutex;


// mmap 权限标志
bitflags! {
    pub struct MMapProt: i32 {
        const PROT_NONE  = 0;
        const PROT_READ  = 1 << 0;
        const PROT_WRITE = 1 << 1;
        const PROT_EXEC  = 1 << 2;
    }
}

// mmap 映射类型标志
bitflags! {
    pub struct MMapFlags: i32 {
        const MAP_FILE           = 0;
        const MAP_SHARED         = 0x01;
        const MAP_PRIVATE        = 0x02;
        const MAP_FIXED          = 0x10;
        const MAP_ANONYMOUS      = 0x20;
        // 下面的标志待完善
        const MAP_GROWSDOWN      = 0x0100;
        const MAP_DENYWRITE      = 0x0800;
        const MAP_EXECUTABLE     = 0x1000;
        const MAP_LOCKED         = 0x2000;
        const MAP_NORESERVE      = 0x4000;
        const MAP_POPULATE       = 0x8000;
        const MAP_NONBLOCK       = 0x10000;
        const MAP_STACK          = 0x20000;
        const MAP_HUGETLB        = 0x40000;
        const MAP_SYNC           = 0x80000;
        const MAP_FIXED_NOREPLACE = 0x100000;
    }
}

/// MAP_SHARED_VALIDATE 标志值。等同于 MAP_SHARED|MAP_PRIVATE，
/// 语义为带校验的 MAP_SHARED：设置后内核会验证所有 flag 位是否已知，
/// 存在未知位导致返回 EOPNOTSUPP。
pub const MAP_SHARED_VALIDATE: i32 = 0x03;

/// 修改断点
pub fn do_brk(addr: usize) -> Result<usize, i32> {
    let task = current_processor().current().unwrap();
    let proc = task.process();
    proc.change_program_brk(addr)
}

/// 内存映射逻辑
/// 要求调用者已经完成了参数检查
pub fn do_mmap(
    addr: usize, 
    length: usize, 
    prot: MMapProt, 
    flags: MMapFlags,
    file_inner: Option<Arc<dyn File + Send + Sync>>,
    offset: usize,                     
) -> Result<usize, isize> {
    let task = current_processor().current().unwrap();
    let proc = task.process();
    // 继续向下转发
    proc.mmap(addr, length, prot, flags, file_inner, offset) 
}

/// 要求调用者已经完成了参数检查
pub fn do_munmap(addr: usize, length: usize) -> Result<(), isize> {
    let task = current_processor().current().unwrap();
    let proc = task.process();
    proc.munmap(addr, length)
}

// shared映射需要page cache

use lazy_static::lazy_static;

lazy_static! {
    /// 共享映射页缓存管理器
    pub static ref SHARED_PAGE_CACHE_MANAGER: SharedPageCacheManager = SharedPageCacheManager {
        page_cache_map: Mutex::new(BTreeMap::new()),
        file_register: Mutex::new(BTreeMap::new()),
    };
}

/// 共享映射页缓存管理器
pub struct SharedPageCacheManager {
    // (ino, page_offset) -> Arc<Mutex<PageCache>>
    page_cache_map: Mutex<BTreeMap<(u64, usize), Arc<Mutex<PageCache>>>>,
    // ino -> Weak<File>，用于回写找到文件
    file_register: Mutex<BTreeMap<u64, Weak<dyn File + Send + Sync>>>,
}

/// 单个页缓存条目
pub struct PageCache {
    pub frame: FrameTracker,
    pub dirty: bool,
}

impl PageCache {
    pub fn new(frame: FrameTracker) -> Self {
        Self { frame, dirty: false }
    }
}

impl SharedPageCacheManager {
    /// 获取页缓存条目。返回 (Arc<Mutex<PageCache>>, 是否新分配)。
    /// 新分配的页内容为零，调用者负责从磁盘填充。
    pub fn get_page_cache(&self, ino: u64, page_offset: usize) -> (Arc<Mutex<PageCache>>, bool) {
        let mut map = self.page_cache_map.lock();
        let key = (ino, page_offset);
        if let Some(cache) = map.get(&key) {
            (Arc::clone(cache), false)
        } else {
            let frame = frame_alloc(super::PageSize::Page4K).unwrap();
            let page = Arc::new(Mutex::new(PageCache::new(frame)));
            map.insert(key, Arc::clone(&page));
            (page, true)
        }
    }

    /// 标记缓存页为脏。内核 write_at 写入后调用。
    pub fn mark_dirty(&self, ino: u64, page_offset: usize) {
        let map = self.page_cache_map.lock();
        if let Some(cache) = map.get(&(ino, page_offset)) {
            cache.lock().dirty = true;
        }
    }

    /// 将共享页缓存写回文件（仅当脏时）。在调用前需要保证释放掉所有cache的锁以避免死锁。
    pub fn write_back_page_cache(&self, ino: u64, page_offset: usize, file: &Arc<dyn File + Send + Sync>) {
        // 锁内判断是否需要写回，并拷贝出数据
        let buffer_data: Option<UserBuffer> = {
            let map = self.page_cache_map.lock();
            let key = (ino, page_offset);
            if let Some(cache) = map.get(&key) {
                let page = cache.lock();
                if page.dirty {
                    let buf = page.frame.get_bytes_array();
                    Some(UserBuffer::new(alloc::vec![buf]))
                } else {
                    None
                }
            } else {
                None
            }
        };

        // 锁外写回文件（用 raw_write_at 绕过页缓存，避免重新进入 page cache）
        if let Some(buffer) = buffer_data {
            file.raw_write_at(page_offset * crate::PAGE_SIZE, buffer);
            // 写回完成后清除 dirty 标记
            if let Some(cache) = self.page_cache_map.lock().get(&(ino, page_offset)) {
                cache.lock().dirty = false;
            }
        }
    }

    // 注册文件以便回写时找到
    pub fn register_file(&self, ino: u64, file: &Arc<dyn File + Send + Sync>) {
        let mut reg = self.file_register.lock();
        reg.insert(ino, Arc::downgrade(file));
    }

    // 注销文件，同时释放该文件对应的所有未被引用的缓存页
    pub fn unregister_file(&self, ino: u64) {
        let mut reg = self.file_register.lock();
        reg.remove(&ino);
        // 释放掉该文件对应的所有缓存页（仅当物理帧无外部引用时释放）
        let mut map = self.page_cache_map.lock();
        map.retain(|(key_ino, _), cache| {
            if *key_ino != ino {
                return true; // 保留其他文件的缓存
            }
            // 检查物理帧引用计数：仅当只有 PageCache 自己持有时才能释放
            // maparea持有一份，unmap时自动drop减少引用计数
            // 另外访问缓存时应该手动克隆一次frame增加引用计数
            let page = cache.lock();
            super::frame_ref_count(page.frame.ppn) == 1
        });
    }

    /// 清理已关闭文件的注册
    pub fn clear_closed_files(&self) {
        let mut register = self.file_register.lock();
        register.retain(|_, weak_file| weak_file.upgrade().is_some());
    }

    /// 同步共享页缓存，将所有脏页写回对应文件
    pub fn sync_shared_page_cache(&self) {
        // 收集所有需要写回的 (ino, page_offset) 以及对应的 File
        let pending: Vec<(u64, usize, Arc<dyn File + Send + Sync>)> = {
            let map = self.page_cache_map.lock();
            let reg = self.file_register.lock();
            let mut result = Vec::new();
            for weak_file in reg.values() {
                if let Some(file) = weak_file.upgrade() {
                    let ino = file.ino();
                    let start = (ino, 0);
                    let end = (ino + 1, 0);
                    for ((_, page_offset), cache) in map.range(start..end) {
                        let page = cache.lock();
                        if page.dirty {
                            result.push((ino, *page_offset, file.clone()));
                        }
                    }
                }
            }
            result
        }; // map 和 reg 的锁在这里释放

        // 逐页写回
        for (ino, page_offset, file) in pending {
            self.write_back_page_cache(ino, page_offset, &file);
        }
    }
}

/// 用于sync系统调用，将缓存内容写回文件
pub fn sync_shared_page_cache() {
    let man = &SHARED_PAGE_CACHE_MANAGER;
    man.clear_closed_files();
    man.sync_shared_page_cache();
}