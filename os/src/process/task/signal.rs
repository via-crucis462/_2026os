use bitflags::*;
use alloc::vec::Vec;
use alloc::sync::{Arc, Weak};
use crate::process::task::rlimit::Rlimits;
use super::*;
/// The max signal number
pub const MAX_SIG: usize = 64;

bitflags! {
    /// 内核态的信号位图
    /// 补充：用户传入的是1based的整数编号，内核使用0-based的位图，所以定义时都减1
    pub struct SignalFlags: u64 {
        //                      * 星号一列是用户传入的值
        const SIGHUP    = 1 << (1 - 1);   // 终端挂断
        const SIGINT    = 1 << (2 - 1);   // 键盘中断 (Ctrl+C)
        const SIGQUIT   = 1 << (3 - 1);   // 键盘退出 (Ctrl+\)
        const SIGILL    = 1 << (4 - 1);   // 非法指令
        const SIGTRAP   = 1 << (5 - 1);   // 断点/陷阱指令
        const SIGABRT   = 1 << (6 - 1);   // 异常中止 (Abort)
        const SIGBUS    = 1 << (7 - 1);   // 总线错误 (内存访问非法)
        const SIGFPE    = 1 << (8 - 1);   // 算术异常 (如除零)
        const SIGKILL   = 1 << (9 - 1);   // 强制杀死 (不可捕获/忽略)
        const SIGUSR1   = 1 << (10 - 1);  // 用户自定义信号 1
        const SIGSEGV   = 1 << (11 - 1);  // 段错误 (非法内存引用)
        const SIGUSR2   = 1 << (12 - 1);  // 用户自定义信号 2
        const SIGPIPE   = 1 << (13 - 1);  // 管道破裂 (写无读端的管道)
        const SIGALRM   = 1 << (14 - 1);  // 定时器到时 (Alarm)
        const SIGTERM   = 1 << (15 - 1);  // 终止信号 (默认杀进程)
        const SIGSTKFLT = 1 << (16 - 1);  // 协处理器栈故障 (已废弃)
        const SIGCHLD   = 1 << (17 - 1);  // 子进程状态改变
        const SIGCONT   = 1 << (18 - 1);  // 继续执行
        const SIGSTOP   = 1 << (19 - 1);  // 停止执行 (不可捕获/忽略)
        const SIGTSTP   = 1 << (20 - 1);  // 键盘停止 (Ctrl+Z)
        const SIGTTIN   = 1 << (21 - 1);  // 后台进程尝试读终端
        const SIGTTOU   = 1 << (22 - 1);  // 后台进程尝试写终端
        const SIGURG    = 1 << (23 - 1);  // 套接字紧急数据
        const SIGXCPU   = 1 << (24 - 1);  // 超过 CPU 限制
        const SIGXFSZ   = 1 << (25 - 1);  // 超过文件大小限制
        const SIGVTALRM = 1 << (26 - 1);  // 虚拟定时器到时
        const SIGPROF   = 1 << (27 - 1);  // 性能分析定时器到时
        const SIGWINCH  = 1 << (28 - 1);  // 窗口大小改变
        const SIGIO     = 1 << (29 - 1);  // 异步 I/O (等同于 SIGPOLL)
        const SIGPWR    = 1 << (30 - 1);  // 电源故障
        const SIGSYS    = 1 << (31 - 1);  // 系统调用参数错误

        // 实时信号
        const SIGRTMIN    = 1 << (32 - 1);
        const SIGRT_33    = 1 << (33 - 1);
        const SIGRT_34    = 1 << (34 - 1);
        const SIGRT_35    = 1 << (35 - 1);
        const SIGRT_36    = 1 << (36 - 1);
        const SIGRT_37    = 1 << (37 - 1);
        const SIGRT_38    = 1 << (38 - 1);
        const SIGRT_39    = 1 << (39 - 1);
        const SIGRT_40    = 1 << (40 - 1);
        const SIGRT_41    = 1 << (41 - 1);
        const SIGRT_42    = 1 << (42 - 1);
        const SIGRT_43    = 1 << (43 - 1);
        const SIGRT_44    = 1 << (44 - 1);
        const SIGRT_45    = 1 << (45 - 1);
        const SIGRT_46    = 1 << (46 - 1);
        const SIGRT_47    = 1 << (47 - 1);
        const SIGRT_48    = 1 << (48 - 1); // LTP kill02 测试的信号
        const SIGRT_49    = 1 << (49 - 1);
        const SIGRT_50    = 1 << (50 - 1);
        const SIGRT_51    = 1 << (51 - 1);
        const SIGRT_52    = 1 << (52 - 1);
        const SIGRT_53    = 1 << (53 - 1);
        const SIGRT_54    = 1 << (54 - 1);
        const SIGRT_55    = 1 << (55 - 1);
        const SIGRT_56    = 1 << (56 - 1);
        const SIGRT_57    = 1 << (57 - 1);
        const SIGRT_58    = 1 << (58 - 1);
        const SIGRT_59    = 1 << (59 - 1);
        const SIGRT_60    = 1 << (60 - 1);
        const SIGRT_61    = 1 << (61 - 1);
        const SIGRT_62    = 1 << (62 - 1);
        const SIGRT_63    = 1 << (63 - 1); // 增加一行填满64位，方便遍历
        const SIGRTMAX    = 1 << (64 - 1);
    }
}

