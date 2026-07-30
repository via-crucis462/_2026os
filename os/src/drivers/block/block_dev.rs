use core::any::Any;
use crate::drivers::block::cache::get_data_block_cache;
use crate::ext4fs::get_block_cache;

use crate::ext4fs::BLOCK_SZ;
use super::BLOCK_DEVICE;

pub trait BlockDevice: Send + Sync + Any {
    // 这里函数名是否有"data"只影响缓存策略
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; BLOCK_SZ] = block.get_ref(0);
        let len = buf.len();
        buf.copy_from_slice(&block_data[..len]);
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        let len = buf.len();
        cache.lock().modify(0, |block_data: &mut [u8; BLOCK_SZ]| {
            block_data[..len].copy_from_slice(buf);
        });
    }
    fn read_data_block(&self, block_id: usize, buf: &mut [u8]) {
        let cache = get_data_block_cache(block_id, BLOCK_DEVICE.clone());
        let block = cache.lock();
        let block_data: &[u8; BLOCK_SZ] = block.get_ref(0);
        let len = buf.len();
        buf.copy_from_slice(&block_data[..len]);
    }
    fn write_data_block(&self, block_id: usize, buf: &[u8]) {
        let cache = get_data_block_cache(block_id, BLOCK_DEVICE.clone());
        let len = buf.len();
        cache.lock().modify(0, |block_data: &mut [u8; BLOCK_SZ]| {
            block_data[..len].copy_from_slice(buf);
        });
    }
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]);
    fn raw_write_block(&self, block_id: usize, buf: &[u8]);
}
