//! 虚拟文件系统的inode号管理
 
use crate::drivers::block::BLOCK_DEVICE;
use crate::ext4fs::superblock::{Ext4SuperBlockDisk, Ext4SuperBlock};

use lazy_static::lazy_static;


use core::sync::atomic::{AtomicU64, Ordering};

lazy_static! {
    /// 用于虚拟文件的ino分配，只增不减，没有回收机制
    pub static ref INO_COUNTER: AtomicU64 = AtomicU64::new(get_disk_ino_max() + 1);
}

/// 读取磁盘inode号上限（从超级块中）
pub fn get_disk_ino_max() -> u64 {
    let disk_sb = Ext4SuperBlockDisk::new(BLOCK_DEVICE.clone());
    let sb = Ext4SuperBlock::new(disk_sb);
    sb.total_inodes as u64
}

/// 分配下一个可用的inode号
pub fn get_next_ino() -> u64 {
    // 先获取当前的值，然后加1，返回旧值
    // 返回的是+1前的旧值
    let ino = INO_COUNTER.fetch_add(1, Ordering::SeqCst);
    // 理论有可能，但实际不出bug应该达不到
    if ino == u64::MAX {
        panic!("Inode number overflow");
    }
    ino
}
