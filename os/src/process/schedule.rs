// 全局线程调度器
use core::cmp::Ordering;

use crate::{CPU_CORE_NUM, arch::config::CLOCK_FREQ, arch::timer::get_timer_ticks, process, sync::MPSafeCell};
#[cfg(target_arch = "riscv64")]
use crate::{arch::sbi::sbi_wakeup_harts};
use super::manager::{SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_RR};
use super::*;
use super::manager::*;
use lazy_static::*;
use alloc::{
    collections::{BinaryHeap, vec_deque::VecDeque}, sync::Arc, vec::Vec
};
// 线程调度器
lazy_static! {
    pub static ref SCHEDULER: MPSafeCell<Scheduler> = MPSafeCell::new(Scheduler {
        task_pool: TaskPool::new(),
    });
    static ref SLEEP_QUEUE: MPSafeCell<SleepQueue> = MPSafeCell::new(SleepQueue::new());
    pub static ref SCHED_DISPATCH_LOCK: MPSafeCell<()> = MPSafeCell::new(());
}

pub fn lock_dispatch() -> crate::sync::MPSafeGuard<'static, ()> {
    SCHED_DISPATCH_LOCK.exclusive_access()
}

pub struct Scheduler {
    pub task_pool: TaskPool,
}

impl Scheduler {
    pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
        //debug!("[kernel] Scheduler::add_task: pid={}, tid={}", task.getpid(), task.gettid());
        self.task_pool.add_task(task);
    }
    pub fn get_pool(&mut self) -> &mut TaskPool {
        //trace!("[kernel] Scheduler::get_pool");
        &mut self.task_pool
    }
    pub fn auto_get_task(&mut self) -> VecDeque<Arc<TaskControlBlock>> {
        // Do not depend on per-core queue locks here; this function is called
        // while other scheduling locks may already be held.
        let mut total_num = self.task_pool.count().saturating_add(1);
        let mut list = VecDeque::new();
        while let Some(task) = self.task_pool.take_a_task() {
            list.push_back(task);
            total_num -= 1;
            if total_num == 0 {
                break;
            }
        }
        list
    }
    pub fn get_task_count(&self) -> usize {
        // Sum lengths of per-core ready queues to reflect current per-core workload
        let mut sum = 0;
        for i in 0..CPU_CORE_NUM {
            sum += TASK_MANAGERS[i].exclusive_access().task_count();
        }
        sum
    }
}


pub struct TaskPool {
    deadline: BinaryHeap<PoolEntry>,
    realtime: BinaryHeap<PoolEntry>,
    fair: BinaryHeap<PoolEntry>,
    idle: BinaryHeap<PoolEntry>,
    enqueue_order: usize,
}

struct PoolEntry {
    class: u8,
    priority: i32,
    order: usize,
    task: Arc<TaskControlBlock>,
}

struct SleepEntry {
    deadline_ns: usize,
    order: usize,
    task: Arc<TaskControlBlock>,
}

impl PartialEq for SleepEntry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline_ns == other.deadline_ns
            && self.order == other.order
            && Arc::ptr_eq(&self.task, &other.task)
    }
}

impl Eq for SleepEntry {}

impl Ord for SleepEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_ptr = Arc::as_ptr(&self.task) as usize;
        let other_ptr = Arc::as_ptr(&other.task) as usize;
        other.deadline_ns
            .cmp(&self.deadline_ns)
            .then_with(|| other.order.cmp(&self.order))
            .then_with(|| self_ptr.cmp(&other_ptr))
    }
}

impl PartialOrd for SleepEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct SleepQueue {
    inner: BinaryHeap<SleepEntry>,
    enqueue_order: usize,
}

impl SleepQueue {
    fn new() -> Self {
        Self {
            inner: BinaryHeap::new(),
            enqueue_order: 0,
        }
    }

