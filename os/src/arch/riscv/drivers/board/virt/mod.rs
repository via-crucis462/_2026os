//! qemu virt 板级驱动配置

mod virtio_blk;
mod virtio_net;

use crate::ext4fs::BlockDevice;
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;

use virtio_blk::VirtIOBlock;
use virtio_net::VirtIONetWrapper;

pub type BlockDeviceImpl = VirtIOBlock;
pub type NetDeviceImpl = VirtIONetWrapper;

use crate::drivers::block::block_cache::get_block_cache;
use crate::ext4fs::BLOCK_SZ as BLOCK_SZ;

impl BlockDevice for VirtIOBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len(), BLOCK_SZ, "block buffer must be {} bytes", BLOCK_SZ);
        self.inner
            .exclusive_access()
            .read_blocks(block_id * (BLOCK_SZ / 512), buf)
            .expect("virtio block read failed");
    }

    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(buf.len(), BLOCK_SZ, "block buffer must be {} bytes", BLOCK_SZ);
        self.inner
            .exclusive_access()
            .write_blocks(block_id * (BLOCK_SZ / 512), buf)
            .expect("virtio block write failed");
    }
}