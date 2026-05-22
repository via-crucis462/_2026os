use crate::process::id::RecycleAllocator;
use crate::sync::MPSafeCell;
use crate::mm::{FrameTracker, PageSize, frame_alloc, frame_dealloc};
use spin::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use lazy_static::lazy_static;

lazy_static! {
    /// 全局shm管理器
    pub static ref SHM_MANAGER: MPSafeCell<ShmManager> = MPSafeCell::new(ShmManager::new());
}

pub struct ShmManager {
    id_allocator: RecycleAllocator,
    // id->Shm
    shms: BTreeMap<usize, Arc<Shm>>,
}

impl ShmManager {
    pub fn new() -> Self {
        Self {
            id_allocator: RecycleAllocator::new(),
            shms: BTreeMap::new(),
        }
    }
    pub fn remove_shm(&mut self, id: usize) {
        self.shms.remove(&id);
        self.id_allocator.dealloc(id);
    }
    pub fn create_shm(&mut self, size: usize, key: i32, mode: u16, cpid: usize) -> Arc<Shm> {
        let id = self.id_allocator.alloc();
        let shm = Arc::new(Shm::new(id, size, key, mode, cpid));
        self.shms.insert(id, shm.clone());
        shm
    }
}

/// System V IPC 权限信息
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct IpcPerm {
    pub key: i32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u16,
    pub seq: u16,
}

/// 对应 Linux shmid_ds
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ShmidDs {
    pub shm_perm: IpcPerm,
    pub shm_segsz: usize,
    pub shm_atime: usize,
    pub shm_dtime: usize,
    pub shm_ctime: usize,
    pub shm_cpid: usize,
    pub shm_lpid: usize,
    pub shm_nattch: usize,
}

pub struct Shm {
    id: usize,
    // 需要保证顺序
    frames: Vec<FrameTracker>,
    // 状态信息
    pub stat: Mutex<ShmidDs>,
}

impl Shm {
    pub fn new(id: usize, size: usize, key: i32, mode: u16, cpid: usize) -> Self {
        // 默认用4K页
        let page_size = PageSize::Page4K;
        let num_pages = (size + page_size.size() - 1) / page_size.size();
        let mut frames = Vec::new();
        for _ in 0..num_pages {
            // 分配页
            let frame = frame_alloc(page_size).expect("Failed to allocate frame for SHM");
            frames.push(frame);
        }
        let mut stat = ShmidDs::default();
        stat.shm_perm.key = key;
        stat.shm_perm.mode = mode;
        stat.shm_segsz = size;
        stat.shm_cpid = cpid;

        Self { 
            id, 
            frames,
            stat: Mutex::new(stat),
        }
    }
    pub fn get_id(&self) -> usize {
        self.id
    }
    pub fn get_size(&self) -> usize {
        self.stat.lock().shm_segsz
    }
}