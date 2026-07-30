//! 内核工作线程
//!
use super::*;

use crate::fs::ROOT_DENTRY;
use crate::ipc::namespace::{IPCNamespace, NsProxy};
use crate::process::scheduler::runqueue::SCHED_IDLE;
use crate::process::signal::{SigHand, Signal, SignalFlags, Sigpending};
use crate::process::{self, kstack_alloc, pid_alloc};
use crate::sync::MPSafeCell;
use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};

impl TaskStruct {
    /// 创建一个新的内核工作线程
    pub fn new_kernel_worker(entry: fn() -> !) -> Arc<Self> {
        let pid = Arc::new(pid_alloc());
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();

        Arc::new_cyclic(|task_weak| Self {
            pid: pid.clone(),
            tgid: pid.clone(),
            group_leader: task_weak.clone(),
            inner: MPSafeCell::new(TaskStructInner {
                on_main_hart: false,
                nsproxy: Arc::new(NsProxy::new(IPCNamespace::new())),
                thread: ThreadStruct {
                    // 从指定入口开始执行
                    task_ctx: TaskContext::goto_kernel_worker(entry, kernel_stack_top),
                    // 不进入用户态，因此不需要 trap_ctx
                    trap_ctx: 0,
                },
                group_leader: task_weak.clone(),
                kernel_stack,
                real_parent: Weak::new(),
                parent: Weak::new(),
                children: Vec::new(),
                pgid: pid.0,
                sid: pid.0,
                state: TaskStatus::Ready,
                exit_state: 0,
                exit_code: 0,
                exit_signal: 0,
                flags: 0,
                errno: 0,
                sched_policy: SCHED_IDLE,
                sched_priority: 0,
                prio: 120,
                static_prio: 120,
                normal_prio: 120,
                se: SchedEntity::new(),
                rt: SchedRtEntity::new(),
                dl: SchedDlEntity::new(),
                mm: None,
                fs: Arc::new(MPSafeCell::new(FsStruct::new(
                    ROOT_DENTRY.clone(),
                    ROOT_DENTRY.clone(),
                ))),
                files: Arc::new(MPSafeCell::new(FileDescriptorTable::new())),
                signal: Arc::new(MPSafeCell::new(Signal::new())),
                signal_hand: Arc::new(MPSafeCell::new(SigHand::new())),
                blocked: SignalFlags::empty(),
                pending: Sigpending::new(),
                signal_interrupted: false,
                signal_mask_backup: Vec::new(),
                trap_ctx_backup: Vec::new(),
                signal_user_context_backup: Vec::new(),
                term_signal: None,
                frozen: false,
                cred: Arc::new(MPSafeCell::new(Cred::new(0, 0, 0, 0, 0, 0, 0, 0))),
                real_cred: Arc::new(MPSafeCell::new(Cred::new(0, 0, 0, 0, 0, 0, 0, 0))),
                start_time: 0,
                start_boottime: 0,
                on_cpu: false,
                on_rq: false,
                cpu: 0,
                cpus_allowed: (1usize << crate::arch::config::CPU_CORE_NUM) - 1,
                need_resched: false,
                clear_child_tid: 0,
                personality: 0,
                locked_bytes: 0,
                comm: [0; 10],
            }),
        })
    }
}

pub fn test_kernel_worker() -> ! {
    let mut counter = 0;
    loop {
        counter += 1;
        if counter % 100000 == 0 {
            println!("Kernel worker is running, counter: {}", counter);
        }
        suspend_current_and_run_next();
    }
}

pub fn bio_kernel_worker() -> ! {
    loop {
        println!("TODO: bio_kernel_worker is running");
        suspend_current_and_run_next();
    }
}