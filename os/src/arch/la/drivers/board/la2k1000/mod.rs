//! virtio_blk device driver

mod net;
mod sata_blk;

pub use sata_blk::*;

#[allow(unused)]
use crate::arch::drivers::pci;
use crate::drivers::DeviceType;
use crate::ext4fs::BlockDevice;
use alloc::sync::Arc;
use lazy_static::*;
pub type BlockDeviceImpl = SataBlock;
pub type NetDeviceImpl = net::LA2k1000NetWrapper;

use crate::drivers::block::block_cache::get_block_cache;
impl BlockDevice for SataBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        if !self.read_block(block_id as u64, buf) {
            error!(
                "read block {} failed, stop. buf.len() = {}",
                block_id,
                buf.len()
            );
            loop {}
        }
    }
    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        if !self.write_block(block_id as u64, buf) {
            error!(
                "write block {} failed, stop. buf.len() = {}",
                block_id,
                buf.len()
            );
            loop {}
        }
    }
}
