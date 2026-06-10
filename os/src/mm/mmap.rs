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

use crate::fs::{File, VfsInode};

use core::sync::atomic::{AtomicUsize, Ordering};
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
    pub static ref SHARED_PAGE_CACHE_MANAGER: Mutex<SharedPageCacheManager> = Mutex::new(SharedPageCacheManager {
        page_cache_map: BTreeMap::new(),
        file_register: BTreeMap::new(),
        vfs_register: BTreeMap::new(),
        lru_queue: Vec::new(),
    });
}

/// 共享映射页缓存管理器
pub struct SharedPageCacheManager {
    // (ino, page_offset) -> Arc<Mutex<PageCache>>
    page_cache_map: BTreeMap<(u64, usize), Arc<Mutex<PageCache>>>,
    // ino -> Weak<File>，mmap 回写用
    file_register: BTreeMap<u64, Weak<dyn File + Send + Sync>>,
    // ino -> Weak<VfsInode>，普通文件 I/O 回写用
    vfs_register: BTreeMap<u64, Weak<dyn VfsInode>>,
    // LRU 队列，(ino, page_offset)
    lru_queue: Vec<(u64, usize)>,
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
    fn lru_update(&mut self, ino: u64, page_offset: usize) {
        // 存在就移到末尾，不存在就 push
        self.lru_queue.retain(|&(i, o)| i != ino || o != page_offset);
        self.lru_queue.push((ino, page_offset));
    }

    fn take_lru_pages(&mut self, count: usize) -> Vec<(u64, usize)> {
        let mut result = Vec::new();
        while result.len() < count && !self.lru_queue.is_empty() {
            let key = self.lru_queue.pop().unwrap();
            // 缓存已经不存在的直接跳过
            if self.page_cache_map.contains_key(&key) {
                result.push(key);
            }
        }
        result
    }

    /// 获取页缓存条目。返回 (Arc<Mutex<PageCache>>, 是否新分配)
    /// 新分配的页内容为零，调用者负责从磁盘填充
    pub fn get_page_cache(&mut self, ino: u64, page_offset: usize) -> (Arc<Mutex<PageCache>>, bool) {
        let key = (ino, page_offset);
        let result = if let Some(cache) = self.page_cache_map.get(&key) {
            (cache.clone(), false)
        } else {
            let frame = frame_alloc(super::PageSize::Page4K).unwrap();
            let page = Arc::new(Mutex::new(PageCache::new(frame)));
            self.page_cache_map.insert(key, Arc::clone(&page));
            (page, true)
        };
        self.lru_update(ino, page_offset);
        result
    }

    /// 提取页缓存数据用于写回
    fn extract_for_writeback(&mut self, ino: u64, page_offset: usize) -> Option<UserBuffer> {
        let key = (ino, page_offset);
        if let Some(cache) = self.page_cache_map.get(&key) {
            let page = cache.lock();
            if !page.dirty {
                return None; // 非脏页无需写回
            }
            let buf = page.frame.get_bytes_array();
            Some(UserBuffer::new(alloc::vec![buf]))
        } else {
            None
        }
    }

    /// 清除脏标记（持有锁时调用）
    fn clear_dirty(&mut self, ino: u64, page_offset: usize) {
        if let Some(cache) = self.page_cache_map.get(&(ino, page_offset)) {
            cache.lock().dirty = false;
        }
    }

    /// 注册文件以便回写时找到（mmap 路径）
    pub fn register_file(&mut self, ino: u64, file: &Arc<dyn File + Send + Sync>) {
        self.file_register.insert(ino, Arc::downgrade(file));
    }

    /// 注册 VfsInode 以便回写时找到（普通文件 I/O 路径）
    pub fn register_vfs_inode(&mut self, ino: u64, vfs: &Arc<dyn VfsInode>) {
        self.vfs_register.insert(ino, Arc::downgrade(vfs));
    }

    /// 注销文件，同时释放该文件对应的所有未被引用的缓存页
    pub fn unregister_file(&mut self, ino: u64) {
        self.file_register.remove(&ino);
        self.vfs_register.remove(&ino);
        // 释放掉该文件对应的所有缓存页（仅当物理帧无外部引用时释放）
        self.page_cache_map.retain(|(key_ino, _), cache| {
            if *key_ino != ino {
                return true; // 保留其他文件的缓存
            }
            // 检查物理帧引用计数：仅当只有 PageCache 自己持有时才能释放
            // maparea持有一份，unmap时自动drop减少引用计数
            // 另外访问缓存时应该手动克隆一次frame增加引用计数
            let page = cache.lock();
            super::frame_ref_count(page.frame.ppn) > 1
        });
    }

