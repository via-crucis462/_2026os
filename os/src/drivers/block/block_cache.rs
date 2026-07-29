//! 块设备的块缓存实现
//! 
//! 当前内核的缓存机制可以叫作“间接二级缓存”
//! 一级缓存是文件的页缓存，二级缓存是块设备的块缓存
//! 
//! 设备关机时会同步块缓存但不同步页缓存

use super::BlockDevice;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

use crate::ext4fs::BLOCK_SZ;

pub struct BlockCache {
    cache: Vec<u8>,
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
    modified: bool,
}

impl BlockCache {
    /// Load a new BlockCache from disk.
    pub fn new(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        let mut this = Self::new_empty(block_id, block_device);
        this.fill_from_disk();
        this
    }

    /// 创建空缓存（不读磁盘），用于防惊群：先插入 map 再在锁外 I/O
    pub fn new_empty(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        Self {
            cache: vec![0u8; BLOCK_SZ],
            block_id,
            block_device,
            modified: false,
        }
    }

    /// 从磁盘填充缓存数据
    /// 调用者必须保证获取对应block的锁
    pub fn fill_from_disk(&mut self) {
        self.block_device.raw_read_block(self.block_id, &mut self.cache);
    }

    fn addr_of_offset(&self, offset: usize) -> usize {
        &self.cache[offset] as *const _  as usize
    }

    pub fn get_ref<T>(&self, offset: usize) -> &T
    where
        T: Sized,
    {
        let type_size = core::mem::size_of::<T>();
        assert!(offset + type_size <= BLOCK_SZ);
        let addr = self.addr_of_offset(offset);
        unsafe { &*(addr as *const T) }
    }

    pub fn get_mut<T>(&mut self, offset: usize) -> &mut T
    where
        T: Sized,
    {
        let type_size = core::mem::size_of::<T>();
        assert!(offset + type_size <= BLOCK_SZ);
        self.modified = true;
        let addr = self.addr_of_offset(offset);
        unsafe { &mut *(addr as *mut T) }
    }

    pub fn read<T, V>(&self, offset: usize, f: impl FnOnce(&T) -> V) -> V {
        f(self.get_ref(offset))

    }
    /// 将缓存指定偏移的块执行f操作，其中f是一个闭包
    pub fn modify<T, V>(&mut self, offset: usize, f: impl FnOnce(&mut T) -> V) -> V {
        f(self.get_mut(offset))
    }

    pub fn sync(&mut self) {
        if self.modified {
            self.modified = false;
            self.block_device.raw_write_block(self.block_id, &self.cache);
        }
    }
}

impl Drop for BlockCache {
    fn drop(&mut self) {
        self.sync()
    }
}

// 1MB 块缓存
const BLOCK_CACHE_SIZE: usize = 256;

/// 块设备缓存管理器，目前是 LRU
/// 用于块设备的底层 read_block 和 write_block
/// 先前的实现只用于元数据，现在改为所有块设备访问都经过
/// 主要用于加速 extent 树的遍历&修改 
/// 上层文件的文件缓存则使用 mmap 的缓存管理器
/// 
/// 锁序：map → queue
/// 需要磁盘 I/O 在这两把锁外完成
/// 但需要保证对于同一块的访问需要在在 block_cache 锁内完成
/// 类似页缓存的思路
pub struct BlockCacheManager {
    /// LRU 驱逐队列（最近使用的在队尾）
    queue: Mutex<VecDeque<usize>>,
    /// block_id → BlockCache
    map: Mutex<BTreeMap<usize, Arc<Mutex<BlockCache>>>>,
}

impl BlockCacheManager {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            map: Mutex::new(BTreeMap::new()),
        }
    }

    /// 获取块缓存
    pub fn get_block_cache(
        &self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<Mutex<BlockCache>> {
        // 检查缓存是否命中
        {
            let map = self.map.lock();
            if let Some(cache) = map.get(&block_id) {
                // 更新 LRU：移到队尾
                let mut queue = self.queue.lock();
                if let Some(pos) = queue.iter().position(|id| *id == block_id) {
                    queue.remove(pos);
                    queue.push_back(block_id);
                }
                return Arc::clone(cache);
            }
        }

        //不命中
        let mut map = self.map.lock();
        // 检查两次持 map 锁期间是否被其他核加载
        if let Some(cache) = map.get(&block_id) {
            let mut queue = self.queue.lock();
            if let Some(pos) = queue.iter().position(|id| *id == block_id) {
                queue.remove(pos);
                queue.push_back(block_id);
            }
            return Arc::clone(cache);
        }
        // 更新 lru
        {
            let mut queue = self.queue.lock();
            if queue.len() >= BLOCK_CACHE_SIZE {
                let mut evicted = false;
                let orig_len = queue.len();
                for _ in 0..orig_len {
                    let front_id = *queue.front().unwrap();
                    let can_evict = map.get(&front_id)
                        .map_or(false, |cache| Arc::strong_count(cache) == 1);
                    if can_evict {
                        queue.pop_front();
                        map.remove(&front_id);
                        evicted = true;
                        break;
                    } else {
                        if let Some(id) = queue.pop_front() {
                            queue.push_back(id);
                        }
                    }
                }
                if !evicted {
                    println!(
                        "Run out of BlockCache! All {} blocks are still referenced.",
                        BLOCK_CACHE_SIZE
                    );
                }
            }
        }

        // 获取块缓存锁
        let block_cache = Arc::new(Mutex::new(BlockCache::new_empty(
            block_id,
            Arc::clone(&block_device),
        )));
        let mut block_guard = block_cache.lock();
        map.insert(block_id, Arc::clone(&block_cache));

        // 更新 LRU
        {
            let mut queue = self.queue.lock();
            queue.push_back(block_id);
        }

        // 释放 map 锁
        drop(map);

        // 在blockcache锁内io
        block_guard.fill_from_disk();
        drop(block_guard);

        block_cache
    }
}

lazy_static! {
    /// 块缓存管理器
    pub static ref BLOCK_CACHE_MANAGER: BlockCacheManager =
        BlockCacheManager::new();
}

pub fn get_block_cache(
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
) -> Arc<Mutex<BlockCache>> {
    BLOCK_CACHE_MANAGER.get_block_cache(block_id, block_device)
}

pub fn block_cache_sync_all() {
    let map = BLOCK_CACHE_MANAGER.map.lock();
    for (_, cache) in map.iter() {
        cache.lock().sync();
    }
}
