//! 页缓存/块缓存管理
//! 
//! 当前实现合并了原本的 SharedPageCacheManager 和 BlockCacheManager 的功能
//! 合并的目的主要是相比旧实现减少一次数据拷贝，但可能不完善

use super::BlockDevice;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::{Mutex, MutexGuard};

use crate::ext4fs::BLOCK_SZ;
use crate::mm::{frame_alloc, FrameTracker, PageSize};
use crate::sync::RwLock;

/// 全局访问时间戳，用于无锁的近似 LRU 淘汰
static CACHE_STAMP: AtomicU64 = AtomicU64::new(0);

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
    /// 数据块缓存还是元数据块缓存（淘汰时按类分别限制）
    is_data: bool,
    /// 最近一次访问的全局时间戳（仅用于淘汰决策，无需精确）
    last_used: AtomicU64,
}

impl PageCache {
    fn new_loading(block_id: usize, block_device: Arc<dyn BlockDevice>, is_data: bool) -> Self {
        Self {
            inner: Mutex::new(PageCacheInner {
                frame: frame_alloc(PageSize::Page4K).unwrap(),
                dirty: false,
                state: CacheState::Loading,
                discarded: false,
            }),
            block_id,
            block_device: Some(block_device),
            is_data,
            last_used: AtomicU64::new(0),
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
            is_data: false,
            last_used: AtomicU64::new(0),
        }
    }
    /// 无锁记录一次访问（近似 LRU 用）
    pub fn touch(&self) {
        self.last_used
            .store(CACHE_STAMP.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
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

// 元数据缓存的最大数量，超过该数量时会尝试回收
const META_CACHE_SIZE: usize = 1 << 18; // 1GB
/// 数据页缓存的最大页数，超过时从 LRU 队头回收
#[cfg(target_arch = "riscv64")]
const DATA_CACHE_SIZE: usize = 1 << 21; // 8GB
#[cfg(target_arch = "loongarch64")]
const DATA_CACHE_SIZE: usize = 1 << 22; // 16GB

type LogicalPageKey = (u64, usize);

/// 文件页双向索引表
///
/// 在原有 (ino, logical_id) -> physical_id 单向表的基础上，
/// 增加反向索引便于释放时的快速查找，避免全表扫描
struct CacheIdMap {
    logical_to_physical: BTreeMap<LogicalPageKey, u64>,
    physical_to_logical: BTreeMap<u64, BTreeSet<LogicalPageKey>>,
}

impl CacheIdMap {
    fn new() -> Self {
        Self {
            logical_to_physical: BTreeMap::new(),
            physical_to_logical: BTreeMap::new(),
        }
    }

    fn insert(&mut self, key: LogicalPageKey, block_id: u64) {
        if let Some(old_block_id) = self.logical_to_physical.insert(key, block_id) {
            if old_block_id != block_id {
                self.remove_reverse(old_block_id, key);
            }
        }
        self.physical_to_logical
            .entry(block_id)
            .or_default()
            .insert(key);
    }

    fn get(&self, key: &LogicalPageKey) -> Option<u64> {
        self.logical_to_physical.get(key).copied()
    }

    fn remove_block(&mut self, block_id: u64) {
        let Some(keys) = self.physical_to_logical.remove(&block_id) else {
            return;
        };
        for key in keys {
            if self.logical_to_physical.get(&key) == Some(&block_id) {
                self.logical_to_physical.remove(&key);
            }
        }
    }

    fn remove_reverse(&mut self, block_id: u64, key: LogicalPageKey) {
        let remove_entry = if let Some(keys) = self.physical_to_logical.get_mut(&block_id) {
            keys.remove(&key);
            keys.is_empty()
        } else {
            false
        };
        if remove_entry {
            self.physical_to_logical.remove(&block_id);
        }
    }

    fn len(&self) -> usize {
        self.logical_to_physical.len()
    }
}

/// 页缓存管理器
/// 
/// 管理所有文件系统数据块和元数据块的缓存。
/// 从原有 SharedPageCacheManager 修改而来，
/// 并且合并了原本 BlockCacheManager 的功能
/// 
pub struct PageCacheManager {
    /// (ino, logical_block_id) -> physical_block_id
    page_cache_id_map: Mutex<CacheIdMap>,
    /// physical_block_id -> cache
    page_cache_map: RwLock<BTreeMap<u64, Arc<PageCache>>>,
    /// 元数据缓存条目数（避免每次 miss 都全表扫描判断是否超限）
    meta_count: AtomicUsize,
    /// 数据缓存条目数
    data_count: AtomicUsize,
    /// 元数据和数据缓存的轮转扫描起点，避免候选长期偏向低物理块号
    meta_scan_cursor: AtomicU64,
    data_scan_cursor: AtomicU64,
}

impl PageCacheManager {
    fn new() -> Self {
        Self {
            page_cache_id_map: Mutex::new(CacheIdMap::new()),
            page_cache_map: RwLock::new(BTreeMap::new()),
            meta_count: AtomicUsize::new(0),
            data_count: AtomicUsize::new(0),
            meta_scan_cursor: AtomicU64::new(0),//游标，环形替换
            data_scan_cursor: AtomicU64::new(0),
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
        // 先尝试获取已存在的缓存
        {
            let map = self.page_cache_map.read();
            if let Some(cache) = map.get(&block_id).cloned() {
                drop(map);
                cache.touch();
                return (cache, false);
            }
        }
        // 这里释放了 page_cache_map 锁，避免内存不足时回收缓存时死锁

        // 旧实现持 page_cache_map 锁创建缓存，现在改为先创建缓存再插入 map
        let cache = Arc::new(PageCache::new_loading(
            block_id as usize,
            block_device.clone(),
            is_data,
        ));

        // 条目公开到全局 map 前必须完成初始化，否则并发查找可能把
        // Loading 状态下的空白 frame 当作文件内容。
        {
            let mut inner = cache.inner.lock();
            if load_from_disk {
                block_device.raw_read_block(block_id as usize, inner.frame.get_bytes_array());
                inner.state = CacheState::Clean;
            } else {
                inner.frame.get_bytes_array().fill(0);
                inner.dirty = true;
                inner.state = CacheState::Dirty;
            }
        }

        let mut map = self.page_cache_map.write();
        if let Some(existing) = map.get(&block_id).cloned() {
            // 防止并发重复插入
            cache.inner.lock().discarded = true;
            drop(map);
            drop(cache);
            existing.touch();
            return (existing, false);
        }
        map.insert(block_id, cache.clone());
        drop(map);
        cache.touch();

        if !is_data {
            self.meta_count.fetch_add(1, Ordering::Relaxed);
            /*
            if self.meta_count.load(Ordering::Relaxed) > META_CACHE_SIZE {
                self.trim_meta_cache();
            }
            */
        } else {
            self.data_count.fetch_add(1, Ordering::Relaxed);
        }
        (cache, true)
    }
    /// 尝试回收元数据缓存
    fn trim_meta_cache(&self) {
        self.trim_cache(META_CACHE_SIZE, false);
    }
    /// 尝试回收超过限制的数据缓存
    fn trim_data_cache(&self) {
        self.trim_cache(DATA_CACHE_SIZE, true);
    }
    /// 按 last_used 轮转扫描淘汰最旧缓存。
    /// 对应类别超过 limit 时最多淘汰一个块，每次扫描 SCAN_LIMIT 项；
    /// 后续扫描起点固定向前推进 SCAN_LIMIT 个物理块号。
    fn trim_cache(&self, limit: usize, is_data: bool) {
        const SCAN_LIMIT: usize = 64;
        let count = if is_data {
            self.data_count.load(Ordering::Relaxed)
        } else {
            self.meta_count.load(Ordering::Relaxed)
        };
        if count <= limit {
            return;
        }

        use core::ops::Bound;

        let scan_cursor = if is_data {
            &self.data_scan_cursor
        } else {
            &self.meta_scan_cursor
        };
        let scan_start = scan_cursor.load(Ordering::Relaxed);
        let mut candidates: Vec<(u64, u64)> = {
            let map = self.page_cache_map.read();
            map.range((Bound::Included(scan_start), Bound::Unbounded))
                .chain(map.range((Bound::Unbounded, Bound::Excluded(scan_start))))
                .filter(|(_, cache)| cache.is_data == is_data)
                .take(SCAN_LIMIT)
                .map(|(&block_id, cache)| {
                    (
                        cache.last_used.load(Ordering::Relaxed),
                        block_id,
                    )
                })
                .collect()
        };
        scan_cursor.store(
            scan_start.wrapping_add(SCAN_LIMIT as u64),
            Ordering::Relaxed,
        );
        candidates.sort_unstable();

        for (_, block_id) in candidates {
            if self.try_evict(block_id) {
                break;
            }
        }
    }
    /// 尝试回收指定物理块的缓存，返回是否成功回收
    fn try_evict(&self, block_id: u64) -> bool {
        let cache = {
            let map = self.page_cache_map.read();
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

        let mut map = self.page_cache_map.write();
        if map
            .get(&block_id)
            .map(|entry| Arc::ptr_eq(entry, &cache) && Arc::strong_count(entry) == 2)
            .unwrap_or(false)
        {
            map.remove(&block_id);
            if cache.is_data {
                self.data_count.fetch_sub(1, Ordering::Relaxed);
            } else {
                self.meta_count.fetch_sub(1, Ordering::Relaxed);
            }
            self.page_cache_id_map.lock().remove_block(block_id);
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
        let mut map = self.page_cache_map.write();
        if let Some(cache) = map.get(&block_id) {
            cache.lock().discarded = true;
            if cache.is_data {
                self.data_count.fetch_sub(1, Ordering::Relaxed);
            } else {
                self.meta_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
        map.remove(&block_id);
        drop(map);
        self.page_cache_id_map.lock().remove_block(block_id);
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
        let (cache, created) =
            self.get_physical_page(physical_block, block_device, true, false);
        // alloc_block() 返回的块应当不在缓存中：dealloc_block() 会在块
        // 回到位图前作废旧缓存。若不变量被破坏，保留已有缓存内容比无条件
        // 清零安全，后者会直接破坏仍被其它路径使用的物理块。
        if created {
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
        let block_id = self.page_cache_id_map.lock().get(&(ino, logical_block))?;
        self.page_cache_map.read().get(&block_id).cloned()
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
        // 分批收集
        use core::ops::Bound;
        const SYNC_BATCH: usize = 1024;
        // `0` is a valid physical block (and contains the ext4 superblock).
        // Starting with Excluded(0) silently left it dirty forever.
        let mut cursor: Option<u64> = None;
        loop {
            let batch: Vec<Arc<PageCache>> = {
                let map = self.page_cache_map.read();
                let lower = match cursor {
                    Some(block_id) => Bound::Excluded(block_id),
                    None => Bound::Unbounded,
                };
                map.range((lower, Bound::Unbounded))
                    .take(SYNC_BATCH)
                    .map(|(id, cache)| {
                        cursor = Some(*id);
                        cache.clone()
                    })
                    .collect()
            };
            if batch.is_empty() {
                break;
            }
            for cache in batch {
                cache.sync();
            }
        }
    }
    pub fn stats(&self) -> (Option<(usize, usize)>, Option<usize>, Option<usize>) {
        let map = self.page_cache_map.read();
        let pinned = map
            .values()
            .filter(|cache| Arc::strong_count(cache) > 1)
            .count();
        let page_stats = (map.len(), pinned);
        drop(map);
        (
            Some(page_stats),
            Some(self.page_cache_id_map.lock().len()),
            None,
        )
    }
    fn free_data_pages(&self, count: usize) -> usize {
        let mut freed = 0;
        while freed < count {
            let victim = {
                let map = self.page_cache_map.read();
                let mut oldest: Option<(u64, u64)> = None;
                for (&bid, c) in map.iter() {
                    if c.is_data {
                        let lu = c.last_used.load(Ordering::Relaxed);
                        if oldest.map_or(true, |(olu, _)| lu < olu) {
                            oldest = Some((lu, bid));
                        }
                    }
                }
                oldest.map(|(_, bid)| bid)
            };
            let Some(block_id) = victim else {
                break;
            };
            if self.try_evict(block_id) {
                freed += 1;
            } else {
                break;
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

pub fn trim_cache() {
    SHARED_PAGE_CACHE_MANAGER.trim_data_cache();
    SHARED_PAGE_CACHE_MANAGER.trim_meta_cache();
}

pub fn block_cache_sync_all() {
    SHARED_PAGE_CACHE_MANAGER.sync_all();
}

pub fn sync_shared_page_cache() {
    SHARED_PAGE_CACHE_MANAGER.sync_all();
    LAST_SYNC_TIME.store(crate::arch::timer::get_time_ms(), Ordering::Release);
}

const SYNC_INTERVAL_MS: usize = 200_000;
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
        trim_cache();
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
