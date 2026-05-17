//! loop设备支持
//! 如下摘自https://blog.csdn.net/zhongbeida_xue/article/details/109657639
//! Loop设备是一种块设备，但是它并不指向硬盘或者光驱，而是指向一个文件块或者另一种块设备。
//! 一种应用的例子：将另外一种文件系统的镜像文件保存到一个文件中，例如iso文件，然后将一个Loop设备指向该文件，
//! 紧接着就可以通过mount挂载该loop设备到主文件系统的一个目录下了，我们就可以正常访问该镜像中的内容，
//! 就像访问一个文件系统一样。

use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::MPSafeCell;
use crate::fs::{File, OSInode, VfsInode, TimeSpec, ROOT_DENTRY, file_name, parent_path};
use crate::ext4fs::BlockDevice;
use crate::ext4fs::ext4::Ext4FS;
use crate::ext4fs::ext4inode::Ext4Inode;
use crate::mm::UserBuffer;
use crate::process::id::RecycleAllocator;
use crate::syscall::errno::Errno;

use lazy_static::lazy_static;

lazy_static! {
    /// 全局Loop设备管理器
    pub static ref LOOP_DEVICE_MANAGER: LoopDeviceManager = LoopDeviceManager::new();
}

/// Loop设备管理器，创建和管理Loop设备
pub struct LoopDeviceManager {
    inner: MPSafeCell<LoopDeviceManagerInner>,
    id_allocator: MPSafeCell<RecycleAllocator>,
}

impl LoopDeviceManager {
    pub fn new() -> Self {
        Self {
            inner: MPSafeCell::new(LoopDeviceManagerInner {
                devices: Vec::new(),
            }),
            id_allocator: MPSafeCell::new(RecycleAllocator::new()), // 假设Loop设备ID范围是0-1023
        }
    }
    /// 创建一个Loop设备，参数包括：指向Loop设备的文件、Loop设备在文件中的偏移量、Loop设备的大小
    pub fn create_loop_device(&self, backing_file: Option<Arc<dyn VfsInode>>, offset: usize, size: usize) -> Arc<LoopDevice> {
        let loop_device = Arc::new(LoopDevice {
            inner: Arc::new(MPSafeCell::new(LoopDeviceInner::new(backing_file, offset, size))),
            device_id: self.id_allocator.exclusive_access().alloc(),
        });
        self.inner.exclusive_access().devices.push(loop_device.clone());
        loop_device
    } 
    pub fn remove_loop_device(&self, device_id: usize) {
        let mut inner = self.inner.exclusive_access();
        if let Some(pos) = inner.devices.iter().position(|d| d.device_id == device_id) {
            inner.devices.remove(pos);
            self.id_allocator.exclusive_access().dealloc(device_id);
        }
    }
    /*
    pub fn inner_exclusive_access(&self) -> impl core::ops::DerefMut<Target=LoopDeviceManagerInner> + '_ {
        self.inner.exclusive_access()
    } */
    pub fn get_free_id(&self) -> isize {
        let inner = self.inner.exclusive_access();
        // 暂时写成硬编码
        for i in 0..8 {
            if let Some(dev) = inner.devices.iter().find(|d| d.device_id == i) {
                if dev.inner.exclusive_access().backing_file.is_none() {
                    return i as isize;
                }
            }
        }
        -1
    }
    
    pub fn set_backing_file(&self, device_id: usize, backing_file: Option<Arc<dyn VfsInode>>) -> bool {
        let inner = self.inner.exclusive_access();
        if let Some(dev) = inner.devices.iter().find(|d| d.device_id == device_id) {
            dev.inner.exclusive_access().set_backing_file(backing_file);
            true
        } else {
            false
        }
    }

    pub fn get_info(&self, device_id: usize) -> Option<(usize, usize)> {
        let inner = self.inner.exclusive_access();
        if let Some(dev) = inner.devices.iter().find(|d| d.device_id == device_id) {
            let inner_dev = dev.inner.exclusive_access();
            if let Some(ref bf) = inner_dev.backing_file {
                Some((inner_dev.offset, bf.get_size()))
            } else {
                None
            }
        } else {
            None
        }
    }
}

pub struct LoopDeviceManagerInner {
    devices: Vec<Arc<LoopDevice>>,
}

/// 创建一个Loop设备，参数包括：指向Loop设备的文件、Loop设备在文件中的偏移量、Loop设备的大小
pub struct LoopDevice {
    pub inner: Arc<MPSafeCell<LoopDeviceInner>>,
    pub device_id: usize, // Loop设备的ID，可以用于标识不同的Loop设备
}

impl BlockDevice for LoopDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        self.inner.exclusive_access().read_at(block_id, buf);
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) {
        self.inner.exclusive_access().write_at(block_id, buf);
    }
}

pub struct LoopDeviceInner {
    pub backing_file: Option<Arc<dyn VfsInode>>,
    pub offset: usize,
    pub size: usize,
}