    /// 清理已关闭文件的注册
    pub fn clear_closed_files(&mut self) {
        self.file_register.retain(|_, weak_file| weak_file.upgrade().is_some());
        self.vfs_register.retain(|_, weak_vfs| weak_vfs.upgrade().is_some());
    }

    /// 收集需要写回文件（mmaped）的脏页信息
    pub fn collect_dirty_pages(&mut self) -> Vec<(u64, usize, Arc<dyn File + Send + Sync>)> {
        let mut result = Vec::new();
        for weak_file in self.file_register.values() {
            if let Some(file) = weak_file.upgrade() {
                let ino = file.ino();
                let start = (ino, 0);
                let end = (ino + 1, 0);
                for ((_, page_offset), cache) in self.page_cache_map.range(start..end) {
                    let page = cache.lock();
                    if page.dirty {
                        result.push((ino, *page_offset, file.clone()));
                    }
                }
            }
        }
        result
    }

    /// 收集所有需要写回 vfsinode 的脏页信息
    pub fn collect_dirty_pages_vfs(&mut self) -> Vec<(u64, usize, Arc<dyn VfsInode>)> {
        let mut result = Vec::new();
        for weak_vfs in self.vfs_register.values() {
            if let Some(vfs) = weak_vfs.upgrade() {
                let ino = vfs.ino();
                let start = (ino, 0);
                let end = (ino + 1, 0);
                for ((_, page_offset), cache) in self.page_cache_map.range(start..end) {
                    let page = cache.lock();
                    if page.dirty {
                        result.push((ino, *page_offset, vfs.clone()));
                    }
                }
            }
        }
        result
    }

    /// 将共享页缓存写回 mmap 映射的文件
    pub fn write_back_page_cache(ino: u64, page_offset: usize, file: &Arc<dyn File + Send + Sync>) {        // 锁内提取数据
        let buffer_data = {
            let mut man = SHARED_PAGE_CACHE_MANAGER.lock();
            man.extract_for_writeback(ino, page_offset)
        };

        // 锁外写回文件
        if let Some(buffer) = buffer_data {
            file.raw_write_at(page_offset * crate::PAGE_SIZE, buffer);
            // 写回完成后清除 dirty 标记
            SHARED_PAGE_CACHE_MANAGER.lock().clear_dirty(ino, page_offset);
        }
    }

    /// 将页缓存写回 vfsinode 文件
    pub fn write_back_page_cache_vfs(ino: u64, page_offset: usize, vfs: &Arc<dyn VfsInode>) {
        // warn!("Writing back page cache for ino {}, page_offset {}", ino, page_offset);

        // 锁内提取数据
        let buffer_data = {
            let mut man = SHARED_PAGE_CACHE_MANAGER.lock();
            man.extract_for_writeback(ino, page_offset)
        };

        // 锁外写回文件
        if let Some(buffer) = buffer_data {
            for slice in buffer.buffers.iter() {
                vfs.raw_write_at(page_offset * crate::PAGE_SIZE, slice);
            }
            // 写回完成后清除 dirty 标记
            SHARED_PAGE_CACHE_MANAGER.lock().clear_dirty(ino, page_offset);
        }
    }
}

/// 用于sync系统调用，将缓存内容写回文件
pub fn sync_shared_page_cache() {
    // 先清理已关闭文件，再收集脏页（同时收集 mmap 和普通 I/O 的脏页）
    let (pending_file, pending_vfs) = {
        let mut man = SHARED_PAGE_CACHE_MANAGER.lock();
        man.clear_closed_files();
        let file_pages = man.collect_dirty_pages();
        let vfs_pages = man.collect_dirty_pages_vfs();
        (file_pages, vfs_pages)
    }; // 锁在此释放

    // 逐页写回（mmaped File）
    for (ino, page_offset, file) in pending_file {
        SharedPageCacheManager::write_back_page_cache(ino, page_offset, &file);
    }
    // 逐页写回（vfs）
    for (ino, page_offset, vfs) in pending_vfs {
        SharedPageCacheManager::write_back_page_cache_vfs(ino, page_offset, &vfs);
    }
    
    // 更新最后一次页回写的时间
    LAST_PAGE_SYNC_TIME.store(crate::arch::timer::get_time_ms(), Ordering::Release);
}