impl SignalFlags {
    /// Check if there is an error in the signal flags
    pub fn check_error(&self) -> Option<(i32, &'static str)> {
        if self.contains(Self::SIGINT) {
            Some((-2, "Killed, SIGINT=2"))
        } else if self.contains(Self::SIGILL) {
            Some((-4, "Illegal Instruction, SIGILL=4"))
        } else if self.contains(Self::SIGABRT) {
            Some((-6, "Aborted, SIGABRT=6"))
        } else if self.contains(Self::SIGFPE) {
            Some((-8, "Erroneous Arithmetic Operation, SIGFPE=8"))
        } else if self.contains(Self::SIGKILL) {
            Some((-9, "Killed, SIGKILL=9"))
        } else if self.contains(Self::SIGSEGV) {
            Some((-11, "Segmentation Fault, SIGSEGV=11"))
        } else {
            // warn!("[kernel] signalflags check_error  {:?}", self);
            None
        }
    }
    /// 转换为用户态定义的信号编号
    pub fn number(&self) -> Option<usize> {
        for i in 0..=MAX_SIG {
            if self.contains(SignalFlags::from_bits(1 << i).unwrap()) {
                return Some(i + 1); // 返回用户传入的1-based编号
            }
        }
        None
    }
}
#[derive(Clone)]
pub struct Sigpending{
    queue: Vec<usize>, // 挂起信号队列
    signals: SignalFlags, // 挂起信号集
    wait_chldexit: bool, // 是否等待子进程退出
}

impl Sigpending {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            signals: SignalFlags::empty(),
            wait_chldexit: false,
        }
    }

    pub fn insert(&mut self, signal: SignalFlags) {
        self.signals.insert(signal);
        if let Some(number) = signal.number() {
            self.queue.push(number);
        }
    }

    pub fn bits(&self) -> u64 {
        self.signals.bits()
    }

    pub fn contains(&self, signal: SignalFlags) -> bool {
        self.signals.contains(signal)
    }

    pub fn remove(&mut self, signal: SignalFlags) {
        self.signals.remove(signal);
        if let Some(number) = signal.number() {
            self.queue.retain(|queued| *queued != number);
        }
    }

    pub fn flags(&self) -> SignalFlags {
        self.signals
    }
}

pub struct Signal{
    shared_pending: Sigpending, // 共享挂起信号集
    group_exit_state: i32, // 线程组退出状态
    thread_num: usize, // 线程组中线程数量
    pub next_thread: Option<Weak<TaskControlBlock>>, // 线程组中下一个线程
    rlimits: Rlimits, // 线程组共享的资源限制
}
impl Signal{
    pub fn new() -> Self {
        Self {
            shared_pending: Sigpending::new(),
            group_exit_state: 0,
            thread_num: 1,
            next_thread: None,
            rlimits: Rlimits::new(),
        }
    }

    pub fn fork_from(parent: &Self) -> Self {
        Self {
            shared_pending: Sigpending::new(),
            group_exit_state: 0,
            thread_num: 1,
            next_thread: None,
            rlimits: parent.rlimits.clone(),
        }
    }

    pub fn add_thread(&mut self) {
        self.thread_num += 1;
    }

    pub fn insert_pending(&mut self, signal: SignalFlags) {
        self.shared_pending.insert(signal);
    }

    pub fn pending_flags(&self) -> SignalFlags {
        self.shared_pending.flags()
    }

    pub fn remove_pending(&mut self, signal: SignalFlags) {
        self.shared_pending.remove(signal);
    }

    pub fn rlimits(&self) -> &Rlimits {
        &self.rlimits
    }

    pub fn rlimits_mut(&mut self) -> &mut Rlimits {
        &mut self.rlimits
    }
}
#[derive(Clone)]
pub struct SigHand{
    sig_actions: SignalActions, // 信号处理函数
}

impl SigHand {
    pub fn new() -> Self {
        Self {
            sig_actions: SignalActions::new(),
        }
    }

    pub fn action(&self, signal_index: usize) -> SignalAction {
        self.sig_actions.table[signal_index]
    }

    pub fn set_action(&mut self, signal_index: usize, action: SignalAction) {
        self.sig_actions.table[signal_index] = action;
    }
}
