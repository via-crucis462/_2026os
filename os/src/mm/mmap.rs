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
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize() as i32)?;
    let result = mm.exclusive_access().change_program_brk(addr);
    result
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
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize())?;
    let result = mm.exclusive_access().mmap(addr, length, prot, flags, file_inner, offset);
    result
}

/// 要求调用者已经完成了参数检查
pub fn do_munmap(addr: usize, length: usize) -> Result<(), isize> {
    let task = current_processor().current().unwrap();
    let mm = task
        .inner_exclusive_access()
        .mm
        .as_ref()
        .cloned()
        .ok_or(crate::syscall::errno::Errno::EINVAL.as_isize())?;
    let result = mm.exclusive_access().munmap(addr, length);
    result
}

// shared映射需要page cache

use lazy_static::lazy_static;

lazy_static! {
    /// 共享映射页缓存管理器（四把独立锁，锁序: page → file → vfs → lru）
    pub static ref SHARED_PAGE_CACHE_MANAGER:SharedPageCacheManager = SharedPageCacheManager {
        page_cache_map: Mutex::new(BTreeMap::new()),
        file_register: Mutex::new(BTreeMap::new()),
        vfs_register: Mutex::new(BTreeMap::new()),
        lru_queue: Mutex::new(PageCacheLRUQueue::new()),
    };
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

// 页缓存 lru 队列
struct PageCacheLRUQueue {
    // 存储存在的页缓存条目
    exists: BTreeMap<(u64, usize), ()>,
    // (ino, page_offset) → next (ino, page_offset)
    nexts: BTreeMap<(u64, usize), Option<(u64, usize)>>,
    // (ino, page_offset) → prev (ino, page_offset)
    prevs: BTreeMap<(u64, usize), Option<(u64, usize)>>,
    head: Option<(u64, usize)>,
    tail: Option<(u64, usize)>,
}

impl PageCacheLRUQueue{
    pub fn new() -> Self {
        Self {
            exists: BTreeMap::new(),
            nexts: BTreeMap::new(),
            prevs: BTreeMap::new(),
            head: None,
            tail: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }
    pub fn pop(&mut self) -> Option<(u64, usize)> {
        if let Some(head) = self.head {
            let next = self.nexts[&head];
            if let Some(n) = next {
                self.prevs.insert(n, None);
            } else {
                self.tail = None;
            }
            self.head = next;
            self.exists.remove(&head);
            Some(head)
        } else {
            None
        }
    }
    /// 将页缓存条目移动到队尾（最近使用）；若不存在则插入
    pub fn update(&mut self, ino: u64, page_offset: usize) {
        let key = (ino, page_offset);
        if !self.exists.contains_key(&key) {
            // 新条目：直接插入到队尾
            self.exists.insert(key, ());
            if let Some(t) = self.tail {
                self.nexts.insert(t, Some(key));
            } else {
                self.head = Some(key);
            }
            self.prevs.insert(key, self.tail);
            self.nexts.insert(key, None);
            self.tail = Some(key);
            return;
        }
        // 从当前位置断开
        let prev = self.prevs[&key];
        let next = self.nexts[&key];
        if let Some(p) = prev {
            self.nexts.insert(p, next);
        } else {
            self.head = next;
        }
        if let Some(n) = next {
            self.prevs.insert(n, prev);
        } else {
            self.tail = prev;
        }
        // 插入到队尾
        if let Some(t) = self.tail {
            self.nexts.insert(t, Some(key));
        } else {
            // 队列在断开后变空（单元素 update 自身），head 也需恢复
            self.head = Some(key);
        }
        self.prevs.insert(key, self.tail);
        self.nexts.insert(key, None);
        self.tail = Some(key);
    }
}

/// 共享映射页缓存管理器，LRU 回收
/// 
/// 需要临界区重叠时，约定锁序（防死锁）：
/// page_cache_map → file_register → vfs_register → lru_queue
pub struct SharedPageCacheManager {
    // (ino, page_offset) → page cache
    //
    // 注意：
    // 尽管语法上没有严格限制，但每一次访问 PageCache 数据，
    // 包括修改、写回磁盘和对 dirty 标志的修改，
    // 都应该在 MutexGuard 内进行，以保证数据一致性。
    page_cache_map: Mutex<BTreeMap<(u64, usize), Arc<Mutex<PageCache>>>>,
    // ino → File 弱引用（mmap）
    file_register: Mutex<BTreeMap<u64, Weak<dyn File + Send + Sync>>>,
    // ino → VfsInode 弱引用（io缓存）
    vfs_register: Mutex<BTreeMap<u64, Weak<dyn VfsInode>>>,
    // 记录访问顺序
    lru_queue: Mutex<PageCacheLRUQueue>,
}


impl SharedPageCacheManager {
    // 无锁辅助函数

    fn lru_update_inner(queue: &mut PageCacheLRUQueue, ino: u64, page_offset: usize) {
        queue.update(ino, page_offset);
    }

    // ---

    // 单锁方法

    /// 注册文件以便回写时找到（mmap）
    pub fn register_file(&self, ino: u64, file: &Arc<dyn File + Send + Sync>) {
        self.file_register.lock().insert(ino, Arc::downgrade(file));
    }

    /// 注册文件以便回写时找到（vfs）
    pub fn register_vfs_inode(&self, ino: u64, vfs: &Arc<dyn VfsInode>) {
        self.vfs_register.lock().insert(ino, Arc::downgrade(vfs));
    }

    // ---

    // 多锁叠加方法

    /// 获取页缓存条目
    /// 返回 (Arc<Mutex<PageCache>>, 是否新分配)
    pub fn get_page_cache(&self, ino: u64, page_offset: usize) -> (Arc<Mutex<PageCache>>, bool) {
        let mut map = self.page_cache_map.lock();
        let mut queue = self.lru_queue.lock(); // page → lru
        let key = (ino, page_offset);
        let result = if let Some(cache) = map.get(&key) {
            (cache.clone(), false)
        } else {
            let frame = frame_alloc(super::PageSize::Page4K).unwrap();
            let page = Arc::new(Mutex::new(PageCache::new(frame)));
            map.insert(key, Arc::clone(&page));
            (page, true)
        };
        Self::lru_update_inner(&mut queue, ino, page_offset);
        result
    }

    /// 注销文件，释放该文件对应的未被引用的缓存页
    pub fn unregister_file(&self, ino: u64) {
        // page → file → vfs
        let mut map = self.page_cache_map.lock();
        let mut file_reg = self.file_register.lock();
        let mut vfs_reg = self.vfs_register.lock();
        file_reg.remove(&ino);
        vfs_reg.remove(&ino);
        map.retain(|(key_ino, _), cache| {
            if *key_ino != ino { return true; }
            let page = cache.lock();
            super::frame_ref_count(page.frame.ppn) > 1
        });
    }

    /// 清理已关闭文件的注册
    pub fn clear_closed_files(&self) {
        // file → vfs
        let mut file_reg = self.file_register.lock();
        let mut vfs_reg = self.vfs_register.lock();
        file_reg.retain(|_, w| w.upgrade().is_some());
        vfs_reg.retain(|_, w| w.upgrade().is_some());
    }

    /// 收集需要写回的脏页（mmap）
    /// 需要 page + file 锁
    pub fn collect_dirty_pages(&self) -> Vec<(u64, usize, Arc<dyn File + Send + Sync>)> {
        let map = self.page_cache_map.lock();
        let file_reg = self.file_register.lock();
        let mut result = Vec::new();
        for weak_file in file_reg.values() {
            if let Some(file) = weak_file.upgrade() {
                let ino = file.ino();
                let start = (ino, 0);
                let end = (ino + 1, 0);
                for ((_, page_offset), cache) 
                in map.range(start..end) {
                    if cache.lock().dirty {
                        result.push((ino, *page_offset, file.clone()));
                    }
                }
            }
        }
        result
    }

    /// 收集需要写回的脏页（vfs）
    /// 需要 page + vfs 锁
    pub fn collect_dirty_pages_vfs(&self) -> Vec<(u64, usize, Arc<dyn VfsInode>)> {
        let map = self.page_cache_map.lock();
        let vfs_reg = self.vfs_register.lock();
        let mut result = Vec::new();
        for weak_vfs in vfs_reg.values() {
            if let Some(vfs) = weak_vfs.upgrade() {
                let ino = vfs.ino();
                let start = (ino, 0);
                let end = (ino + 1, 0);
                for ((_, page_offset), cache) in map.range(start..end) {
                    if cache.lock().dirty {
                        result.push((ino, *page_offset, vfs.clone()));
                    }
                }
            }
        }
        result
    }

    /// 将共享页缓存写回 mmap 映射的文件
    pub fn write_back_page_cache(&self, ino: u64, page_offset: usize, file: &Arc<dyn File + Send + Sync>) {
        let cache_arc = {
            self.page_cache_map.lock().get(&(ino, page_offset)).cloned()
        };
        if let Some(cache) = cache_arc {
            let mut page = cache.lock();
            if page.dirty {
                page.dirty = false;
                let buf = page.frame.get_bytes_array();
                file.raw_write_at(page_offset * crate::PAGE_SIZE, UserBuffer::new(alloc::vec![buf]));
            }
        }
    }

    /// 将页缓存写回 vfsinode 文件
    pub fn write_back_page_cache_vfs(&self, ino: u64, page_offset: usize, vfs: &Arc<dyn VfsInode>) {
        let cache_arc = {
            self.page_cache_map.lock().get(&(ino, page_offset)).cloned()
        };
        if let Some(cache) = cache_arc {
            let mut page = cache.lock();
            if page.dirty {
                page.dirty = false;
                let buf = page.frame.get_bytes_array();
                for slice in UserBuffer::new(alloc::vec![buf]).buffers.iter() {
                    vfs.raw_write_at(page_offset * crate::PAGE_SIZE, slice);
                }
            }
        }
    }
}

/// 用于sync系统调用，将缓存内容写回文件
pub fn sync_shared_page_cache() {
    let man = &SHARED_PAGE_CACHE_MANAGER;
    // 先清理已关闭文件，再收集脏页
    let (pending_file, pending_vfs) = {
        // page → file → vfs
        let map = man.page_cache_map.lock();
        let mut file_reg = man.file_register.lock();
        let mut vfs_reg = man.vfs_register.lock();
        // 清理
        file_reg.retain(|_, w| w.upgrade().is_some());
        vfs_reg.retain(|_, w| w.upgrade().is_some());
        // 收集 File 脏页
        let mut file_pages = Vec::new();
        for weak_file in file_reg.values() {
            if let Some(file) = weak_file.upgrade() {
                let ino = file.ino();
                for ((_, po), cache) in map.range((ino, 0)..(ino + 1, 0)) {
                    if cache.lock().dirty {
                        file_pages.push((ino, *po, file.clone()));
                    }
                }
            }
        }
        // 收集 Vfs 脏页
        let mut vfs_pages = Vec::new();
        for weak_vfs in vfs_reg.values() {
            if let Some(vfs) = weak_vfs.upgrade() {
                let ino = vfs.ino();
                for ((_, po), cache) in map.range((ino, 0)..(ino + 1, 0)) {
                    if cache.lock().dirty {
                        vfs_pages.push((ino, *po, vfs.clone()));
                    }
                }
            }
        }
        (file_pages, vfs_pages)
    }; // 所有锁释放

    for (ino, po, file) in pending_file {
        man.write_back_page_cache(ino, po, &file);
    }
    for (ino, po, vfs) in pending_vfs {
        man.write_back_page_cache_vfs(ino, po, &vfs);
    }
    LAST_PAGE_SYNC_TIME.store(crate::arch::timer::get_time_ms(), Ordering::Release);
}

/// 周期性回写间隔，ms
const PAGE_SYNC_INTERVAL_MS: usize = 5000;
const BLOCK_SYNC_INTERVAL_MS: usize = 50000;

/// 最后一次触发自动页回写的时间
static LAST_PAGE_SYNC_TIME: AtomicUsize = AtomicUsize::new(0);
/// 最后一次触发自动块回写的时间
static LAST_BLOCK_SYNC_TIME: AtomicUsize = AtomicUsize::new(0);

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
    let man = &SHARED_PAGE_CACHE_MANAGER;

    // 清理已关闭文件的注册，避免误保留页缓存
    man.clear_closed_files();

    // 从队列中获取收集待回收页
    let to_evict: Vec<(u64, usize, bool, Option<Arc<dyn VfsInode>>, Option<Arc<dyn File + Send + Sync>>)> = {
        let map = man.page_cache_map.lock();
        let file_reg = man.file_register.lock();
        let vfs_reg = man.vfs_register.lock();
        let mut queue = man.lru_queue.lock();

        // 收集的同时清除无效项目
        let mut pending = Vec::new();
        while pending.len() < std_pages && !queue.is_empty() {
            let key = queue.pop().unwrap();
            if map.contains_key(&key) {
                pending.push(key);
            }
        }

        let mut result = Vec::new();
        for (ino, page_offset) in pending {
            let has_dirty = map.get(&(ino, page_offset))
                .map(|c| c.lock().dirty)
                .unwrap_or(false);
            let vfs = vfs_reg.get(&ino).and_then(|w| w.upgrade());
            let file = file_reg.get(&ino).and_then(|w| w.upgrade());
            result.push((ino, page_offset, has_dirty, vfs, file));
        }
        result
    }; // 所有锁释放

    // 写回脏页
    for (ino, page_offset, has_dirty, vfs, file) in &to_evict {
        if *has_dirty {
            if let Some(v) = vfs {
                SHARED_PAGE_CACHE_MANAGER.write_back_page_cache_vfs(*ino, *page_offset, v);
            } else if let Some(f) = file {
                SHARED_PAGE_CACHE_MANAGER.write_back_page_cache(*ino, *page_offset, f);
            }
        }
    }

    // 检查条件并移除
    let mut unreleased = Vec::new();
    let mut freed = 0;
    {
        let mut map = man.page_cache_map.lock();
        let mut queue = man.lru_queue.lock(); // page → lru
        for (ino, page_offset, _, _, _) in &to_evict {
            let can_free = map.get(&(*ino, *page_offset))
                .map(|c| {
                    let arc_ref = Arc::strong_count(c);
                    let page = c.lock();
                    (   
                        // PageCache 仅被 map 持有
                        // 此处在map锁内，所以能保证不会被其他核再获取到
                        arc_ref == 1 && 
                        // 不被 mmap 映射
                        // PageCache 的引用计数已经为 1
                        // 因此 frame 也能保证引用数不变
                        super::frame_ref_count(page.frame.ppn) == 1 && 
                        // 上次回写后没有被别别处再次修改
                        !page.dirty
                    )
                })
                .unwrap_or(false);
            if can_free {
                map.remove(&(*ino, *page_offset));
                freed += 1;
            } else {
                unreleased.push((*ino, *page_offset));
            }
        }
        for (ino, po) in &unreleased {
            SharedPageCacheManager::lru_update_inner(&mut queue, *ino, *po);
        }
    }

    if freed < std_pages {
        println!("free_up_mem_space: only freed {} pages, requested {}", freed, std_pages);
        println!("remaining page num: {:?}", man.lru_queue.lock().exists.keys().collect::<Vec<_>>().len());
        println!("remaining page(in map) num: {:?}", man.page_cache_map.lock().len());

    }

    freed
}