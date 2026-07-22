//! Implementation of [`TaskManager`]
//!
//! It is only used to manage processes and schedule process based on ready queue.
//! Other CPU process monitoring functions are in Processor.


use core::cmp::Ordering;

use super::TaskControlBlock;
use super::task::taskstatus::TaskStatus;
use super::schedule::*;
use super::pcb::*;
use crate::MAIN_HART_ID;
use crate::sync::MPSafeCell;
use crate::arch::config::CPU_CORE_NUM;
use crate::get_hart_id;
use alloc::collections::{BTreeMap, BinaryHeap, VecDeque};
use alloc::sync::Arc;
use lazy_static::*;



lazy_static!{
    pub static ref PROCESS_MANAGER: MPSafeCell<ProcessManager> = MPSafeCell::new(ProcessManager{
        process_pool: BTreeMap::new(),
    });
}

// 全局进程管理器/列表，掌握所有进程的生命周期
// 用B树+arc指针实现，应该能很快地遍历/删除等
pub struct ProcessManager{
    // 进程池
    process_pool: BTreeMap<usize, Arc<ProcessControlBlock>>,
}

impl ProcessManager{
    pub fn add_process(&mut self, process: Arc<ProcessControlBlock>){
        self.process_pool.insert(process.getpid(), process);
    }

    pub fn get_process(&self, pid: usize) -> Option<Arc<ProcessControlBlock>>{
        self.process_pool.get(&pid).map(Arc::clone)
    }

    pub fn list_pids(&self) -> alloc::vec::Vec<usize> {
        self.process_pool.keys().copied().collect()
    }

    pub fn remove_process(&mut self, pid: usize){
        info!("ProcessManager::try to remove_process: pid={}", pid);
        if self.process_pool.remove(&pid).is_none(){
            panic!("cannot find pid {} in process pool! hart_id={}", pid , get_hart_id());
        }
        info!("ProcessManager::remove_process: pid={} removed", pid);
    }
}


pub fn add_process(process: Arc<ProcessControlBlock>){
    PROCESS_MANAGER.exclusive_access().add_process(process);
}

pub fn get_process(pid: usize) -> Option<Arc<ProcessControlBlock>>{
    PROCESS_MANAGER.exclusive_access().get_process(pid)
}

pub fn list_pids() -> alloc::vec::Vec<usize> {
    PROCESS_MANAGER.exclusive_access().list_pids()
}

pub fn remove_process(pid: usize){
    PROCESS_MANAGER.exclusive_access().remove_process(pid);
}

pub fn dump_processes(reason: &str) {
    println!("========== process dump: {} ==========" , reason);

    let tasks = {
        let map = TID2TCB.exclusive_access();
        map.values().cloned().collect::<alloc::vec::Vec<_>>()
    };

    for task in tasks {
        let process = task.process();
        let proc_inner = process.inner_exclusive_access();
        let task_inner = task.inner_exclusive_access();
        let ppid = proc_inner
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
            .map_or(0, |parent| parent.getpid());

        println!(
            "[PROC] pid={} parent_pid={} pgid={} tgid={} tid={} name={} status={} policy={} prio={} children={} proc_sig={:#x} task_sig={:#x} killed={} term={:?} main_hart={} owner={:?}",
            process.getpid(),
            ppid,
            proc_inner.pgid,
            task.gettgid(),
            task.gettid(),
            proc_inner.pname,
            task_inner.task_status,
            task_inner.sched_policy,
            task_inner.sched_priority,
            proc_inner.children.len(),
            proc_inner.signals.bits(),
            task_inner.signals.bits(),
            task_inner.killed,
            task_inner.term_signal,
            proc_inner.on_main_hart,
            task_inner.owner_hart,
        );
    }

    println!("======================================");
}
// 优先级和tcb引用
struct HeapInode{
    priority: usize,
    order: usize,
    tcb: Arc<TaskControlBlock>,
}
impl PartialEq for HeapInode {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.order == other.order && Arc::ptr_eq(&self.tcb, &other.tcb)
    }
}

impl Eq for HeapInode {}

impl Ord for HeapInode {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_ptr = Arc::as_ptr(&self.tcb) as usize;
        let other_ptr = Arc::as_ptr(&other.tcb) as usize;
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.order.cmp(&self.order))
            .then_with(|| self_ptr.cmp(&other_ptr))
    }
}

impl PartialOrd for HeapInode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
pub struct TaskManager {
    ready_queue: BinaryHeap<HeapInode>,
    enqueue_order: usize,
}

pub const SCHED_OTHER: isize = 0;
pub const SCHED_FIFO: isize = 1;
pub const SCHED_RR: isize = 2;
pub const SCHED_BATCH: isize = 3;
pub const SCHED_IDLE: isize = 5;

