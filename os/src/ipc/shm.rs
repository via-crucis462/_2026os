use crate::process::id::RecycleAllocator;
use crate::sync::MPSafeCell;
use crate::fs::tmpfs::TmpfsFileInode;
use crate::fs::{OSInode, VfsInode, Dentry};

use alloc::sync::{Arc, Weak};
use spin::Mutex;
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
    // addr -> shmid，供 shmdt 查找
    attachments: BTreeMap<usize, u32>,
}

impl ShmManager {
    pub fn new() -> Self {
        Self {
            id_allocator: RecycleAllocator::new(),
            shms: BTreeMap::new(),
            attachments: BTreeMap::new(),
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
    pub fn create_shm(&mut self, size: usize, key: i32, mode: u16, cpid: usize, uid: u32, gid: u32) -> Arc<Shm> {
        let id = self.id_allocator.alloc();
        // 上溢出检查
        if id > u32::MAX as usize {
            panic!("Too many shared memory segments");
        }
        let shm = Arc::new(Shm::new(id as u32, size, key, mode, cpid, uid, gid));
        self.shms.insert(id as u32, shm.clone());
        shm
    }
    /// 记录 shmat 的映射关系
    pub fn record_attach(&mut self, shmid: u32, addr: usize) {
        self.attachments.insert(addr, shmid);
    }
    /// 根据地址查找 shmid，查完后移除记录
    pub fn take_attach(&mut self, addr: usize) -> Option<u32> {
        self.attachments.remove(&addr)
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
    // 状态信息，注意和inode stat不同
    pub stat: Mutex<ShmidDs>,
    //
    inner: Arc<OSInode>,
}

impl Shm {
    pub fn new(id: u32, size: usize, key: i32, mode: u16, cpid: usize, uid: u32, gid: u32) -> Self {
        let mut perm = IpcPerm::default();
        perm.key = key;
        perm.mode = mode;
        perm.uid = uid;
        perm.gid = gid;

        let stat = ShmidDs {
            shm_perm: perm,
            shm_segsz: size,
            shm_atime: 0,
            shm_dtime: 0,
            shm_ctime: 0,
            shm_cpid: cpid,
            shm_lpid: 0,
            shm_nattch: 0,
        };
        let mut trunc = TmpfsFileInode::new(0o0777);
        trunc.truncate(size);
        let tmpfs = Arc::new(trunc);
        // 孤儿 Dentry：不挂 VFS 树，仅满足 OSInode 的类型要求
        let dentry = Dentry::new(
            alloc::format!("shm_{}", id),
            tmpfs.clone(),
            Weak::new(),
        );
        let os_inode = Arc::new(OSInode::new(true, true, false, dentry));
        Self { id, stat: Mutex::new(stat), inner: os_inode }
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
    pub fn set_lpid(&self, pid: usize) {
        self.stat.lock().shm_lpid = pid;
    }
    pub fn set_perm_fields(&self, uid: u32, gid: u32, mode: u16) {
        let mut s = self.stat.lock();
        s.shm_perm.uid = uid;
        s.shm_perm.gid = gid;
        s.shm_perm.mode = mode;
    }
    pub fn inner(&self) -> Arc<OSInode> {
        self.inner.clone()
    }
}

pub fn get_new_shm(size: usize, key: i32, mode: u16, cpid: usize, uid: u32, gid: u32) -> Arc<Shm> {
    SHM_MANAGER.exclusive_access().create_shm(size, key, mode, cpid, uid, gid)
}

pub fn get_shm_by_id(id: u32) -> Option<Arc<Shm>> {
    SHM_MANAGER.exclusive_access().shms.get(&id).cloned()
}

pub fn get_shm_by_key(key: i32) -> Option<Arc<Shm>> {
    SHM_MANAGER.exclusive_access().shms.values().find(|s| s.get_key() == key).cloned()
}
