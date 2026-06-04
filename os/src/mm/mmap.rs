#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use crate::{mm::{FrameTracker, MapArea, PhysPageNum, frame_alloc}, task::processor::*};
use alloc::{
    sync::Arc,
    collections::BTreeMap,
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
        const MAP_FILE      = 0;
        const MAP_SHARED    = 0x01;
        const MAP_PRIVATE   = 0x02;
        const MAP_FIXED     = 0x10;
        const MAP_ANONYMOUS = 0x20;
    }
}

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
    };
}

/// 共享映射页缓存管理器
pub struct SharedPageCacheManager {
    // (ino, page_offset) -> SharedPageCache
    page_cache_map: Mutex<BTreeMap<(u64, usize), FrameTracker>>,
}

impl SharedPageCacheManager {
    /// 获取共享页缓存，返回页框和是否新分配的标志
    pub fn get_shared_page_cache(&self, ino: u64, page_offset: usize) -> (FrameTracker, bool) {
        let mut map = self.page_cache_map.lock();
        let key = (ino, page_offset);
        if let Some(cache) = map.get(&key) {
            (cache.clone(), false)
        } else {
            // 如果没有，分配一个新的页框插入缓存
            let frame = frame_alloc(super::PageSize::Page4K).unwrap();
            map.insert(key, frame.clone());
            (frame, true)
        }
    }
}

/// 用于sync系统调用，将缓存内容写回文件
pub fn sync_shared_page_cache() {
    // TODO
}