    fn push(&mut self, deadline_ns: usize, task: Arc<TaskControlBlock>) {
        self.inner.retain(|entry| entry.task.gettid() != task.gettid());
        let entry = SleepEntry {
            deadline_ns,
            order: self.enqueue_order,
            task,
        };
        self.enqueue_order = self.enqueue_order.wrapping_add(1);
        self.inner.push(entry);
    }

    fn pop_expired(&mut self, now_ns: usize) -> Option<Arc<TaskControlBlock>> {
        if self.inner.peek().map_or(false, |entry| entry.deadline_ns <= now_ns) {
            self.inner.pop().map(|entry| entry.task)
        } else {
            None
        }
    }
}

impl PartialEq for PoolEntry {
    fn eq(&self, other: &Self) -> bool {
        self.class == other.class
            && self.priority == other.priority
            && self.order == other.order
            && Arc::ptr_eq(&self.task, &other.task)
    }
}

impl Eq for PoolEntry {}

impl Ord for PoolEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_ptr = Arc::as_ptr(&self.task) as usize;
        let other_ptr = Arc::as_ptr(&other.task) as usize;
        self.class
            .cmp(&other.class)
            .then_with(|| self.priority.cmp(&other.priority))
            .then_with(|| other.order.cmp(&self.order))
            .then_with(|| self_ptr.cmp(&other_ptr))
    }
}

impl PartialOrd for PoolEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn task_sched_rank(task: &Arc<TaskControlBlock>) -> (u8, i32) {
    let inner = task.inner_exclusive_access();
    match inner.sched_policy {
        SCHED_FIFO | SCHED_RR if inner.sched_priority > 0 => (2, inner.sched_priority),
        SCHED_IDLE => (0, 0),
        SCHED_BATCH => (1, 0),
        _ => (1, 0),
    }
}

fn monotonic_now_ns() -> usize {
    let ns = (get_timer_ticks() as u128)
        .saturating_mul(1_000_000_000)
        / CLOCK_FREQ as u128;
    ns.min(usize::MAX as u128) as usize
}

pub fn sleep_current_until(deadline_ns: usize) {
    let task = take_current_task().unwrap();
    let task_cx_ptr = {
        let mut inner = task.inner_exclusive_access();
        let ptr = &mut inner.task_cx as *mut TaskContext;
        inner.task_status = TaskStatus::Blocked;
        ptr
    };
    SLEEP_QUEUE.exclusive_access().push(deadline_ns, task);
    schedule(task_cx_ptr);
}

pub fn wake_expired_sleep_tasks() {
    loop {
        let task = {
            let now_ns = monotonic_now_ns();
            SLEEP_QUEUE.exclusive_access().pop_expired(now_ns)
        };
        let Some(task) = task else {
            break;
        };

        let mut inner = task.inner_exclusive_access();
        if matches!(inner.task_status, TaskStatus::Blocked) {
            inner.task_status = TaskStatus::Ready;
            drop(inner);
            SCHEDULER.exclusive_access().get_pool().add_task(task);
        }
    }
}

impl TaskPool {
    pub fn new() -> Self {
        Self {
            deadline: BinaryHeap::new(),
            realtime: BinaryHeap::new(),
            fair: BinaryHeap::new(),
            idle: BinaryHeap::new(),
            enqueue_order: 0,
        }
    }
    pub fn count(&self) -> usize {
        self.deadline.len() + self.realtime.len() + self.fair.len() + self.idle.len()
    }

    pub fn remove_task(&mut self, tid: usize) {
        self.deadline.retain(|entry| entry.task.gettid() != tid);
        self.realtime.retain(|entry| entry.task.gettid() != tid);
        self.fair.retain(|entry| entry.task.gettid() != tid);
        self.idle.retain(|entry| entry.task.gettid() != tid);
    }

    pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
        let tid = task.gettid();
        if self.contains_task(tid) {
            return;
        }
        let (class, priority) = task_sched_rank(&task);
        let entry = PoolEntry {
            class,
            priority,
            order: self.enqueue_order,
            task,
        };
        self.enqueue_order = self.enqueue_order.wrapping_add(1);
        match class {
            2 => self.realtime.push(entry),
            0 => self.idle.push(entry),
            _ => self.fair.push(entry),
        }
    }
    // 获取一个线程的引用
    pub fn get_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
        tid2task(tid)
    }
    // 拿出一个线程
    pub fn take_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
        if let Some(task) = tid2task(tid) {
            // 从池中移除
            self.remove_task(tid);
            Some(task)
        } else {
            None
        }
    }
    // 拿出优先级最高的线程；同优先级保持入队顺序
    pub fn take_a_task(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.deadline.pop().or_else(|| self.realtime.pop())
            .or_else(|| self.fair.pop())
            .or_else(|| self.idle.pop())
            .map(|entry| entry.task)
    }
    // 获取一份列表（注意会使引用计数+1）
    pub fn get_task_list(&self) -> VecDeque<Arc<TaskControlBlock>> {
        self.deadline.iter()
            .chain(self.realtime.iter())
            .chain(self.fair.iter())
            .chain(self.idle.iter())
            .map(|entry| Arc::clone(&entry.task))
            .collect()
    }

    fn contains_task(&self, tid: usize) -> bool {
        self.deadline.iter()
            .chain(self.realtime.iter())
            .chain(self.fair.iter())
            .chain(self.idle.iter())
            .any(|entry| entry.task.gettid() == tid)
    }
}

pub(crate) fn add_task_into_pool_unlocked(task: Arc<TaskControlBlock>) {
    //trace!("[kernel] Scheduler::add_task_into_pool: pid={}", task.getpid());
    remove_task_from_all_local_queues_unlocked(task.gettid());
    let mut scheduler = SCHEDULER.exclusive_access();
    scheduler.get_pool().remove_task(task.gettid());
    scheduler.get_pool().add_task(task);
    drop(scheduler);
    //sbi_wakeup_harts(0b1111);
    //trace!("add into pool finised");
}

pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
    let _dispatch = lock_dispatch();
    add_task_into_pool_unlocked(task);
}

pub(crate) fn remove_task_from_global_pool_unlocked(tid: usize) {
    SCHEDULER.exclusive_access().get_pool().remove_task(tid);
}

pub fn remove_task_from_global_pool(tid: usize) {
    let _dispatch = lock_dispatch();
    remove_task_from_global_pool_unlocked(tid);
}

pub fn ask_for_task() -> Option<Arc<TaskControlBlock>> {
    wake_expired_sleep_tasks();
    SCHEDULER.exclusive_access().get_pool().take_a_task()
}

pub fn ask_for_tasks() -> VecDeque<Arc<TaskControlBlock>> {
    //debug!("[kernel] Scheduler::ask_for_tasks");
    let mut list = VecDeque::new();
    if let Some(task) = ask_for_task() {
        list.push_back(task);
    }
    //debug!("[kernel] Scheduler::ask_for_tasks: got {} tasks", list.len());
    list
}

pub fn get_task_count() -> usize {
    // Return total length of per-core ready queues.
    let mut sum = 0usize;
    for i in 0..CPU_CORE_NUM {
        sum += TASK_MANAGERS[i].exclusive_access().task_count();
    }
    sum
}

pub fn wake_up_task(task: Arc<TaskControlBlock>) {
    trace!("[kernel] wake_up_task: pid={}", task.getpid());
    let _dispatch = lock_dispatch();
    let mut inner = task.inner_exclusive_access();
    // 只有处于阻塞状态的任务才需要被唤醒
    // (具体枚举名称请根据你项目里的定义替换，如 TaskStatus::Blocking)
    if matches!(inner.task_status, TaskStatus::Blocked) {
        inner.task_status = TaskStatus::Ready;
        //inner.owner_hart = None;
        drop(inner); 
        
        // 重新塞回你的全局就绪池！
        add_task_into_pool_unlocked(task);
    }
}

