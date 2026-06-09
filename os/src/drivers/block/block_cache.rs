//! 块设备的块缓存实现
//! 
//! 当前内核的缓存机制可以叫作“间接二级缓存”
//! 一级缓存是文件的页缓存，二级缓存是块设备的块缓存
//! 
//! 设备关机时会同步块缓存但不同步页缓存

use super::{BlockDevice, BLOCK_SZ};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

pub struct BlockCache {
    cache: Vec<u8>,
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
    modified: bool,
}

impl BlockCache {
    /// Load a new BlockCache from disk.
    pub fn new(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        // for alignment and move effciency
        let mut cache = vec![0u8; BLOCK_SZ];
        block_device.raw_read_block(block_id, &mut cache);
        Self {
            cache,
            block_id,
            block_device,
            modified: false,
        }
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

/// 块设备缓存管理器，目前是LRU
/// 用于块设备的底层 read_block 和 write_block
/// 先前的实现只用于元数据，现在改为所有块设备访问都经过
/// 主要用于加速 extent 树的遍历&修改 
/// 上层文件的文件缓存则使用 mmap 的缓存管理器
pub struct BlockCacheManager {
    // block_id 队列
    queue: VecDeque<usize>,
    // block_id -> BlockCache
    map: BTreeMap<usize, Arc<Mutex<BlockCache>>>,
}

impl BlockCacheManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            map: BTreeMap::new(),
        }
    }

    pub fn get_block_cache(
        &mut self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<Mutex<BlockCache>> {
        // 命中
        if let Some(cache) = self.map.get(&block_id) {
            if let Some(pos) = self.queue.iter().position(|id| *id == block_id) {
                self.queue.remove(pos);
                self.queue.push_back(block_id);
            }
            return Arc::clone(cache);
        }

        // 未命中
        if self.queue.len() >= BLOCK_CACHE_SIZE {
            loop {
                let mut evicted = false;
                for _ in 0..self.queue.len() {
                    let front_id = self.queue.front().copied().unwrap();
                    let can_evict = self.map.get(&front_id)
                    .map_or(false, |cache| {
                        Arc::strong_count(cache) == 1 // 仅 map 持有
                    });
                    if can_evict {
                        self.queue.pop_front();
                        self.map.remove(&front_id);
                        evicted = true;
                        break;
                    } else {
                        // 仍被外部引用，移到队尾
                        if let Some(id) = self.queue.pop_front() {
                            self.queue.push_back(id);
                        }
                    }
                }
                if evicted {
                    break;
                }
                println!("Run out of BlockCache! All {} blocks are still referenced.", BLOCK_CACHE_SIZE);
            }
        }

        // 加载新块
        let block_cache = Arc::new(Mutex::new(BlockCache::new(
            block_id,
            Arc::clone(&block_device),
        )));
        self.map.insert(block_id, Arc::clone(&block_cache));
        self.queue.push_back(block_id);
        block_cache
    }
}

lazy_static! {
    /// 块缓存管理器，主要用于磁盘元数据
    pub static ref BLOCK_CACHE_MANAGER: Mutex<BlockCacheManager> =
        Mutex::new(BlockCacheManager::new());
}

pub fn get_block_cache(
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
) -> Arc<Mutex<BlockCache>> {
    BLOCK_CACHE_MANAGER
        .lock()
        .get_block_cache(block_id, block_device)
}

pub fn block_cache_sync_all() {
    let manager = BLOCK_CACHE_MANAGER.lock();
    for (_, cache) in manager.map.iter() {
        cache.lock().sync();
    }
}
