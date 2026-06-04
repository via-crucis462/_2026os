#![deny(warnings)]
#![allow(missing_docs)]

use bitflags::*;
use riscv::register;
use crate::{mm::{FrameTracker, MapArea, PhysPageNum, UserBuffer, frame_alloc}, task::processor::*};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
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
    // (ino, page_offset) -> SharedPageCache
    page_cache_map: Mutex<BTreeMap<(u64, usize), FrameTracker>>,
    // ino -> Weak<File>，用于回写找到文件
    file_register: Mutex<BTreeMap<u64, Weak<dyn File + Send + Sync>>>,
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
    /// 将共享页缓存内容写回文件
    pub fn write_back_shared_page_cache(&self, ino: u64, page_offset: usize, file: &Arc<dyn File + Send + Sync>) {
        let mut map = self.page_cache_map.lock();
        let key = (ino, page_offset);
        if let Some(cache) = map.get(&key) {
            let buf = cache.get_bytes_array();
            let buffer = UserBuffer::new(alloc::vec![buf]);
            file.write_at(page_offset * crate::PAGE_SIZE, buffer);
        } else {
            error!("Shared page cache not found for ino {}, page_offset {}", ino, page_offset);
        }
    }
    // 注册文件以便回写时找到
    pub fn register_file(&self, ino: u64, file: &Arc<dyn File + Send + Sync>) {
        let mut reg = self.file_register.lock();
        reg.insert(ino, Arc::downgrade(file));
    }
    // 注销文件，同时释放该文件对应的所有缓存页
    pub fn unregister_file(&self, ino: u64) {
        let mut reg = self.file_register.lock();
        reg.remove(&ino);
        // 释放掉该文件对应的所有缓存页
        let mut map = self.page_cache_map.lock();
        map.retain(|(_ino, _), _| *_ino != ino);
    }
    // 释放共享页缓存
    pub fn remove_shared_page_cache(&self, ino: u64, page_offset: usize) {
        let mut map = self.page_cache_map.lock();
        let key = (ino, page_offset);
        map.remove(&key);
    }
    /// 清理已关闭文件的注册
    pub fn clear_closed_files(&self) {
        let mut register = self.file_register.lock();
        register.retain(|_, weak_file| weak_file.upgrade().is_some());
    }
    /// 同步共享页缓存，将所有缓存内容写回对应文件
    pub fn sync_shared_page_cache(&self) {
        let map = self.page_cache_map.lock();
        let reg = self.file_register.lock();
        for weak_file in reg.values(){
            if let Some(file) = weak_file.upgrade() {
                let ino = file.ino();
                let start = (ino, 0);
                let end = (ino + 1, 0);
                for ((_, page_offset), _) in map.range(start..end) {
                self.write_back_shared_page_cache(ino, *page_offset, &file);
            }
            }
        }
    }
}

/// 用于sync系统调用，将缓存内容写回文件
pub fn sync_shared_page_cache() {
    let man = &SHARED_PAGE_CACHE_MANAGER;
    man.clear_closed_files();
    man.sync_shared_page_cache();
}