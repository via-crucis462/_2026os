//! Types related to task management & Functions for completely changing TCB
#![allow(unused)]
use crate::process::{KernelStack, PidHandle, SignalFlags};
use crate::{
    arch::trap::TrapContext,
    mm::MemorySet,
    sync::{MPSafeCell, MPSafeGuard},
    ipc::namespace::NsProxy,
};
use crate::process::task::{
    context::ThreadStruct,
    cred::Cred,
    fs::FsStruct,
};
use crate::process::signal::{Signal, SigHand, Sigpending};
use alloc::{sync::{Arc, Weak}, vec::Vec};
use crate::process::task::*;

#[deny(non_camel_case_types)]
pub struct TaskStruct {
    pub pid: Arc<PidHandle>,                // 全局唯一线程 ID
    pub tgid: Arc<PidHandle>,               // 线程组 ID，主线程 pid=tgid
    pub group_leader: Weak<TaskStruct>, // 线程组领头进程
    pub inner: MPSafeCell<TaskStructInner>, // 内部可变结构体
}

#[derive(Clone, Copy, Default)]
pub struct SignalAltStackState {
    pub sp: usize,
    pub size: usize,
    pub flags: u32,
}

impl TaskStruct {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, TaskStructInner> {
        self.inner.exclusive_access()
    }

    /*pub fn process(self: &Arc<Self>) -> Arc<Self> {
        self.group_leader
            .upgrade()
            .unwrap_or_else(|| Arc::clone(self))
    }*/

    pub fn getpid(&self) -> usize {
        self.tgid.0
    }
    pub fn gettgid(&self) -> usize {
        self.tgid.0
    }
    pub fn gettid(&self) -> usize {
        self.pid.0
    }


    /// 返回线程组共享的 RLIMIT_NOFILE 软限制。
    pub fn nofile_limit(&self) -> usize {
        let signal = self.inner_exclusive_access().signal.clone();
        let limit = signal.exclusive_access().rlimits().nofile.rlim_cur;
        limit
    }

    /// 返回优先级相关
    pub fn get_policy_and_priority(&self) -> (isize, i32) {
        let inner = self.inner_exclusive_access();
        (inner.sched_policy, inner.sched_priority)
     }

}
pub struct TaskStructInner {
    pub on_main_hart: bool, // 是否在主核上运行
    // 命名空间
    pub nsproxy: Arc<NsProxy>,
    /* 0. 上下文 */
    pub thread: ThreadStruct, // 线程上下文，保存寄存器等信息

    /* 1. 进程标识信息 */
    pub group_leader: Weak<TaskStruct>,  // 线程组领头进程
    pub kernel_stack: KernelStack,

    /* 2. 进程亲缘关系 */
    pub real_parent: Weak<TaskStruct>,  // 实际创建当前进程的父进程
    pub parent: Weak<TaskStruct>,       // 接收 SIGCHLD 信号的父进程
    pub children: Vec<Arc<TaskStruct>>,              // 子进程链表头
    pub pgid: usize,    //进程组id
    pub sid: usize,     //会话id

    /* 3. 进程状态 */
    pub state: TaskStatus,        // 进程运行状态
    pub exit_state: i64,            // 进程退出状态
    pub exit_code: i32,     // 进程退出码
    pub exit_signal: i32,   // 进程退出信号
    pub flags: u32,         // 进程特性标志
    pub errno: i32,         // 进程错误码

    /* 4. 进程调度相关 */
    /// 用户设置的调度策略，例如 SCHED_OTHER、SCHED_RR 或 SCHED_DEADLINE。
    pub sched_policy: isize,
    /// 用户可见的实时优先级；普通策略为 0，实时策略为 1..=99。
    pub sched_priority: i32,
    /// 调度器当前使用的动态优先级。
    pub prio: i32,
    /// 由 nice 或实时参数计算出的静态优先级。
    pub static_prio: i32,
    /// 不考虑临时优先级继承时的正常优先级。
    pub normal_prio: i32,
    /// 普通公平调度实体。
    pub se: SchedEntity,
    /// FIFO/RR 实时调度实体。
    pub rt: SchedRtEntity,
    /// Deadline/CBS 调度实体。
    pub dl: SchedDlEntity,

    /* 5. 内存管理相关 */
    pub mm: Option<Arc<MPSafeCell<MemorySet>>>,       // 用户进程内存描述符
    // pub active_mm: *mut mm_struct,// 上下文切换使用的活动 mm

    /* 6. 文件系统与文件描述符 */
    pub fs: Arc<MPSafeCell<FsStruct>>,       // 进程当前目录、根目录信息
    pub files: Arc<MPSafeCell<FileDescriptorTable>>, // 进程打开的文件描述符表

    /*7. 信号处理相关 */
    pub signal: Arc<MPSafeCell<Signal>>,  // 信号处理相关信息
    pub signal_hand: Arc<MPSafeCell<SigHand>>, // 信号处理函数相关信息
    pub blocked: SignalFlags, // 当前阻塞的信号集
    pub pending: Sigpending, // 当前挂起的信号集 
    pub signal_interrupted: bool,
    pub sigsuspend_saved_mask: Option<SignalFlags>,
    pub signal_mask_backup: Vec<SignalFlags>,
    pub trap_ctx_backup: Vec<TrapContext>,
    pub signal_user_context_backup: Vec<usize>,
    /// 当前线程通过 sigaltstack(2) 注册的备用信号栈。
    pub signal_alt_stack: SignalAltStackState,
    pub term_signal: Option<i32>,
    pub frozen: bool,

    /* 8. gid uid等 */
    pub cred: Arc<MPSafeCell<Cred>>, // 进程的凭证信息
    pub real_cred: Arc<MPSafeCell<Cred>>, // 进程的真实凭证信息

    /* 9. 其他 */
    pub start_time: u64, // 进程启动时间
    pub start_boottime: u64, // 进程启动时间的低位

    /* 10 .CPU调度  */
    pub on_cpu: bool,
    pub on_rq: bool,
    pub cpu: usize,
    /// 允许任务运行的 CPU 位图；第 N 位对应 CPU N。
    pub cpus_allowed: usize,
    /// 是否请求在安全调度点重新调度当前任务。
    pub need_resched: bool,

    /*11 .线程退出清理地址 */
    pub clear_child_tid: usize, // 线程清理指针
    pub personality: usize, // 进程个性化标志
    pub locked_bytes: usize, // MAP_LOCKED 映射字节数
    pub comm: [u8; 10],
}
impl TaskStructInner {

    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        unsafe { (self.thread.trap_ctx as *mut TrapContext).as_mut().unwrap() }
    }

    pub fn get_user_token(&self) -> usize {
        self.mm
            .as_ref()
            .expect("user task has no mm")
            .exclusive_access()
            .token()
    }

    pub fn get_asid(&self) -> usize {
        self.mm
            .as_ref()
            .expect("user task has no mm")
            .exclusive_access()
            .asid()
    }
}

pub type TaskControlBlock = TaskStruct;
pub type TaskControlBlockInner = TaskStructInner;