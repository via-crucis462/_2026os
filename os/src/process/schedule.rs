use crate::{sync::MPSafeCell, task::processor::{self, PROCESSOR}};

use super::{TaskControlBlock};
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
    pub fn get_pool(&mut self) -> &mut TaskPool {
        &mut self.task_pool
    }
    pub fn auto_get_task(&mut self) -> Vec<Arc<TaskControlBlock>> {
        let mut total_num = 0;
        let mut list = Vec::new();
        get_task_count();
        while let Some(task) = self.task_pool.take_a_task() {
            list.push(task);
            total_num -= 1;
            if total_num == 0 {
                break;
            }
        }
        list
        
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
        self.inner.iter().find(|task| {
            task.tid.0 == tid
        }).cloned()
    }
    // 拿出一个线程
    pub fn take_task(&mut self, tid: usize) -> Option<Arc<TaskControlBlock>> {
        if let Some(pos) = self.inner.iter().position(|task| task.tid.0 == tid) {
            Some(self.inner.remove(pos))
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

pub fn add_task_to_pool(task: Arc<TaskControlBlock>) {
    SCHEDULER.exclusive_access().get_pool().add_task(task);
}

pub fn ask_for_task() -> Vec<Arc<TaskControlBlock>> {
    let list = Vec::new();
    list
}
