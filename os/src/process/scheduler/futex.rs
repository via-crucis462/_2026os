//! Futex 等待队列管理

use crate::mm::FutexKey;
use crate::process::{
    current_task, schedule, TaskContext, TaskControlBlock, TaskStatus,
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
use lazy_static::*;
use spin::Mutex;

pub(crate) struct FutexWaiter {
    task: Arc<TaskControlBlock>,
    bitset: u32,
    /// 仅允许在持有目标队列锁时设置或清除（移动则需要同时持有源和目标队列的锁）（读取不限），
    /// 保证队列内容和任务 loc 的一致性（操作的原子化）。
    location: Mutex<Option<FutexWaiterLocation>>,
}

#[derive(Clone)]
struct FutexWaiterLocation {
    key: FutexKey,
    queue: Weak<Mutex<FutexWaitQueue>>,
}

/// 移除信息
/// 
/// queue: 任务被移除前所在的队列
pub(crate) struct FutexWaiterRemoval {
    pub(crate) key: FutexKey,
    pub(crate) queue: Arc<Mutex<FutexWaitQueue>>,
}

impl FutexWaiter {
    fn new(task: Arc<TaskControlBlock>, bitset: u32) -> Self {
        Self {
            task,
            bitset,
            location: Mutex::new(None),
        }
    }
    /// 当任务被加入等待队列时设置其 loc
    /// 
    /// 规定：必须先持有希望设置的目标队列锁，才能调用此函数
    /// 用于保证访问队列时不会观察到队列中的任务 loc 和队列本身不一致的情况
    fn set_location_while_queued(
        &self,
        key: FutexKey,
        queue: &Arc<Mutex<FutexWaitQueue>>,
    ) {
        *self.location.lock() = Some(FutexWaiterLocation {
            key,
            queue: Arc::downgrade(queue),
        });
    }
    /// 当任务被从等待队列中移除时清除其 loc
    /// 
    /// 规定：必须先持有希望移除的目标队列锁，才能调用此函数
    /// 用于保证访问队列时不会观察到队列中的任务 loc 和队列本身不一致的情况
    fn clear_location_while_queued(&self) {
        *self.location.lock() = None;
    }
    fn location(&self) -> Option<FutexWaiterLocation> {
        self.location.lock().clone()
    }
}

pub(crate) struct FutexWaitQueue {
    queue: VecDeque<Arc<FutexWaiter>>,
}

impl FutexWaitQueue {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    fn push_back(&mut self, waiter: Arc<FutexWaiter>) {
        self.queue.push_back(waiter);
    }

    fn pop_matching(&mut self, bitset: u32) -> Option<Arc<FutexWaiter>> {
        let index = self
            .queue
            .iter()
            .position(|waiter| waiter.bitset & bitset != 0)?;
        self.queue.remove(index)
    }

    fn pop_front(&mut self) -> Option<Arc<FutexWaiter>> {
        self.queue.pop_front()
    }

    fn remove_waiter(&mut self, waiter: &Arc<FutexWaiter>) -> bool {
        let Some(index) = self
            .queue
            .iter()
            .position(|queued| Arc::ptr_eq(queued, waiter))
        else {
            return false;
        };
        self.queue.remove(index);
        true
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn tids(&self) -> Vec<usize> {
        self.queue.iter().map(|waiter| waiter.task.gettid()).collect()
    }
}

lazy_static! {
    pub(crate) static ref FUTEX_WAIT_QUEUES:
        Mutex<BTreeMap<FutexKey, Arc<Mutex<FutexWaitQueue>>>> = Mutex::new(BTreeMap::new());
}

/// 当队列没有被使用时移除
///
/// 队列为空且不被别处引用（2 一个是调用者的Arc，一个是全局 map 的 Arc）
pub(crate) fn retire_futex_wait_queue_if_unused(
    key: FutexKey,
    queue: &Arc<Mutex<FutexWaitQueue>>,
) {
    let mut queues = FUTEX_WAIT_QUEUES.lock();
    let Some(current) = queues.get(&key) else {
        return;
    };
    if !Arc::ptr_eq(current, queue) || Arc::strong_count(current) != 2 {
        return;
    }
    if current.lock().is_empty() {
        queues.remove(&key);
    }
}

/// 当队列没有被使用时移除
///
/// 队列为空且不被别处引用（2 一个是调用者的Arc，一个是全局 map 的 Arc）
pub(crate) fn retire_futex_wait_queue_by_key_if_unused(key: FutexKey) {
    let mut queues = FUTEX_WAIT_QUEUES.lock();
    let Some(current) = queues.get(&key).cloned() else {
        return;
    };
    if Arc::strong_count(&current) == 2 && current.lock().is_empty() {
        queues.remove(&key);
    }
}

/// 原子化的带条件阻塞
///
/// 用于 futex 入队前的锁内二次检查
pub(crate) fn block_current_on_futex_if<F>(
    key: FutexKey,
    bitset: u32,
    deadline_ns: Option<usize>,
    should_block: F,
) -> Option<(Arc<FutexWaiter>, Arc<Mutex<FutexWaitQueue>>)>
where
    F: FnOnce() -> bool,
{
    let mut queues = FUTEX_WAIT_QUEUES.lock();
    let queue = queues
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(FutexWaitQueue::new())))
        .clone();
    let mut waiters = queue.lock();
    if !should_block() {
        let is_empty = waiters.is_empty();
        drop(waiters);
        if is_empty {
            queues.remove(&key);
        }
        return None;
    }

    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut inner = task.inner_exclusive_access();
        let ptr = &mut inner.thread.task_ctx as *mut TaskContext;
        inner.wake_pending = false;
        inner.state = TaskStatus::BlockSaving;
        ptr
    };
    if let Some(deadline_ns) = deadline_ns {
        crate::process::scheduler::nanosleep::register_sleep_task(deadline_ns, task.clone());
    }
    let waiter = Arc::new(FutexWaiter::new(task, bitset));
    waiter.set_location_while_queued(key, &queue);
    waiters.push_back(waiter.clone());
    drop(waiters);
    drop(queues);
    schedule(task_cx_ptr);
    Some((waiter, queue))
}

