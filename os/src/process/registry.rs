//! Global task registry indexed by thread ID.

use crate::process::scheduler::runqueue::{
    add_task_into_pool, lock_dispatch, remove_task_from_all_local_queues_unlocked,
    remove_task_from_global_pool_unlocked,
};
use crate::process::TaskStruct;
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use lazy_static::*;

lazy_static! {
    pub static ref TID2TCB: MPSafeCell<BTreeMap<usize, Arc<TaskStruct>>> =
        MPSafeCell::new(BTreeMap::new());
}

pub fn add_process(process: Arc<TaskStruct>) {
    TID2TCB
        .exclusive_access()
        .insert(process.gettid(), process);
}

pub fn get_process(pid: usize) -> Option<Arc<TaskStruct>> {
    let tasks = TID2TCB.exclusive_access();
    tasks
        .get(&pid)
        .filter(|task| task.gettgid() == pid)
        .cloned()
        .or_else(|| tasks.values().find(|task| task.gettgid() == pid).cloned())
}

pub fn list_pids() -> alloc::vec::Vec<usize> {
    let tasks = TID2TCB.exclusive_access();
    let mut pids = tasks
        .values()
        .map(|task| task.gettgid())
        .collect::<alloc::vec::Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

pub fn remove_process(pid: usize) {
    let _dispatch = lock_dispatch();
    let tids = {
        let tasks = TID2TCB.exclusive_access();
        tasks
            .values()
            .filter(|task| task.gettgid() == pid)
            .map(|task| task.gettid())
            .collect::<alloc::vec::Vec<_>>()
    };
    for &tid in &tids {
        remove_from_tid2task(tid);
    }

    for tid in tids.into_iter().chain(core::iter::once(pid)) {
        remove_task_from_all_local_queues_unlocked(tid);
        remove_task_from_global_pool_unlocked(tid);
    }
}

pub fn dump_processes(reason: &str) {
    println!("========== process dump: {} ==========", reason);
    let tasks = {
        let map = TID2TCB.exclusive_access();
        map.values().cloned().collect::<alloc::vec::Vec<_>>()
    };

    for task in tasks {
        let task_inner = task.inner_exclusive_access();
        let ppid = task_inner
            .parent
            .upgrade()
            .map_or(0, |parent| parent.getpid());
        println!(
            "[PROC] pid={} parent_pid={} tgid={} tid={} status={:?} policy={} prio={} children={} pending={:#x} term={:?} main_hart={}",
            task.getpid(),
            ppid,
            task.gettgid(),
            task.gettid(),
            task_inner.state,
            task_inner.sched_policy,
            task_inner.sched_priority,
            task_inner.children.len(),
            task_inner.pending.bits(),
            task_inner.term_signal,
            task_inner.on_main_hart,
        );
    }
    println!("======================================");
}

pub fn add_task(task: Arc<TaskStruct>) {
    debug!("[kernel] TaskManager::add_task: pid={}", task.getpid());
    TID2TCB
        .exclusive_access()
        .insert(task.gettid(), Arc::clone(&task));
    add_task_into_pool(task);
}

pub fn tid2task(tid: usize) -> Option<Arc<TaskStruct>> {
    let map = TID2TCB.exclusive_access();
    map.get(&tid).map(Arc::clone)
}

pub fn remove_from_tid2task(tid: usize) {
    let mut map = TID2TCB.exclusive_access();
    // 已不在表中则视为已移除，直接返回
    map.remove(&tid);
}

pub fn task_count_in_mng() -> usize {
    TID2TCB.exclusive_access().len()
}