impl LoopDeviceInner {
    fn new(backing_file: Option<Arc<dyn VfsInode>>, offset: usize, size: usize) -> Self {
        Self {
            backing_file,
            offset,
            size,
        }
    }
    // 更新Loop设备的backing file，并根据新的backing file更新设备大小
    pub fn set_backing_file(&mut self, backing_file: Option<Arc<dyn VfsInode>>) {
        self.backing_file = backing_file;
        if let Some(ref bf) = self.backing_file {
            self.size = bf.get_size();
        } else {
            self.size = 0;
        }
    }
    fn read_at(&self, block_id: usize ,buf: &mut [u8]) -> usize {
        let size = buf.len();
        debug_assert_eq!(size, 512, "Block size should strictly matching disk sector size");
        let file_size = self.backing_file.as_ref().unwrap().get_size();
        let actural_offset = self.offset + block_id * size;
        if actural_offset >= file_size {
            return 0; // 超出文件大小，返回0字节
        }
        self.backing_file.as_ref().unwrap().read_at(actural_offset, buf)
    }
    fn write_at(&self, block_id: usize, buf: &[u8]) -> usize {
        let size = buf.len();
        debug_assert_eq!(size, 512, "Block size should strictly matching disk sector size");
        let actural_offset = self.offset + block_id * size;
        let file_size = self.backing_file.as_ref().unwrap().get_size();
        if actural_offset >= file_size {
            // 本来可以直接报错返回，但将错误处理交给底层一级的文件系统驱动
        }
        self.backing_file.as_ref().unwrap().write_at(actural_offset, buf)
    }
}

impl VfsInode for LoopDevice {
    fn get_size(&self) -> usize {
        let inner = self.inner.exclusive_access();
        if let Some(ref bf) = inner.backing_file {
            bf.get_size()
        } else {
            0
        }
    }
    fn set_time(&self, _atime: &TimeSpec, _mtime: &TimeSpec) -> isize {
        -1 // 不支持设置时间
    }
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let inner = self.inner.exclusive_access();
        // Also just forward read
        inner.backing_file.as_ref().unwrap().read_at(inner.offset + offset, buf)
    }
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let inner = self.inner.exclusive_access();
        // Just forward the write to the backing file without size limitation to allow file extension if needed
        inner.backing_file.as_ref().unwrap().write_at(inner.offset + offset, buf)
    }
    fn create_dir(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>> {
        None // Loop设备不支持创建目录
    }
    fn create_file(&self, name: &str, mode: u32) -> Option<Arc<dyn VfsInode>> {
        None // Loop设备不支持创建文件
    }
    fn get_stat(&self) -> crate::fs::Stat {
        crate::fs::Stat {
            mode: 0o060666, // 块设备标志位 (S_IFBLK) | rw-rw-rw-
            blksize: 512,
            size: self.get_size() as i64,
            ..Default::default()
        }
    }
    fn get_statx(&self) -> crate::fs::Statx {
        let mut stx = crate::fs::Statx::default();
        stx.stx_mode = 0o060666;
        stx.stx_blksize = 512;
        stx.stx_size = self.get_size() as u64;
        stx
    }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

// 创建一个Loop设备，参数包括：指向Loop设备的文件、Loop设备在文件中的偏移量、Loop设备的大小
pub fn create_loop_device(backing_file: Option<Arc<dyn VfsInode>>, offset: usize, size: usize) -> Arc<LoopDevice> {
    LOOP_DEVICE_MANAGER.create_loop_device(backing_file, offset, size)
}

// 挂载Loop设备到指定的挂载点，暂时保留不使用
pub fn mount_loop_device(loop_device: Arc<LoopDevice>, mount_point: &str) -> Result<usize, isize>{
    // 创建一个loop设备实例
    let id = loop_device.device_id;
    let ext4fs = Ext4FS::open(loop_device);
    let root_disk_inode = ext4fs.get_disk_inode(2);
    let root_inode = Arc::new(Ext4Inode::new(2, &root_disk_inode, Arc::new(ext4fs), None));

    // 挂载到文件系统目录树中
    let parent_dir = parent_path(mount_point);
    let name = file_name(mount_point);
    
    if let Some(parent_dentry) = ROOT_DENTRY.find_tree(&parent_dir, true) {
        parent_dentry.insert(name, root_inode);
        Ok(id)
    } else {
        return Err(Errno::ENOENT.as_isize());
    }
}

/// loop control设备
pub struct LoopControlInode {}

impl LoopControlInode {
    pub fn new() -> Self { Self {} }
}

impl VfsInode for LoopControlInode {
    fn get_size(&self) -> usize { 0 }
    fn set_time(&self, _atime: &crate::fs::TimeSpec, _mtime: &crate::fs::TimeSpec) -> isize { 0 }
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize { buf.len() }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn get_stat(&self) -> crate::fs::Stat {
        crate::fs::Stat {
            mode: 0o20666, // S_IFCHR
            blksize: 512,
            ..Default::default()
        }
    }
    fn get_statx(&self) -> crate::fs::Statx {
        let mut stx = crate::fs::Statx::default();
        stx.stx_mode = 0o20666;
        stx.stx_blksize = 512;
        stx
    }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}