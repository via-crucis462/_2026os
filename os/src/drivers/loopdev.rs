//! loop设备支持
//! 如下摘自https://blog.csdn.net/zhongbeida_xue/article/details/109657639
//! Loop设备是一种块设备，但是它并不指向硬盘或者光驱，而是指向一个文件块或者另一种块设备。
//! 一种应用的例子：将另外一种文件系统的镜像文件保存到一个文件中，例如iso文件，然后将一个Loop设备指向该文件，
//! 紧接着就可以通过mount挂载该loop设备到主文件系统的一个目录下了，我们就可以正常访问该镜像中的内容，
//! 就像访问一个文件系统一样。

use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::MPSafeCell;
use crate::fs::{File, OSInode};
use crate::ext4fs::BlockDevice;
use crate::mm::UserBuffer;

use lazy_static::lazy_static;

lazy_static! {
    /// 全局Loop设备管理器
    pub static ref LOOP_DEVICE_MANAGER: LoopDeviceManager = LoopDeviceManager::new();
}

/// Loop设备管理器，创建和管理Loop设备
pub struct LoopDeviceManager {
    inner: MPSafeCell<LoopDeviceManagerInner>,
}

impl LoopDeviceManager {
    pub fn new() -> Self {
        Self {
            inner: MPSafeCell::new(LoopDeviceManagerInner {
                devices: Vec::new(),
            }),
        }
    }
    /// 创建一个Loop设备，参数包括：指向Loop设备的文件、Loop设备在文件中的偏移量、Loop设备的大小
    pub fn create_loop_device(&self, backing_file: Arc<OSInode>, offset: usize, size: usize) -> Arc<LoopDevice> {
        let loop_device = Arc::new(LoopDevice {
            inner: Arc::new(MPSafeCell::new(LoopDeviceInner::new(backing_file, offset, size))),
        });
        self.inner.exclusive_access().devices.push(loop_device.clone());
        loop_device
    }
}

pub struct LoopDeviceManagerInner {
    devices: Vec<Arc<LoopDevice>>,
}


/// 创建一个Loop设备，参数包括：指向Loop设备的文件、Loop设备在文件中的偏移量、Loop设备的大小
pub struct LoopDevice {
    pub inner: Arc<MPSafeCell<LoopDeviceInner>>,
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
    pub backing_file: Arc<OSInode>,
    pub offset: usize,
    pub size: usize,
}

impl LoopDeviceInner {
    fn new(backing_file: Arc<OSInode>, offset: usize, size: usize) -> Self {
        Self {
            backing_file,
            offset,
            size,
        }
    }
    fn read_at(&self, block_id: usize ,buf: &mut [u8]) -> usize {
        let size = buf.len();
        self.backing_file.inode.read_at(self.offset + block_id * size, buf)
    }
    fn write_at(&self, block_id: usize, buf: &[u8]) -> usize {
        let size = buf.len();
        self.backing_file.inode.write_at(self.offset + block_id * size, buf)
    }
}