//! 页缓存/块缓存管理
//! 
//! 当前实现合并了原本的 SharedPageCacheManager 和 BlockCacheManager 的功能
//! 合并的目的主要是相比旧实现减少一次数据拷贝，但可能不完善

use super::BlockDevice;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::{Mutex, MutexGuard};

use crate::ext4fs::BLOCK_SZ;
use crate::mm::{frame_alloc, FrameTracker, PageSize};


/// 缓存状态
/// 
/// 用于异步 IO 的状态同步，
/// 目前还没有真正使用
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheState {
    Loading,
    Clean,
    Dirty,
    Writeback,
    Error,
}

pub struct PageCacheInner {
    pub frame: FrameTracker,
    pub dirty: bool,
    pub state: CacheState,
    /// 缓存已作废：对应物理块已被释放，禁止再写回
    pub discarded: bool,
}

impl PageCacheInner {
    fn addr_of_offset(&self, offset: usize) -> usize {
        &self.frame.get_bytes_array()[offset] as *const u8 as usize
    }
    pub fn get_ref<T>(&self, offset: usize) -> &T
    where
        T: Sized,
    {
        assert!(offset + core::mem::size_of::<T>() <= BLOCK_SZ);
        unsafe { &*(self.addr_of_offset(offset) as *const T) }
    }
    pub fn get_mut<T>(&mut self, offset: usize) -> &mut T
    where
        T: Sized,
    {
        assert!(offset + core::mem::size_of::<T>() <= BLOCK_SZ);
        self.dirty = true;
        self.state = CacheState::Dirty;
        unsafe { &mut *(self.addr_of_offset(offset) as *mut T) }
    }
    pub fn read<T, V>(&self, offset: usize, f: impl FnOnce(&T) -> V) -> V {
        f(self.get_ref(offset))
    }
    pub fn modify<T, V>(&mut self, offset: usize, f: impl FnOnce(&mut T) -> V) -> V {
        f(self.get_mut(offset))
    }
}

/// 页缓存
/// 
/// 合并了原本的 SharedPageCache 和 BlockCache 的功能
/// 当前实现规定为 4KB
pub struct PageCache {
    inner: Mutex<PageCacheInner>,
    block_id: usize,
    block_device: Option<Arc<dyn BlockDevice>>,
}

impl PageCache {
    fn new_loading(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        Self {
            inner: Mutex::new(PageCacheInner {
                frame: frame_alloc(PageSize::Page4K).unwrap(),
                dirty: false,
                state: CacheState::Loading,
                discarded: false,
            }),
            block_id,
            block_device: Some(block_device),
        }
    }
    pub fn from_frame(frame: FrameTracker) -> Self {
        Self {
            inner: Mutex::new(PageCacheInner {
                frame,
                dirty: false,
                state: CacheState::Clean,
                discarded: false,
            }),
            block_id: 0,
            block_device: None,
        }
    }
    pub fn lock(&self) -> MutexGuard<'_, PageCacheInner> {
        self.inner.lock()
    }
    /// 将缓存同步到磁盘，如果脏则写回
    pub fn sync(&self) {
        let Some(block_device) = &self.block_device else {
            return;
        };
        let mut inner = self.inner.lock();
        if inner.discarded || !inner.dirty {
            return;
        }
        inner.state = CacheState::Writeback;
        block_device.raw_write_block(self.block_id, inner.frame.get_bytes_array());
        inner.dirty = false;
        inner.state = CacheState::Clean;
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        self.sync();
    }
}

struct PageCacheLruQueue {
    chain: BTreeMap<u64, (Option<u64>, Option<u64>)>,
    head: Option<u64>,
    tail: Option<u64>,
}

impl PageCacheLruQueue {
    fn new() -> Self {
        Self {
            chain: BTreeMap::new(),
            head: None,
            tail: None,
        }
    }
    fn len(&self) -> usize {
        self.chain.len()
    }
    fn pop(&mut self) -> Option<u64> {
        let head = self.head?;
        let (_, next) = self.chain.remove(&head).unwrap();
        self.head = next;
        if let Some(next) = next {
            self.chain.get_mut(&next).unwrap().0 = None;
        } else {
            self.tail = None;
        }
        Some(head)
    }

    fn remove(&mut self, block_id: u64) -> bool {
        let Some((prev, next)) = self.chain.remove(&block_id) else {
            return false;
        };
        if let Some(prev) = prev {
            self.chain.get_mut(&prev).unwrap().1 = next;
        } else {
            self.head = next;
        }
        if let Some(next) = next {
            self.chain.get_mut(&next).unwrap().0 = prev;
        } else {
            self.tail = prev;
        }
        true
    }

    /// 将指定物理块移到队尾
    fn update(&mut self, block_id: u64) {
        if self.tail == Some(block_id) {
            return;
        }

        self.remove(block_id);

        if let Some(tail) = self.tail {
            self.chain.get_mut(&tail).unwrap().1 = Some(block_id);
        } else {
            self.head = Some(block_id);
        }
        self.chain.insert(block_id, (self.tail, None));
        self.tail = Some(block_id);
    }
}

