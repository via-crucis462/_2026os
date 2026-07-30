//! 异步 I/O 请求处理
//! 
//! 参考 Linux 的 bio 机制，使用一个内核线程来处理 I/O 请求

use alloc::sync::Arc;
use super::BlockDevice;

pub struct AsyncIoRequest {
    pub block_id: usize,
    pub buffer: [u8; 4096],
    pub is_write: bool, // true 表示写请求，false 表示读请求
    pub block_device: Arc<dyn BlockDevice>,
}