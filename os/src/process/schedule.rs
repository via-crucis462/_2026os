// 全局线程调度器
use crate::{process, sync::MPSafeCell};
use super::*;
use super::manager::*;
use lazy_static::*;
use alloc::{
    vec::Vec,
    sync::Arc,
};
// 线程调度器
lazy_static! {
    pub static ref SCHEDULER: MPSafeCell<Scheduler> = MPSafeCell::new(Scheduler {
        task_pool: TaskPool::new(),
    });
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
        debug!("[kernel] Scheduler::get_pool");
        &mut self.task_pool
    }
    pub fn auto_get_task(&mut self) -> Vec<Arc<TaskControlBlock>> {
        let mut total_num = core::cmp::max(self.get_task_count(), 1);
        let mut list = Vec::new();
        while let Some(task) = self.task_pool.take_a_task() {
            list.push(task);
            total_num -= 1;
            if total_num == 0 {
                break;
            }
        }
        list
    }
    pub fn get_task_count(&self) -> usize {
        self.task_pool.count() + task_count_in_mng()
    }
}


pub struct TaskPool {
    inner: Vec<Arc<TaskControlBlock>>,
}

impl TaskPool {
    pub fn new() -> Self {
        Self {
            inner: Vec::new(),
        }
    }
    pub fn count(&self) -> usize {
        self.inner.len()
    }
    pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
        self.inner.push(task);
    }
    // 获取一个线程的引用
    pub fn get_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
        tid2task(tid)
    }
    // 拿出一个线程
    pub fn take_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
        if let Some(task) = tid2task(tid) {
            // 从池中移除
            self.inner.retain(|t| t.gettid() != tid);
            Some(task)
        } else {
            None
        }
    }
    // 随机拿出一个线程
    pub fn take_a_task(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.inner.pop()
    }
    // 获取一份列表（注意会使引用计数+1）
    pub fn get_task_list(&self) -> Vec<Arc<TaskControlBlock>> {
        self.inner.clone()
    }
}

pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
    debug!("[kernel] Scheduler::add_task_into_pool: pid={}", task.getpid());
    let mut scheduler = SCHEDULER.exclusive_access();
    scheduler.get_pool().add_task(task);
    drop(scheduler);
    debug!("add into pool finised");
}

pub fn ask_for_tasks() -> Vec<Arc<TaskControlBlock>> {
    //debug!("[kernel] Scheduler::ask_for_tasks");
    let list = SCHEDULER.exclusive_access().auto_get_task();
    //debug!("[kernel] Scheduler::ask_for_tasks: got {} tasks", list.len());
    list
}

pub fn get_task_count() -> usize {
    SCHEDULER.exclusive_access().get_pool().count() + task_count_in_mng()
}