/// 唤醒 count 个匹配的等待者，返回实际唤醒的数量
pub(crate) fn wake_futex_waiters(
    queue: &Arc<Mutex<FutexWaitQueue>>,
    count: usize,
    bitset: u32,
) -> usize {
    let mut waiters = Vec::new();
    {
        let mut guard = queue.lock();
        while waiters.len() < count {
            let Some(waiter) = guard.pop_matching(bitset) else {
                break;
            };
            waiter.clear_location_while_queued();
            waiters.push(waiter);
        }
    }

    // 定义唤醒任务数摘下的任务数量，后续唤醒前可能有任务被别的路径已经唤醒，同样视为唤醒成功
    let woken = waiters.len();
    for waiter in waiters {
        let _ = crate::process::scheduler::runqueue::wake_up_task(waiter.task.clone());
    }
    woken
}

/// 唤醒 wake_count 个等待者，并将 requeue_count 个等待者从 src_key 队列移动到 dst_key 队列
/// 
/// 约定锁序为按照目标队列 mutex 地址升序锁定，避免循环等待死锁
pub(crate) fn requeue_futex_waiters<F>(
    src_key: FutexKey,
    dst_key: FutexKey,
    wake_count: usize,
    requeue_count: usize,
    mut should_requeue: F,
) -> Result<usize, ()>
where
    F: FnMut() -> bool,
{
    let mut wake_waiters = Vec::new();
    let mut moved = 0;

    // 先拿全局 map 锁再按固定锁序获取队列锁
    let mut queues = FUTEX_WAIT_QUEUES.lock();
    let Some(src_queue) = queues.get(&src_key).cloned() else {
        return if should_requeue() { Ok(0) } else { Err(()) };
    };
    let dst_queue = if requeue_count > 0 {
        Some(
            queues
                .entry(dst_key)
                .or_insert_with(|| Arc::new(Mutex::new(FutexWaitQueue::new())))
                .clone(),
        )
    } else {
        None
    };

    if let Some(dst_queue) = dst_queue.as_ref() {
        if Arc::ptr_eq(&src_queue, dst_queue) {
            let mut src = src_queue.lock();
            if !should_requeue() {
                return Err(());
            }
            for _ in 0..wake_count {
                let Some(waiter) = src.pop_front() else {
                    break;
                };
                waiter.clear_location_while_queued();
                wake_waiters.push(waiter);
            }
            // 在同一个队列中移动等待者可以什么都不干，但宣称已经移动了这么多
            moved = core::cmp::min(requeue_count, src.len());
        } else if Arc::as_ptr(&src_queue) as usize <= Arc::as_ptr(dst_queue) as usize {
            let mut src = src_queue.lock();
            let mut dst = dst_queue.lock();
            if !should_requeue() {
                return Err(());
            }
            for _ in 0..wake_count {
                let Some(waiter) = src.pop_front() else {
                    break;
                };
                waiter.clear_location_while_queued();
                wake_waiters.push(waiter);
            }
            for _ in 0..requeue_count {
                let Some(waiter) = src.pop_front() else {
                    break;
                };
                waiter.set_location_while_queued(dst_key, dst_queue);
                dst.push_back(waiter);
                moved += 1;
            }
        } else {
            let mut dst = dst_queue.lock();
            let mut src = src_queue.lock();
            if !should_requeue() {
                return Err(());
            }
            for _ in 0..wake_count {
                let Some(waiter) = src.pop_front() else {
                    break;
                };
                waiter.clear_location_while_queued();
                wake_waiters.push(waiter);
            }
            for _ in 0..requeue_count {
                let Some(waiter) = src.pop_front() else {
                    break;
                };
                waiter.set_location_while_queued(dst_key, dst_queue);
                dst.push_back(waiter);
                moved += 1;
            }
        }
    } else {
        let mut src = src_queue.lock();
        if !should_requeue() {
            return Err(());
        }
        for _ in 0..wake_count {
            let Some(waiter) = src.pop_front() else {
                break;
            };
            waiter.clear_location_while_queued();
            wake_waiters.push(waiter);
        }
    }
    drop(queues);
    // 在锁外唤醒收集到的待唤醒任务
    let woken = wake_waiters.len();
    for waiter in wake_waiters {
        let _ = crate::process::scheduler::runqueue::wake_up_task(waiter.task.clone());
    }
    Ok(woken + moved)
}

