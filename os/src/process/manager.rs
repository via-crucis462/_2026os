//! Implementation of [`TaskManager`]
//!
//! It is only used to manage processes and schedule process based on ready queue.
//! Other CPU process monitoring functions are in Processor.

use super::TaskControlBlock;
use super::schedule::*;
use crate::sync::MPSafeCell;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use lazy_static::*;
use crate::arch::config::CPU_CORE_NUM;

pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// A simple FIFO scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.ready_queue.pop_front()
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGERS: [MPSafeCell<TaskManager>; CPU_CORE_NUM] ={
        let mut arr: [MPSafeCell<TaskManager>; CPU_CORE_NUM] = unsafe { core::mem::zeroed() };
        for i in 0..CPU_CORE_NUM {
            arr[i] = MPSafeCell::new(TaskManager::new());
        }
        arr
    };
    /// PID2PCB instance (map of pid to pcb)
    pub static ref TID2TCB: MPSafeCell<BTreeMap<usize, Arc<TaskControlBlock>>> =
        MPSafeCell::new(BTreeMap::new());
}

pub fn get_current_task_manager() -> &'static MPSafeCell<TaskManager> {
    let hart_id = riscv::register::mhartid::read();
    &TASK_MANAGERS[hart_id]
}

/// 向全局池索取任务并加入当前处理器的就绪队列
pub fn current_add_tasks() {
    let mut manager = get_current_task_manager().exclusive_access();
    let tasks = ask_for_tasks();
    for task in tasks {
        manager.add(task);
    }
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
	debug!("[kernel] TaskManager::add_task: pid={}", task.getpid());
    TID2TCB
        .exclusive_access()
        .insert(task.getpid(), Arc::clone(&task));
    get_current_task_manager().exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
	//trace!("kernel: TaskManager::fetch_task");
    get_current_task_manager().exclusive_access().fetch()
}

/// Get process by tid
pub fn tid2task(tid: usize) -> Option<Arc<TaskControlBlock>> {
    let map = TID2TCB.exclusive_access();
    map.get(&tid).map(Arc::clone)
}

/// Remove item(tid, _some_pcb) from TID2TCB map (called by exit_current_and_run_next)
/// 从索引表中删除
pub fn remove_from_tid2task(tid: usize) {
    let mut map = TID2TCB.exclusive_access();
    if map.remove(&tid).is_none() {
        panic!("cannot find tid {} in tid2task!", tid);
    }
}

pub fn task_count_in_mng() -> usize {
    TID2TCB.exclusive_access().len()
}