const LOCAL_QUEUE_LOW_WATERMARK: usize = 2;
const LOCAL_QUEUE_REFILL_TARGET: usize = 4;

/// A simple FIFO scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: BinaryHeap::new(),
            enqueue_order: 0,
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        let tid = task.gettid();
        if self.ready_queue.iter().any(|inode| inode.tcb.gettid() == tid) {
            return;
        }
        let (class, priority) = task_sched_rank(&task);
        let priority = (class as usize) * 100 + priority.max(0) as usize;
        let order = self.enqueue_order;
        self.enqueue_order = self.enqueue_order.wrapping_add(1);
        self.ready_queue.push(HeapInode { priority, order, tcb: task });
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.ready_queue.pop().map(|inode| inode.tcb)
    }
    pub fn task_count(&self) -> usize {
        self.ready_queue.len()
    }

    pub fn remove(&mut self, tid: usize) {
        self.ready_queue.retain(|inode| inode.tcb.gettid() != tid);
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGERS: [MPSafeCell<TaskManager>; CPU_CORE_NUM] ={
        core::array::from_fn(|_| MPSafeCell::new(TaskManager::new()))
    };
    /// TID2TCB instance (map of tid to pcb)
    pub static ref TID2TCB: MPSafeCell<BTreeMap<usize, Arc<TaskControlBlock>>> =
        MPSafeCell::new(BTreeMap::new());
}

pub fn get_current_task_manager() -> &'static MPSafeCell<TaskManager> {
    let hart_id = get_hart_id();
    &TASK_MANAGERS[hart_id]
}

/// 向全局池索取任务并加入当前处理器的就绪队列
pub fn current_add_tasks() {
    let mut manager = get_current_task_manager().exclusive_access();
    if manager.task_count() >= LOCAL_QUEUE_LOW_WATERMARK {
        return;
    }
    while manager.task_count() < LOCAL_QUEUE_REFILL_TARGET {
        if let Some(task) = ask_for_task() {
            manager.add(task);
        } else {
            break;
        }
    }
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    debug!("[kernel] TaskManager::add_task: pid={}", task.getpid());
    //dump_processes("add_task");
    TID2TCB
        .exclusive_access()
        .insert(task.gettid(), Arc::clone(&task));
    //dump_processes("add_task");
    add_task_into_pool(task);
}

pub fn add_task_in_current_hart(task: Arc<TaskControlBlock>) {
    let _dispatch = lock_dispatch();
    add_task_in_current_hart_unlocked(task);
}

pub(crate) fn add_task_in_current_hart_unlocked(task: Arc<TaskControlBlock>) {
    remove_task_from_all_local_queues_unlocked(task.gettid());
    remove_task_from_global_pool_unlocked(task.gettid());
    let mut manager = get_current_task_manager().exclusive_access();
    manager.add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
	//trace!("kernel: TaskManager::fetch_task");
    let _dispatch = lock_dispatch();
    current_add_tasks();
    let hart_id = get_hart_id();
    loop {
        let task = get_current_task_manager().exclusive_access().fetch();
        let Some(task) = task else {
            return None;
        };
        //println!("[kernel] fetch_task: hart_id={}, got task pid={} tid={} priority={}", hart_id, task.getpid(), task.gettid(), task.inner_exclusive_access().sched_priority);
        let mut task_inner = task.inner_exclusive_access();
        if task_inner.task_status != TaskStatus::Ready || task_inner.owner_hart.is_some() {
            continue;
        }
        task_inner.task_status = TaskStatus::Running;
        drop(task_inner);
        remove_task_from_all_local_queues_unlocked(task.gettid());
        remove_task_from_global_pool_unlocked(task.gettid());
        return Some(task);
    }
}

pub fn cores_fetch_task() {
    for i in 0..CPU_CORE_NUM {
        debug!("core {} is fetching tasks", i);
        let mut manager = TASK_MANAGERS[i].exclusive_access();
        let list = ask_for_tasks();
        for task in list {
            manager.add(task);
        }
    }
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
    drop(map);
    //dump_processes("remove_task");
}

pub fn task_count_in_mng() -> usize {
    TID2TCB.exclusive_access().len()
}

pub(crate) fn remove_task_from_all_local_queues_unlocked(tid: usize) {
    for hart_id in 0..CPU_CORE_NUM {
        TASK_MANAGERS[hart_id].exclusive_access().remove(tid);
    }
}

pub fn remove_task_from_all_local_queues(tid: usize) {
    let _dispatch = lock_dispatch();
    remove_task_from_all_local_queues_unlocked(tid);
}
