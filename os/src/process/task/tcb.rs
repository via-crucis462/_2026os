//! Types related to task management & Functions for completely changing TCB
#![allow(unused)]
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle, TIdHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    arch::trap::{TrapContext, trap_handler},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap},
    sync::MPSafeCell,
    ipc::namespace::NsProxy,
};
use crate::process::task::{
    context::ThreadStruct,
    cred::Cred,
    signal::{Signal, SigHand, Sigpending},
    task_fs::FsStruct,
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
#[allow(unused)]
use crate::arch::config::*;
use super::*;


/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// 线程所属进程
    /// 让线程拥有对进程的弱引用，便于调用进程的方法
    /// 不能用arc否则循环引用
    pub process: Weak<ProcessControlBlock>,

    /// 线程id
    pub tid: Arc<TIdHandle>,

    /// Thread group id. Linux getpid() returns this id, while gettid() returns tid.
    pub tgid: usize,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    pub inner: MPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    pub fn process(&self) -> Arc<ProcessControlBlock> {
        self.process.upgrade().unwrap()
    }
    pub fn getpid(&self) -> usize {
        self.tgid
    }
    pub fn gettgid(&self) -> usize {
        self.tgid
    }
    pub fn gettid(&self) -> usize {
        self.tid.0
    }
    pub fn get_policy_and_priority(&self) -> (isize, i32) {
        let inner = self.inner_exclusive_access();
        (inner.sched_policy, inner.sched_priority)
     }
    pub fn recycle_on_exit(&self, exit_code: i32) {
        remove_from_tid2task(self.gettid());

        let mut inner = self.inner_exclusive_access();
        inner.exit_code = exit_code;
        inner.errno = 0;
        inner.task_status = TaskStatus::Zombie;
        inner.signals = SignalFlags::empty();
        inner.signal_interrupted = false;
        inner.signal_mask_backup.clear();
        inner.trap_ctx_backup.clear();
        inner.signal_user_context_backup.clear();
        inner.killed = false;
        inner.term_signal = None;
        inner.frozen = false;
    }
}

pub struct TaskControlBlockInner {

    /// 此处改为直接保存地址
    pub trap_cx_addr: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// 当前由哪个 hart 持有运行所有权；None 表示可被调度领取。
    pub owner_hart: Option<usize>,

    pub sched_policy: isize,
    pub sched_priority: i32,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,
    pub errno: i32,
    pub signals: SignalFlags,
    pub signal_interrupted: bool,
    pub signal_mask: SignalFlags,
    /// 信号嵌套处理时的掩码栈（当前未完全验证行为是否正确，初步测试没问题）
    pub signal_mask_backup: Vec<SignalFlags>,
    // if the task is killed
    pub killed: bool,
    pub term_signal: Option<i32>,
    // if the task is frozen by a signal
    pub frozen: bool,
    /// 信号嵌套处理时的上下文栈（当前未完全验证行为是否正确，初步测试没问题）
    pub trap_ctx_backup: Vec<TrapContext>,

    /// 用户态 signal frame 中 ucontext 的地址，用于 sigreturn 读取用户修改后的上下文。
    pub signal_user_context_backup: Vec<usize>,

    pub clear_child_tid: usize,// 线程清理指针
}

impl TaskControlBlockInner {
    #[cfg(target_arch = "riscv64")]
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        PhysAddr(self.trap_cx_addr).get_mut()
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        unsafe { (self.trap_cx_addr as *mut TrapContext).as_mut().unwrap() }
    }

    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }

}

impl TaskControlBlock {

}


pub struct TaskStruct {
    pub inner: MPSafeCell<TaskStructInner>,
}
pub struct TaskStructInner {

    // 命名空间
    pub nsproxy: Arc<NsProxy>,
    /* 0. 上下文 */
    pub thread: ThreadStruct, // 线程上下文，保存寄存器等信息

    /* 1. 进程标识信息 */
    pub pid: Arc<TIdHandle>,                // 全局唯一进程 ID
    pub tgid: Arc<TIdHandle>,               // 线程组 ID，主线程 pid=tgid
    pub group_leader: Weak<TaskStruct>,  // 线程组领头进程
    pub kernel_stack: KernelStack,

    /* 2. 进程亲缘关系 */
    pub real_parent: Weak<TaskStruct>,  // 实际创建当前进程的父进程
    pub parent: Weak<TaskStruct>,       // 接收 SIGCHLD 信号的父进程
    pub children: Vec<Arc<TaskStruct>>,              // 子进程链表头

    /* 3. 进程状态 */
    pub state: TaskStatus,        // 进程运行状态
    pub exit_state: i64,            // 进程退出状态
    pub exit_code: i32,     // 进程退出码
    pub exit_signal: i32,   // 进程退出信号
    pub flags: u32,         // 进程特性标志
    pub errno: i32,         // 进程错误码

    /* 4. 进程调度相关 */
    /*pub sched_class: *const sched_class,  // 绑定的调度器类
    pub se: sched_entity,     // CFS 完全公平调度实体
    pub rt: sched_rt_entity,  // 实时调度实体
    pub prio: i32,                  // 动态优先级
    pub static_prio: i32,           // 静态优先级
    pub normal_prio: i32,           // 普通优先级*/
    pub sched_policy: isize,
    pub sched_priority: i32,

    /* 5. 内存管理相关 */
    pub mm: Option<Arc<MPSafeCell<MemorySet>>>,       // 用户进程内存描述符
    // pub active_mm: *mut mm_struct,// 上下文切换使用的活动 mm

    /* 6. 文件系统与文件描述符 */
    pub fs: Option<Arc<MPSafeCell<FsStruct>>>,       // 进程当前目录、根目录信息
    pub files: Option<Arc<MPSafeCell<Vec<FileDescriptor>>>>, // 进程打开的文件描述符表

    /*7. 信号处理相关 */
    pub signal: Option<Arc<MPSafeCell<Signal>>>,  // 信号处理相关信息
    pub signal_hand: Option<Arc<MPSafeCell<SigHand>>>, // 信号处理函数相关信息
    pub blocked: SignalFlags, // 当前阻塞的信号集
    pub pending: Sigpending, // 当前挂起的信号集 

    /* 8. gid uid等 */
    pub cred: Option<Arc<MPSafeCell<Cred>>>, // 进程的凭证信息
    pub real_cred: Option<Arc<MPSafeCell<Cred>>>, // 进程的真实凭证信息

    /* 9. 其他 */
    pub start_time: u64, // 进程启动时间
    pub start_boottime: u64, // 进程启动时间的低位

    /* 10 .CPU调度  */
    pub on_cpu: bool,
    pub on_rq: bool,
    pub cpu: usize,

    /*11 .线程退出清理地址 */
    pub clear_child_tid: usize, // 线程清理指针
    pub personality: usize, // 进程个性化标志
    pub comm: [u8; 10],
}