//! Futex 等待队列管理结构
//! 
//! 将旧实现的物理地址作为键改为使用 FutexKey 作为键，主要避免以下情况：
//! A fork 出子进程 B，B 中有两个线程 b1，b2；
//! b1 对 ptr 调用 futex_wait，其中 ptr 是需要 COW 的私有映射页；
//! 内核按 ptr 只读翻译，得到 COW 前的物理地址 P，使用 P 作为键创建等待队列；
//! b2 对 ptr 调用 futex_wake；
//! 内核按 ptr 可写翻译，得到 COW 后的物理地址 P'，使用 P' 作为键发现找不到等待队列；
//! **b1 永远阻塞**

use crate::{mm::FutexKey, sync::WaitQueue};
use alloc::{collections::BTreeMap, sync::Arc};
use lazy_static::*;
use spin::Mutex;

lazy_static! {
	/// Stable futex identities to their corresponding wait queues.
	pub static ref FUTEX_WAIT_QUEUES: Mutex<BTreeMap<FutexKey, Arc<Mutex<WaitQueue>>>> =
		Mutex::new(BTreeMap::new());
}

/// Obtain the wait queue for one stable futex identity, creating it on demand.
pub(crate) fn get_futex_wait_queue(key: FutexKey) -> Arc<Mutex<WaitQueue>> {
	let mut queues = FUTEX_WAIT_QUEUES.lock();
	queues
		.entry(key)
		.or_insert_with(|| Arc::new(Mutex::new(WaitQueue::new())))
		.clone()
}

/// 最后一个队列成员醒来时释放掉队列
pub(crate) fn retire_futex_wait_queue_if_unused(key: FutexKey, queue: &Arc<Mutex<WaitQueue>>) {
	let mut queues = FUTEX_WAIT_QUEUES.lock();
	let Some(current) = queues.get(&key) else {
		return;
	};
	if Arc::ptr_eq(current, queue) && Arc::strong_count(current) == 2 {
		queues.remove(&key);
	}
}
