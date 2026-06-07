use crate::process::id::RecycleAllocator;
use crate::sync::MPSafeCell;
use crate::mm::{FrameTracker, PageSize, frame_alloc, frame_dealloc};
use spin::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use super::IpcPerm;


use lazy_static::lazy_static;

lazy_static! {
    /// 全局shm管理器
    pub static ref SHM_MANAGER: MPSafeCell<ShmManager> = MPSafeCell::new(ShmManager::new());
}

pub struct ShmManager {
    id_allocator: RecycleAllocator,
    // id->Shm
    shms: BTreeMap<u32, Arc<Shm>>,
}

impl ShmManager {
    pub fn new() -> Self {
        Self {
            id_allocator: RecycleAllocator::new(),
            shms: BTreeMap::new(),
        }
    }
    pub fn get_shm(&self, id: u32) -> Option<Arc<Shm>> {
        self.shms.get(&id).cloned()
    }

    pub fn get_shm_by_key(&self, key: i32) -> Option<Arc<Shm>> {
        self.shms.values().find(|s| s.get_key() == key).cloned()
    }

    pub fn remove_shm(&mut self, id: u32) {
        self.shms.remove(&id);
        self.id_allocator.dealloc(id as usize);
    }
    pub fn create_shm(&mut self, size: usize, key: i32, mode: u16, cpid: usize) -> Arc<Shm> {
        let id = self.id_allocator.alloc();
        // 上溢出检查
        if id > u32::MAX as usize {
            panic!("Too many shared memory segments");
        }
        let shm = Arc::new(Shm::new(id as u32, size, key, mode, cpid));
        self.shms.insert(id as u32, shm.clone());
        shm
    }
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
    // 当前连接数，即被映射到多少个进程
    pub shm_nattch: usize,
}

pub struct Shm {
    id: u32,
    // 需要保证顺序
    frames: Vec<FrameTracker>,
    // 状态信息，注意和inode stat不同
    pub stat: Mutex<ShmidDs>,
}

impl Shm {
    pub fn new(id: u32, size: usize, key: i32, mode: u16, cpid: usize) -> Self {
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
    pub fn get_id(&self) -> u32 {
        self.id
    }
    pub fn get_size(&self) -> usize {
        self.stat.lock().shm_segsz
    }
    pub fn get_key(&self) -> i32 {
        self.stat.lock().shm_perm.key
    }
    pub fn get_frames(&self) -> &Vec<FrameTracker> {
        &self.frames
    }
    pub fn inc_nattch(&self) {
        self.stat.lock().shm_nattch += 1;
    }
    pub fn dec_nattch(&self) {
        let mut stat = self.stat.lock();
        if stat.shm_nattch > 0 {
            stat.shm_nattch -= 1;
        }
    }
    pub fn get_perm(&self) -> IpcPerm {
        self.stat.lock().shm_perm
    }
    pub fn get_stat(&self) -> ShmidDs {
        *self.stat.lock()
    }
}

pub fn get_new_shm(size: usize, key: i32, mode: u16, cpid: usize) -> Arc<Shm> {
    SHM_MANAGER.exclusive_access().create_shm(size, key, mode, cpid)
}

pub fn get_shm_by_id(id: u32) -> Option<Arc<Shm>> {
    SHM_MANAGER.exclusive_access().shms.get(&id).cloned()
}

pub fn get_shm_by_key(key: i32) -> Option<Arc<Shm>> {
    SHM_MANAGER.exclusive_access().shms.values().find(|s| s.get_key() == key).cloned()
}