// 元数据缓存的最大数量，超过该数量时会尝试回收
const META_CACHE_SIZE: usize = 256;

/// 页缓存管理器
/// 
/// 管理所有文件系统数据块和元数据块的缓存。
/// 从原有 SharedPageCacheManager 修改而来，
/// 并且合并了原本 BlockCacheManager 的功能
/// 
pub struct PageCacheManager {
    /// (ino, logical_block_id) -> physical_block_id
    page_cache_id_map: Mutex<BTreeMap<(u64, usize), u64>>,
    /// physical_block_id -> cached
    page_cache_map: Mutex<BTreeMap<u64, Arc<PageCache>>>,

    // 旧实现中这里还有 vfsinode/file 注册表
    // 而现在所有页缓存通过块号管理
    // 不再需要回写的注册机制

    /// 数据块（非元数据块） LRU 队列
    data_lru_queue: Mutex<PageCacheLruQueue>,
    /// 元数据块 LRU 队列
    meta_lru_queue: Mutex<PageCacheLruQueue>,
}

impl PageCacheManager {
    fn new() -> Self {
        Self {
            page_cache_id_map: Mutex::new(BTreeMap::new()),
            page_cache_map: Mutex::new(BTreeMap::new()),
            data_lru_queue: Mutex::new(PageCacheLruQueue::new()),
            meta_lru_queue: Mutex::new(PageCacheLruQueue::new()),
        }
    }
    /// 获取指定物理块的缓存，如果不存在则创建新的缓存
    /// 
    /// 返回 (缓存, 是否新创建)
    fn get_physical_page(
        &self,
        block_id: u64,
        block_device: Arc<dyn BlockDevice>,
        is_data: bool,
        load_from_disk: bool,
    ) -> (Arc<PageCache>, bool) {
        let mut map = self.page_cache_map.lock();
        if let Some(cache) = map.get(&block_id).cloned() {
            drop(map);
            self.touch_lru(block_id, is_data);
            return (cache, false);
        }

        let cache = Arc::new(PageCache::new_loading(
            block_id as usize,
            block_device.clone(),
        ));
        let mut inner = cache.inner.lock();
        map.insert(block_id, cache.clone());
        // 新建 cache
        drop(map);
        self.touch_lru(block_id, is_data);
        
        if load_from_disk {
            block_device.raw_read_block(block_id as usize, inner.frame.get_bytes_array());
            inner.state = CacheState::Clean;
        } else {
            inner.frame.get_bytes_array().fill(0);
            inner.dirty = true;
            inner.state = CacheState::Dirty;
        }
        drop(inner);

        if !is_data {
            self.trim_meta_cache();
        }
        (cache, true)
    }
    /// 在 LRU 队列中标记一次访问
    fn touch_lru(&self, block_id: u64, is_data: bool) {
        // 固定锁序 data -> meta，并保证一个物理块只属于一种 LRU。
        let mut data_queue = self.data_lru_queue.lock();
        let mut meta_queue = self.meta_lru_queue.lock();
        if is_data {
            meta_queue.remove(block_id);
            data_queue.update(block_id);
        } else {
            data_queue.remove(block_id);
            meta_queue.update(block_id);
        }
    }
    /// 尝试回收元数据缓存
    fn trim_meta_cache(&self) {
        let attempts = self.meta_lru_queue.lock().len();
        for _ in 0..attempts {
            if self.meta_lru_queue.lock().len() <= META_CACHE_SIZE {
                break;
            }
            let Some(block_id) = self.meta_lru_queue.lock().pop() else {
                break;
            };
            if !self.try_evict(block_id) {
                self.meta_lru_queue.lock().update(block_id);
            }
        }
    }
    /// 尝试回收指定物理块的缓存，返回是否成功回收
    fn try_evict(&self, block_id: u64) -> bool {
        let cache = {
            let map = self.page_cache_map.lock();
            let Some(cache) = map.get(&block_id) else {
                return true;
            };
            if Arc::strong_count(cache) != 1 {
                return false;
            }
            cache.clone()
        };

        cache.sync();
        let count = Arc::strong_count(&cache.lock().frame);
        if count != 1 {
            return false;
        }

        let mut map = self.page_cache_map.lock();
        if map
            .get(&block_id)
            .map(|entry| Arc::ptr_eq(entry, &cache) && Arc::strong_count(entry) == 2)
            .unwrap_or(false)
        {
            map.remove(&block_id);
            true
        } else {
            false
        }
    }
    /// 作废并丢弃指定物理块的缓存（不回写）。
    /// 
    /// 用于物理块被释放（truncate/dealloc）的场景：
    /// 块释放后旧缓存既不能继续回写（可能污染重新分配后的新所有者），
    /// 也不能继续作为该物理块的缓存被复用（会读到已释放文件的旧数据）。
    /// 与 get_physical_page 保持一致：先锁 map 再锁 inner。
    pub fn invalidate_block(&self, block_id: u64) {
        let mut map = self.page_cache_map.lock();
        if let Some(cache) = map.get(&block_id).cloned() {
            cache.lock().discarded = true;
        }
        map.remove(&block_id);
        drop(map);

        // 从 LRU 队列移除，避免之后被当作存活缓存弹出
        self.data_lru_queue.lock().remove(block_id);
        self.meta_lru_queue.lock().remove(block_id);
        // 清理指向该物理块的 (ino, logical_block) 映射
        self.page_cache_id_map
            .lock()
            .retain(|_, v| *v != block_id);
    }
    /// 封装原本 BlockCacheManager 的功能，获取元数据块缓存
    pub fn get_block_cache(
        &self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<PageCache> {
        self.get_physical_page(block_id as u64, block_device, false, true)
            .0
    }
    /// 获取不需要 ino/logical_block 映射的普通文件数据块缓存。
    pub fn get_data_block_cache(
        &self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<PageCache> {
        self.get_physical_page(block_id as u64, block_device, true, true)
            .0
    }
    /// 封装原本 SharedPageCacheManager 的功能，获取数据块缓存
    pub fn get_page_cache(
        &self,
        ino: u64,
        logical_block: usize,
        physical_block: u64,
        block_device: Arc<dyn BlockDevice>,
    ) -> (Arc<PageCache>, bool) {
        self.page_cache_id_map
            .lock()
            .insert((ino, logical_block), physical_block);
        self.get_physical_page(physical_block, block_device, true, true)
    }
    /// 获取一个空白页缓存
    pub fn get_new_page_cache(
        &self,
        ino: u64,
        logical_block: usize,
        physical_block: u64,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<PageCache> {
        self.page_cache_id_map
            .lock()
            .insert((ino, logical_block), physical_block);
        let cache = self
            .get_physical_page(physical_block, block_device, true, false)
            .0;
        {
            let mut inner = cache.lock();
            inner.frame.get_bytes_array().fill(0);
            inner.dirty = true;
            inner.state = CacheState::Dirty;
        }
        cache
    }

    /// 按文件逻辑块查询已经存在的缓存页，不创建缓存，也不访问文件系统 extent。
    pub fn get_cached_file_page(
        &self,
        ino: u64,
        logical_block: usize,
    ) -> Option<Arc<PageCache>> {
        let block_id = *self.page_cache_id_map.lock().get(&(ino, logical_block))?;
        self.page_cache_map.lock().get(&block_id).cloned()
    }
    pub fn write_back_page_cache(
        &self,
        ino: u64,
        page_offset: usize,
    ) {
        if let Some(cache) = self.get_cached_file_page(ino, page_offset) {
            cache.sync();
        }
    }
    pub fn sync_all(&self) {
        let caches: Vec<_> = self.page_cache_map.lock().values().cloned().collect();
        for cache in caches {
            cache.sync();
        }
    }
    fn free_data_pages(&self, count: usize) -> usize {
        let attempts = self.data_lru_queue.lock().len();
        let mut freed = 0;
        for _ in 0..attempts {
            if freed == count {
                break;
            }
            let Some(block_id) = self.data_lru_queue.lock().pop() else {
                break;
            };
            if self.try_evict(block_id) {
                freed += 1;
            } else {
                self.data_lru_queue.lock().update(block_id);
            }
        }
        freed
    }
}

lazy_static! {
    pub static ref SHARED_PAGE_CACHE_MANAGER: PageCacheManager = PageCacheManager::new();
}

pub fn get_block_cache(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Arc<PageCache> {
    SHARED_PAGE_CACHE_MANAGER.get_block_cache(block_id, block_device)
}

pub fn get_data_block_cache(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Arc<PageCache> {
    SHARED_PAGE_CACHE_MANAGER.get_data_block_cache(block_id, block_device)
}

pub fn invalidate_block_cache(block_id: usize) {
    SHARED_PAGE_CACHE_MANAGER.invalidate_block(block_id as u64);
}

pub fn block_cache_sync_all() {
    SHARED_PAGE_CACHE_MANAGER.sync_all();
}

pub fn sync_shared_page_cache() {
    SHARED_PAGE_CACHE_MANAGER.sync_all();
    LAST_SYNC_TIME.store(crate::arch::timer::get_time_ms(), Ordering::Release);
}

const SYNC_INTERVAL_MS: usize = 20000;
static LAST_SYNC_TIME: AtomicUsize = AtomicUsize::new(0);

pub fn tick_sync() {
    let now = crate::arch::timer::get_time_ms();
    let last = LAST_SYNC_TIME.load(Ordering::Relaxed);
    if now.wrapping_sub(last) >= SYNC_INTERVAL_MS
        && LAST_SYNC_TIME
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        sync_shared_page_cache();
    }
}

pub fn next_sync_delay_ms() -> usize {
    let elapsed = crate::arch::timer::get_time_ms()
        .wrapping_sub(LAST_SYNC_TIME.load(Ordering::Relaxed));
    SYNC_INTERVAL_MS.saturating_sub(elapsed.min(SYNC_INTERVAL_MS))
}

pub fn free_up_mem_space(std_pages: usize) -> usize {
    SHARED_PAGE_CACHE_MANAGER.free_data_pages(std_pages)
}
