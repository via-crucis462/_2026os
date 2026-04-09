// 全局线程调度器
use crate::{CPU_CORE_NUM, process, sync::MPSafeCell};
#[cfg(target_arch = "riscv64")]
use crate::{arch::sbi::sbi_wakeup_harts};
use super::*;
use super::manager::*;
use lazy_static::*;
use alloc::{
    collections::vec_deque::VecDeque, sync::Arc, vec::Vec
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
        trace!("[kernel] Scheduler::get_pool");
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
    inner: VecDeque<Arc<TaskControlBlock>>,
}

impl TaskPool {
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }
    pub fn count(&self) -> usize {
        self.inner.len()
    }

    pub fn remove_task(&mut self, tid: usize) {
        self.inner.retain(|task| task.gettid() != tid);
    }

    pub fn add_task(&mut self, task: Arc<TaskControlBlock>) {
        let tid = task.gettid();
        if self.inner.iter().any(|t| t.gettid() == tid) {
            return;
        }
        self.inner.push_back(task);
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

        let Some(x) = self.inner.pop_front() else {
            //println!("[kernel] Scheduler::take_a_task: no task in pool");
            return None;
        };
        Some(x)
    }
    // 获取一份列表（注意会使引用计数+1）
    pub fn get_task_list(&self) -> VecDeque<Arc<TaskControlBlock>> {
        self.inner.clone()
    }
}

pub fn add_task_into_pool(task: Arc<TaskControlBlock>) {
    trace!("[kernel] Scheduler::add_task_into_pool: pid={}", task.getpid());
    remove_task_from_all_local_queues(task.gettid());
    let mut scheduler = SCHEDULER.exclusive_access();
    scheduler.get_pool().remove_task(task.gettid());
    scheduler.get_pool().add_task(task);
    drop(scheduler);
    //sbi_wakeup_harts(0b1111);
    trace!("add into pool finised");
}

pub fn remove_task_from_global_pool(tid: usize) {
    SCHEDULER.exclusive_access().get_pool().remove_task(tid);
}

pub fn ask_for_tasks() -> VecDeque<Arc<TaskControlBlock>> {
    //debug!("[kernel] Scheduler::ask_for_tasks");
    let list = SCHEDULER.exclusive_access().auto_get_task();
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
    
    let mut inner = task.inner_exclusive_access();
    // 只有处于阻塞状态的任务才需要被唤醒
    // (具体枚举名称请根据你项目里的定义替换，如 TaskStatus::Blocking)
    if matches!(inner.task_status, TaskStatus::Blocked) {
        inner.task_status = TaskStatus::Ready;
        drop(inner); // 🚩 极其重要：在调用 add_task_into_pool 前必须释放 task inner 的锁！
        
        // 重新塞回你的全局就绪池！
        crate::process::add_task_into_pool(task);
    }
}

