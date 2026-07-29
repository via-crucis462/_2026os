//! virtio_blk device driver

mod net;
mod sata_blk;

pub use sata_blk::*;


use crate::{ext4fs::BlockDevice};
use crate::drivers::DeviceType;
use lazy_static::*;
#[allow(unused)]
use crate::arch::drivers::pci;
use alloc::sync::Arc;
pub type BlockDeviceImpl = SataBlock;
pub type NetDeviceImpl = net::LA2k1000NetWrapper;

lazy_static! {
    /// The global block device driver instance: BLOCK_DEVICE with BlockDevice trait
    /// 已修改：从固定mmio地址改为扫描获取
    pub static ref BLOCK_DEVICE: Arc<BlockDeviceImpl> = {
        SATA_BLOCK.clone()
    };
    pub static ref NET_DEVICE: Arc<net::LA2k1000NetWrapper> = {
        debug!("NET_DEVICE lazy init: begin scan transport");
        debug!("NET_DEVICE lazy init: transport ready, build VirtIONetWrapper");
        unsafe {
            let net = Arc::new(net::LA2k1000NetWrapper::new());
            debug!("NET_DEVICE lazy init: done");
            net
        }

    };
}

use crate::drivers::block::block_cache::get_block_cache;
impl BlockDevice for SataBlock {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        if !self.read_block(block_id as u64, buf){
            error!("read block {} failed, stop. buf.len() = {}", block_id, buf.len());
            loop{}
        }
    }
    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        if !self.write_block(block_id as u64, buf){
            error!("write block {} failed, stop. buf.len() = {}", block_id, buf.len());
            loop{}
        }

    }
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; 4096] = block.get_ref(0);
        let len = buf.len();
        buf.copy_from_slice(&block_data[..len]);
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let len = buf.len();
        cache.lock().modify(0, |block_data: &mut [u8; 4096]| {
            block_data[..len].copy_from_slice(buf);
        });
    }
}

#[allow(unused)]
/// Test the block device
pub unsafe fn block_device_test() {
    let mut block_device = BLOCK_DEVICE.as_ref();
    let mut write_buffer = [0u8; 4096];
    let mut read_buffer = [0u8; 4096];
    for i in 0..512 {
        for byte in write_buffer.iter_mut() {
            *byte = i as u8;
        }
        let wres = block_device.write_block(i as u64, &write_buffer);
        let rres = block_device.read_block(i as u64, &mut read_buffer);
        trace!("Block {}: write_result={:?}, read_result={:?}", i, wres, rres);
        if write_buffer != read_buffer {
            error!(
                "block device test data mismatch at block {}; halting without syncing disks",
                i
            );
            loop {
                core::hint::spin_loop();
            }
        }
    }
    info!("block device test passed!");
}