/// 从全局 futex 队列中移除指定的等待者，如果成功则返回其移除前所在的队列和 key
pub(crate) fn remove_futex_waiter(
    waiter: &Arc<FutexWaiter>,
) -> Option<FutexWaiterRemoval> {
    loop {
        let Some(location) = waiter.location() else {
            return None;
        };
        let Some(queue) = location.queue.upgrade() else {
            // 目标任务已经被唤醒并且队列被 retire
            return None;
        };

        let mut guard = queue.lock();
        if guard.remove_waiter(waiter) {
            waiter.clear_location_while_queued();
            drop(guard);
            return Some(FutexWaiterRemoval {
                key: location.key,
                queue,
            });
        }
        drop(guard);

        let Some(current) = waiter.location() else {
            return None;
        };
        assert!(current.key != location.key || !current.queue.ptr_eq(&location.queue),
            "futex waiter location points to a queue that does not contain it");
        // 两次检查期间任务可能被移动到另一个队列
    }
}

pub fn debug_print_futex_queues() {
    let queues = FUTEX_WAIT_QUEUES.lock();
    if queues.is_empty() {
        println!("[FUTEX-DBG] no futex queues");
        return;
    }
    for (key, queue) in queues.iter() {
        let guard = queue.lock();
        if !guard.is_empty() {
            println!(
                "[FUTEX-DBG] key={:?} waiters={} tids={:?}",
                key,
                guard.len(),
                guard.tids()
            );
        }
    }
}
