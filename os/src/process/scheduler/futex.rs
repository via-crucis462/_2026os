//! Futex 等待队列管理结构。

use crate::sync::WaitQueue;
use alloc::{collections::BTreeMap, sync::Arc};
use lazy_static::*;
use spin::Mutex;

lazy_static! {
	/// Futex 物理地址到对应等待队列的映射。
	pub static ref FUTEX_WAIT_QUEUES: Mutex<BTreeMap<usize, Arc<Mutex<WaitQueue>>>> =
		Mutex::new(BTreeMap::new());
}

/// 获取 Futex 地址对应的等待队列；队列不存在时按原逻辑创建。
pub(crate) fn get_futex_wait_queue(uaddr: usize) -> Arc<Mutex<WaitQueue>> {
	let mut queues = FUTEX_WAIT_QUEUES.lock();
	queues
		.entry(uaddr)
		.or_insert_with(|| Arc::new(Mutex::new(WaitQueue::new())))
		.clone()
}

/// 调试用：打印所有 futex 等待队列的长度。
pub fn debug_print_futex_queues() {
	let queues = FUTEX_WAIT_QUEUES.lock();
	if queues.is_empty() {
		println!("[FUTEX-DBG] no futex queues");
		return;
	}
	for (key, queue) in queues.iter() {
		let guard = queue.lock();
		let len = guard.len();
		if len > 0 {
			let tids = guard.get_tids();
			println!(
				"[FUTEX-DBG] key={:#x} waiters={} tids={:?}",
				key, len, tids
			);
		}
	}
}