/// 周期性回写间隔，ms
const PAGE_SYNC_INTERVAL_MS: usize = 5000;
const BLOCK_SYNC_INTERVAL_MS: usize = 50000;

/// 最后一次触发自动页回写的时间
static LAST_PAGE_SYNC_TIME: AtomicUsize = AtomicUsize::new(0);
/// 最后一次触发自动块回写的时间
static LAST_BLOCK_SYNC_TIME: AtomicUsize = AtomicUsize::new(0);

// 计时器中断后触发
pub fn tick_sync() {
    let now = crate::arch::timer::get_time_ms();
    let last_page = LAST_PAGE_SYNC_TIME.load(Ordering::Relaxed);
    let last_block = LAST_BLOCK_SYNC_TIME.load(Ordering::Relaxed);
    if now.wrapping_sub(last_page) >= PAGE_SYNC_INTERVAL_MS {
        warn!("Auto page sync triggered by timer interrupt");
        if LAST_PAGE_SYNC_TIME.compare_exchange(last_page, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            sync_shared_page_cache();
            warn!("Auto sync completed");
        }
    }
    if now.wrapping_sub(last_block) >= BLOCK_SYNC_INTERVAL_MS {
        warn!("Auto block sync triggered by timer interrupt");
        if LAST_BLOCK_SYNC_TIME.compare_exchange(last_block, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            crate::drivers::block::block_cache::block_cache_sync_all();
            warn!("Auto block sync completed");
        }
    }
}

/// 强制回收 LRU 缓存页以腾出物理内存。脏页先写回再回收。
/// 返回实际回收的物理帧数，调用者可将其加入空闲帧池。
pub fn free_up_mem_space(std_pages: usize) -> usize {
    // 收集待回收页的信息
    let to_evict: Vec<(u64, usize, bool, Option<Arc<dyn VfsInode>>, Option<Arc<dyn File + Send + Sync>>)> = {
        let mut man = SHARED_PAGE_CACHE_MANAGER.lock();
        let pending = man.take_lru_pages(std_pages);
        let mut result = Vec::new();
        for (ino, page_offset) in pending {
            let has_dirty = man.page_cache_map
                .get(&(ino, page_offset))
                .map(|c| c.lock().dirty)
                .unwrap_or(false);
            let vfs = man.vfs_register.get(&ino).and_then(|w| w.upgrade());
            let file = man.file_register.get(&ino).and_then(|w| w.upgrade());
            result.push((ino, page_offset, has_dirty, vfs, file));
        }
        result
    };

    // 锁外写回脏页
    for (ino, page_offset, has_dirty, vfs, file) in &to_evict {
        if *has_dirty {
            if let Some(v) = vfs {
                SharedPageCacheManager::write_back_page_cache_vfs(*ino, *page_offset, v);
            } else if let Some(f) = file {
                SharedPageCacheManager::write_back_page_cache(*ino, *page_offset, f);
            }
        }
    }

    // 重新加回没被回收的页
    let mut unreleased_frames: Vec<(u64, usize)> = Vec::new();

    let mut freed_frames = 0;
    // 移除缓存条目，回收物理帧（仅 frame 引用计数为 1 且未被再次写脏的页可回收）
    let mut man = SHARED_PAGE_CACHE_MANAGER.lock();
    for (ino, page_offset, _, _, _) in &to_evict {
        let can_free = man.page_cache_map
            .get(&(*ino, *page_offset))
            .map(|c| {
                let page = c.lock();
                super::frame_ref_count(page.frame.ppn) == 1 && !page.dirty
            })
            .unwrap_or(false);
        if can_free {
            man.page_cache_map.remove(&(*ino, *page_offset));
            freed_frames += 1;
        } else {
            unreleased_frames.push((*ino, *page_offset));
        }
    }

    // 将未被回收的页重新加入 LRU 队列
    for (ino, page_offset) in unreleased_frames {
        man.lru_update(ino, page_offset);
    }
    freed_frames
}