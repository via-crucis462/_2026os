use crate::process::{TaskContext, TaskStatus};
use crate::process::scheduler::processor::{current_task, schedule};
use crate::process::scheduler::runqueue::add_task_into_pool;
use crate::sync::WaitQueue;
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

pub fn wake_up_one(queue: &Mutex<WaitQueue>) -> bool {
    if let Some(task) = queue.lock().pop_front() {
        while task.inner_exclusive_access().state == TaskStatus::BlockSaving {
            println!("wake_up_one: task is still saving context");
            core::hint::spin_loop();
            println!("wake_up_one: rechecking task status...");
        }
        let mut task_inner = task.inner_exclusive_access();
        task_inner.state = TaskStatus::Ready;
        drop(task_inner);
        add_task_into_pool(task);
        true
    } else {
        false
    }
}
