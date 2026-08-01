use crate::process::{TaskContext, TaskControlBlockInner, TaskStatus};
use crate::process::scheduler::processor::{current_task, schedule};
use crate::process::scheduler::runqueue::wake_up_task;
use crate::sync::{MPSafeCell, WaitQueue};
use spin::Mutex;

pub fn suspend_current_and_run_next() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
    task_inner.state = TaskStatus::Ready;
    drop(task_inner);
    drop(task);
    schedule(task_cx_ptr);
}
// 无条件阻塞
pub fn block_current_and_run_next_if_task<F>(should_block: F) -> bool
where
    F: FnOnce(&TaskControlBlockInner) -> bool,
{
    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        if !should_block(&task_inner) {
            return false;
        }
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        ptr
    };
    drop(task);
    schedule(task_cx_ptr);
    true
}

pub fn block_current_and_run_next(queue: &Mutex<WaitQueue>) {
    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        drop(task_inner);
        let mut guard = queue.lock();
        guard.push_back(task);
        ptr
    };
    schedule(task_cx_ptr);
}

pub fn block_current_and_run_next_if<F>(queue: &Mutex<WaitQueue>, should_block: F) -> bool
where
    F: FnOnce() -> bool,
{
    let mut guard = queue.lock();
    if !should_block() {
        return false;
    }

    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        ptr
    };
    guard.push_back(task);
    drop(guard);
    schedule(task_cx_ptr);
    true
}

/// 在多核安全等待队列锁内检查条件并原子入队。
pub(crate) fn block_current_and_run_next_if_mp<F>(
    queue: &MPSafeCell<WaitQueue>,
    should_block: F,
) -> bool
where
    F: FnOnce() -> bool,
{
    let mut guard = queue.exclusive_access();
    if !should_block() {
        return false;
    }

    let task = current_task().unwrap();
    let task_cx_ptr = {
        let mut task_inner = task.inner_exclusive_access();
        let ptr = &mut task_inner.thread.task_ctx as *mut TaskContext;
        task_inner.state = TaskStatus::BlockSaving;
        ptr
    };
    guard.push_back(task);
    drop(guard);
    schedule(task_cx_ptr);
    true
}

pub fn wake_up_one(queue: &Mutex<WaitQueue>) -> bool {
    if let Some(task) = queue.lock().pop_front() {
        while task.inner_exclusive_access().state == TaskStatus::BlockSaving {
            //println!("wake_up_one: task is still saving context");
            core::hint::spin_loop();
            //println!("wake_up_one: rechecking task status...");
        }
		wake_up_task(task);
        true
    } else {
        false
    }
}

/// 唤醒等待队列中的全部任务；子进程状态变化时所有 wait 系统调用都需重新检查过滤条件。
pub fn wake_up_all(queue: &Mutex<WaitQueue>) -> usize {
    let mut count = 0;
    while wake_up_one(queue) {
        count += 1;
    }
    count
}

/// 唤醒多核安全等待队列中的全部任务。
pub(crate) fn wake_up_all_mp(queue: &MPSafeCell<WaitQueue>) -> usize {
    let tasks = {
        let mut guard = queue.exclusive_access();
        let mut tasks = alloc::vec::Vec::new();
        while let Some(task) = guard.pop_front() {
            tasks.push(task);
        }
        tasks
    };

    let count = tasks.len();
    for task in tasks {
        while task.inner_exclusive_access().state == TaskStatus::BlockSaving {
            core::hint::spin_loop();
        }
        wake_up_task(task);
    }
    count
}
