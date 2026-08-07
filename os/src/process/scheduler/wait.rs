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

/// 满足条件则阻塞，返回 false 表示未阻塞，返回 true 表示阻塞成功并且已经被唤醒
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

/// 无条件阻塞，放入指定等待队列
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

/// 带条件阻塞，放入指定等待队列
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

/// 在等待队列中原子检查、入队并注册单调时钟 deadline。
pub fn block_current_and_run_next_if_timeout<F>(
    queue: &Mutex<WaitQueue>,
    deadline_ns: usize,
    should_block: F,
) -> bool
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
    crate::process::scheduler::nanosleep::register_sleep_task(deadline_ns, task.clone());
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
    loop {
        let Some(task) = queue.lock().pop_front() else {
            return false;
        };
        // 跳过已经不在阻塞状态的任务
        // 上面弹出了，不再重新入队，自动drop
        loop {
            let state = task.inner_exclusive_access().state;
            if state != TaskStatus::BlockSaving {
                break;
            }
            core::hint::spin_loop();
        }
        let state = task.inner_exclusive_access().state;
        if matches!(state, TaskStatus::Blocked) {
            wake_up_task(task);
            return true;
        }
        warn!(
            "[wake_up_one] dropping non-blocked queue entry pid={} tid={} state={:?}",
            task.getpid(),
            task.gettid(),
            state,
        );
        // 非阻塞/已就绪/僵尸等陈旧条目：丢弃并继续找下一个真正的等待者。
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

/// 唤醒多核安全等待队列中的全部任务
pub(crate) fn wake_up_all_mp(queue: &MPSafeCell<WaitQueue>) -> usize {
    let tasks = {
        let mut guard = queue.exclusive_access();
        let mut tasks = alloc::vec::Vec::new();
        while let Some(task) = guard.pop_front() {
            tasks.push(task);
        }
        tasks
    };

    let mut count = 0;
    for task in tasks {
        while task.inner_exclusive_access().state == TaskStatus::BlockSaving {
            core::hint::spin_loop();
        }
        if matches!(task.inner_exclusive_access().state, TaskStatus::Blocked) {
            wake_up_task(task);
            count += 1;
        }
    }
    count
}
