//! Types related to task management & Functions for completely changing TCB
#![allow(unused)]
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle, TIdHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    arch::{
        timer::get_time_us,
        trap::{TrapContext, trap_handler},
    },
    fs::{open_file, Dentry, File, OpenFlags, ROOT_DENTRY,Stdin, Stdout},
    ipc::namespace::IPCNamespace,
    mm::{
        page_table::PageSize,
        translated_write, KERNEL_SPACE, MapArea, MapPermission, MapType, MemorySet, PhysAddr,
        VirtAddr, mmap,
    },
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
use crate::syscall::errno::Errno;
use crate::syscall::errno::*;
#[allow(unused)]
use crate::arch::config::*;
use super::*;


/// Task control block structure
///
/// Directly save the contents that will not change during running
/*pub struct TaskControlBlock {
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
        #[cfg(target_arch = "riscv64")]
        if let Some(mm) = inner.mm.as_ref() {
            mm.exclusive_access()
                .remove_trap_context_page(VirtAddr::from(inner.thread.trap_ctx));
        }
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
    pub state: TaskStatus,

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
        self.state
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }

}

impl TaskControlBlock {

} */

#[deny(non_camel_case_types)]
pub struct TaskStruct {
    pub pid: Arc<PidHandle>,                // 全局唯一线程 ID
    pub tgid: Arc<PidHandle>,               // 线程组 ID，主线程 pid=tgid
    pub group_leader: Weak<TaskStruct>, // 线程组领头进程
    pub inner: MPSafeCell<TaskStructInner>, // 内部可变结构体
}
impl TaskStruct {
     pub fn init_proc(elf_data: &[u8]) -> Arc<TaskControlBlock> {
        //println!("[kernel] TaskControlBlock::new: start creating a new process");

        //处理 ELF 文件，创建内存空间，返回的memory_set中已经包含了用户程序的代码段、数据段、bss段以及长度为1的堆段
        let Some((mut memory_set, heap_bottom, user_sp, entry_point, _main_entry, _phdr, _phnum, _phent, _interp_base))
            = MemorySet::from_elf(elf_data) else {
                panic!("TaskControlBlock::new: invalid ELF for init process");
            };
        debug!(
            "TaskControlBlock::new: entry_point={:#x}",
            entry_point
        );
        
        //pid ，tid 和内核栈的分配
        let pid_handle = Arc::new(pid_alloc());
        //println!("[kernel] TaskControlBlock::new: allocated PID {}", pid_handle.0);
        //let tid_handle = Arc::new(tid_from_pid(pid_handle.0));
        //println!("[kernel] TaskControlBlock::new: allocated TID {}", tid_handle.0);
        let kernel_stack = kstack_alloc();
        
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
        let kernel_stack_top = trap_cx_addr;
        let initial_user_sp = user_sp;
        #[cfg(target_arch = "riscv64")]{
            let trap_cx_va = VirtAddr::from(trap_cx_addr);
            let trap_cx_ppn = KERNEL_SPACE
                .exclusive_access()
                .translate(trap_cx_va.std_floor())
                .expect("kernel TrapContext is not mapped")
                .ppn();
            memory_set.install_trap_context_page(trap_cx_va, trap_cx_ppn);
        }

        debug!("TaskControlBlock::new: kernel_stack_top={:#x}", kernel_stack.get_top());
        // 进程控制块

        // 为pcb创建主线程
        let task_control_block = Arc::new_cyclic(|task_weak| TaskStruct {
            pid: pid_handle.clone(),
            tgid: pid_handle.clone(),
            group_leader: task_weak.clone(),
            inner: MPSafeCell::new(TaskStructInner {
                on_main_hart: false,
                nsproxy: Arc::new(NsProxy::new(IPCNamespace::new())),
                thread: ThreadStruct {
                    trap_ctx: trap_cx_addr,
                    task_ctx: TaskContext::goto_trap_return(kernel_stack_top),
                },
                group_leader: task_weak.clone(),
                kernel_stack,
                real_parent: Weak::new(),
                parent: Weak::new(),
                children: Vec::new(),
                pgid: pid_handle.0,
                sid: pid_handle.0,
                state: TaskStatus::Ready,
                exit_state: 0,
                exit_code: 0,
                exit_signal: 0,
                flags: 0,
                errno: 0,
                sched_policy: SCHED_IDLE, // initproc默认用SCHED_IDLE策略
                sched_priority: 0,
                mm: Some(Arc::new(MPSafeCell::new(memory_set))),
                fs: Arc::new(MPSafeCell::new(FsStruct::new(ROOT_DENTRY.clone(), ROOT_DENTRY.clone()))),
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
                start_time: get_time_us() as u64,
                start_boottime: get_time_us() as u64,
                on_cpu: false,
                on_rq: false,
                cpu: 0,
                clear_child_tid: 0,
                personality: 0,
                locked_bytes: 0,
                comm: [0; 10],
            }),
        });
        
        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        // 发现问题：这样解引用写入会炸
        // 已解决：只分配了256MB内存，之前的实现写到了有效区之外
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            initial_user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        debug!("TaskControlBlock::new: finished creating a new process");
        //println!("[kernel] TaskControlBlock::new: created init process with PID {}, main thread TID {}, entry_point={:#x}", proc_control_block.getpid(), task_control_block.gettid(), entry_point);
        // 返回PCB和主线程
        task_control_block
    }
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, TaskStructInner> {
        self.inner.exclusive_access()
    }
    pub fn process(self: &Arc<Self>) -> Arc<Self> {
        self.group_leader
            .upgrade()
            .unwrap_or_else(|| Arc::clone(self))
    }
    pub fn exec(
        self: &Arc<Self>,
        caller_task: Arc<TaskControlBlock>,
        elf_data: &[u8],
        args: Vec<String>,
        envs: Vec<String>,
        on_main_hart: bool,
    ) {

        const AT_BASE: usize = 7;
        const AT_PHDR: usize = 3;
        const AT_PHENT: usize = 4;
        const AT_PHNUM: usize = 5;
        const AT_PAGESZ: usize = 6;
        const AT_ENTRY: usize = 9;
        const AT_RANDOM: usize = 25;

        fn prepare_stack_pages(memory_set: &mut MemorySet, start: usize, end: usize) {
            let mut page = start / PAGE_SIZE * PAGE_SIZE;
            while page < end {
                memory_set.handle_page_fault(page, end);
                page += PAGE_SIZE;
            }
        }

        let cwd = self.inner_exclusive_access().fs.exclusive_access().get_pwd();
        let mut has_interp = false;
        let Some((
            mut memory_set,
            _heap_bottom,
            mut user_sp,
            final_entry_point,
            main_entry_point,
            phdr_addr,
            phnum,
            phent,
            interp_base,
        )) = MemorySet::from_elf_with_interp_loader(elf_data, |interp_path| {
            open_file(cwd.clone(), interp_path, OpenFlags::RDONLY, 0).map(|inode| {
                has_interp = true;
                inode.read_all()
            })
        }) else {
            return;
        };

        #[cfg(target_arch = "riscv64")]
        let (trap_cx_addr, kernel_stack_top) = {
            let trap_cx_addr = caller_task.inner_exclusive_access().thread.trap_ctx;
            let trap_cx_va = VirtAddr::from(trap_cx_addr);
            let trap_cx_ppn = KERNEL_SPACE
                .exclusive_access()
                .translate(trap_cx_va.std_floor())
                .expect("kernel TrapContext is not mapped")
                .ppn();
            memory_set.install_trap_context_page(trap_cx_va, trap_cx_ppn);
            (trap_cx_addr, trap_cx_addr)
        };

        #[cfg(target_arch = "loongarch64")]
        let (trap_cx_addr, kernel_stack_top) = {
            let inner = caller_task.inner_exclusive_access();
            (inner.thread.trap_ctx, inner.thread.trap_ctx)
        };

        let token = memory_set.token();
        let mut argv_ptrs = Vec::with_capacity(args.len());
        let arg_size = args.iter().map(|arg| arg.len() + 1).sum::<usize>();
        if arg_size != 0 {
            prepare_stack_pages(&mut memory_set, user_sp - arg_size, user_sp);
        }
        for arg in &args {
            user_sp -= arg.len() + 1;
            for (offset, byte) in arg.as_bytes().iter().enumerate() {
                translated_write(token, (user_sp + offset) as *mut u8, *byte);
            }
            translated_write(token, (user_sp + arg.len()) as *mut u8, 0);
            argv_ptrs.push(user_sp);
        }

        let mut envp_ptrs = Vec::with_capacity(envs.len());
        let env_size = envs.iter().map(|env| env.len() + 1).sum::<usize>();
        if env_size != 0 {
            prepare_stack_pages(&mut memory_set, user_sp - env_size, user_sp);
        }
        for env in &envs {
            user_sp -= env.len() + 1;
            for (offset, byte) in env.as_bytes().iter().enumerate() {
                translated_write(token, (user_sp + offset) as *mut u8, *byte);
            }
            translated_write(token, (user_sp + env.len()) as *mut u8, 0);
            envp_ptrs.push(user_sp);
        }

        user_sp -= 16;
    prepare_stack_pages(&mut memory_set, user_sp, user_sp + 16);
        let random_at = user_sp;
        for offset in 0..16 {
            translated_write(token, (random_at + offset) as *mut u8, 0x23);
        }
        user_sp -= user_sp % core::mem::size_of::<usize>();

        let mut auxv = vec![
            (AT_PHDR, phdr_addr),
            (AT_PHENT, phent),
            (AT_PHNUM, phnum),
            (AT_PAGESZ, PAGE_SIZE),
            (AT_ENTRY, main_entry_point),
            (AT_RANDOM, random_at),
        ];
        if has_interp {
            if let Some(base) = interp_base {
                auxv.push((AT_BASE, base));
            }
        }
        auxv.push((0, 0));

        let word_size = core::mem::size_of::<usize>();
        let metadata_size = (auxv.len() * 2
            + envp_ptrs.len() + 1
            + argv_ptrs.len() + 1
            + 1)
            * word_size;
        user_sp -= (user_sp - metadata_size) % 16;
        prepare_stack_pages(&mut memory_set, user_sp - metadata_size, user_sp);

        for (id, value) in auxv.iter().rev() {
            user_sp -= word_size;
            translated_write(token, user_sp as *mut usize, *value);
            user_sp -= word_size;
            translated_write(token, user_sp as *mut usize, *id);
        }

        user_sp -= word_size;
        translated_write(token, user_sp as *mut usize, 0usize);
        for env_ptr in envp_ptrs.iter().rev() {
            user_sp -= word_size;
            translated_write(token, user_sp as *mut usize, *env_ptr);
        }

        user_sp -= word_size;
        translated_write(token, user_sp as *mut usize, 0usize);
        for arg_ptr in argv_ptrs.iter().rev() {
            user_sp -= word_size;
            translated_write(token, user_sp as *mut usize, *arg_ptr);
        }
        let argv_base = user_sp;
        user_sp -= word_size;
        translated_write(token, user_sp as *mut usize, args.len());

        let mut trap_cx = TrapContext::app_init_context(
            final_entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        trap_cx.set_a0(args.len());
        trap_cx.set_a1(argv_base);

        let old_signal = {
            let mut inner = caller_task.inner_exclusive_access();
            let old_signal = inner.signal.clone();
            inner.thread.trap_ctx = trap_cx_addr;
            inner.mm = Some(Arc::new(MPSafeCell::new(memory_set)));
            inner.on_main_hart = on_main_hart;
            inner.signal = Arc::new(MPSafeCell::new(Signal::fork_from(
                &old_signal.exclusive_access(),
            )));
            inner.signal_hand = Arc::new(MPSafeCell::new(SigHand::new()));
            inner.pending = Sigpending::new();
            inner.signal_interrupted = false;
            inner.signal_mask_backup.clear();
            inner.trap_ctx_backup.clear();
            inner.signal_user_context_backup.clear();
            inner.term_signal = None;
            inner.frozen = false;
            inner.clear_child_tid = 0;
            inner.start_time = get_time_us() as u64;
            if let Some(argv0) = args.first() {
                inner.comm = [0; 10];
                let bytes = argv0.as_bytes();
                let copy_len = bytes.len().min(inner.comm.len() - 1);
                inner.comm[..copy_len].copy_from_slice(&bytes[..copy_len]);
            }
            *inner.get_trap_cx() = trap_cx;
            inner.files.clone()
        };
        {
            let mut files = old_signal.exclusive_access();
            for fd in 0..files.fds.len() {
                if files.fds[fd].flags.contains(FdFlags::CLOEXEC) {
                    files.clear_fd(fd);
                }
            }
        }

        let sibling_tasks = {
            let tasks = TID2TCB.exclusive_access();
            tasks
                .values()
                .filter(|task| task.gettgid() == caller_task.gettgid())
                .filter(|task| !Arc::ptr_eq(task, &caller_task))
                .cloned()
                .collect::<Vec<_>>()
        };
        for sibling in sibling_tasks {
            let mut sibling_inner = sibling.inner_exclusive_access();
            #[cfg(target_arch = "riscv64")]
            if let Some(mm) = sibling_inner.mm.as_ref() {
                mm.exclusive_access().remove_trap_context_page(VirtAddr::from(
                    sibling_inner.thread.trap_ctx,
                ));
            }
            sibling_inner.state = TaskStatus::Zombie;
            sibling_inner.mm.take();
            drop(sibling_inner);
            let _dispatch = lock_dispatch();
            remove_from_tid2task(sibling.gettid());
            remove_task_from_all_local_queues_unlocked(sibling.gettid());
            remove_task_from_global_pool_unlocked(sibling.gettid());
        }
    }
    /// clone 系统调用的核心实现。
    ///
    /// 参考 Linux kernel_clone()，根据 flags 控制父子任务之间的资源共享粒度。
    /// 接收者必须是 `&Arc<Self>`，以便通过 `Arc::downgrade` 建立父子关系和线程组关系。
    ///
    /// ## 参数
    /// - `flags`:   低 8 位为退出信号（CSIGNAL），高位为 CLONE_* 资源共享标志
    /// - `stack`:   子任务的用户栈顶；0 表示继承父任务的 sp
    /// - `ptid`:    父地址空间中的 `pid_t*`，由 `CLONE_PARENT_SETTID` 激活
    /// - `ctid`:    子地址空间中的 `pid_t*`，由 `CLONE_CHILD_SETTID` / `CLONE_CHILD_CLEARTID` 激活
    /// - `tls`:     TLS 指针，由 `CLONE_SETTLS` 激活
    ///
    /// ## 返回值
    /// 成功时向父任务返回子任务 PID；子任务从 `clone()` 返回 0（通过 trap 上下文中设置 a0=0 实现）。
    pub fn do_clone(self: &Arc<Self>, flags: usize, stack: usize, ptid: usize, ctid: usize, tls: usize) -> isize {
        // ── clone flags 常量 ──
        const CSIGNAL: usize = 0x000000ff;              // 退出信号掩码（低 8 位）
        const CLONE_VM: usize = 0x00000100;              // 共享地址空间
        const CLONE_FS: usize = 0x00000200;              // 共享 fs_struct（根目录/工作目录）
        const CLONE_FILES: usize = 0x00000400;           // 共享文件描述符表
        const CLONE_SIGHAND: usize = 0x00000800;         // 共享信号处理函数表
        const CLONE_SETTLS: usize = 0x00080000;          // 设置子任务 TLS 指针
        const CLONE_PARENT_SETTID: usize = 0x00100000;   // 向父地址空间写入子 TID
        const CLONE_CHILD_CLEARTID: usize = 0x00200000;  // 子任务退出时清零 ctid 并 futex 唤醒
        const CLONE_CHILD_SETTID: usize = 0x01000000;    // 向子地址空间写入 TID
        const CLONE_THREAD: usize = 0x00010000;           // 创建线程（共享 tgid）
        const CLONE_SYSVSEM: usize = 0x00040000;           // 共享 System V 信号量（todo）

        // ── 0. 判断是创建线程还是独立进程 ──
        let clone_thread = flags & CLONE_THREAD != 0;

        // ── 1. 获取父任务页表 token，用于后续 TID 指针可行性检查 ──
        let parent_token = {
            let inner = self.inner_exclusive_access();
            let Some(mm) = inner.mm.as_ref() else {
                return Errno::EINVAL.as_isize();
            };
            let mm_guard = mm.exclusive_access();
            mm_guard.token()
        };

        // ── 2. 参数合法性校验：检查 TID 指针对应的用户地址是否可写 ──
        // CLONE_PARENT_SETTID：内核在父地址空间中向 *ptid 写入子 TID
        if flags & CLONE_PARENT_SETTID != 0
            && (ptid == 0
                || !crate::mm::prepare_user_write(
                    parent_token,
                    ptid,
                    core::mem::size_of::<u32>(),
                ))
        {
            return Errno::EFAULT.as_isize();
        }
        // CLONE_CHILD_SETTID / CLONE_CHILD_CLEARTID：内核在子地址空间中写入或清零 *ctid
        if flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID) != 0
            && (ctid == 0
                || !crate::mm::prepare_user_write(
                    parent_token,
                    ctid,
                    core::mem::size_of::<u32>(),
                ))
        {
            return Errno::EFAULT.as_isize();
        }

        // ── 3. 获取父任务 inner 锁，开始复制/共享各类资源 ──
        let parent_inner = self.inner_exclusive_access();

        // 3a. 地址空间（mm）
        let parent_mm = parent_inner.mm.as_ref().unwrap().clone();
        let child_mm = if flags & CLONE_VM != 0 {
            // CLONE_VM：共享同一页表（父子使用同一个 MemorySet），用于线程
            parent_mm.clone()
        } else {
            // 无 CLONE_VM：写时复制（COW），创建独立的地址空间副本
            let mut parent_memory = parent_mm.exclusive_access();
            let child_memory = MemorySet::from_existed_user(&mut parent_memory);
            crate::arch::mm::flush_tlb_for_asid(parent_memory.asid());
            Arc::new(MPSafeCell::new(child_memory))
        };

        // 3b. 文件系统信息（fs_struct：根目录、当前目录、umask）
        let child_fs = if flags & CLONE_FS != 0 {
            // 共享 fs_struct
            parent_inner.fs.clone()
        } else {
            // 深拷贝
            Arc::new(MPSafeCell::new(parent_inner.fs.exclusive_access().clone()))
        };

        // 3c. 文件描述符表
        let child_files = if flags & CLONE_FILES != 0 {
            // 共享 fd 表（线程典型行为）
            parent_inner.files.clone()
        } else {
            // 深拷贝 fd 表
            Arc::new(MPSafeCell::new(parent_inner.files.exclusive_access().clone()))
        };

        // 3d. 信号处理函数表（SigHand）
        let child_signal_hand = if flags & CLONE_SIGHAND != 0 {
            // 共享信号处理函数（线程必须；Linux 要求 CLONE_SIGHAND ⇒ CLONE_VM）
            parent_inner.signal_hand.clone()
        } else {
            // 深拷贝
            Arc::new(MPSafeCell::new(parent_inner.signal_hand.exclusive_access().clone()))
        };

        // 3e. 信号状态（Signal：共享挂起信号、线程计数、rlimit）
        let child_signal = if clone_thread {
            // 线程：增加线程组引用计数，共享 Signal
            parent_inner.signal.exclusive_access().add_thread();
            parent_inner.signal.clone()
        } else {
            // 独立进程：从父进程 fork 新的 Signal（清空挂起信号，继承 rlimit）
            Arc::new(MPSafeCell::new(Signal::fork_from(
                &parent_inner.signal.exclusive_access(),
            )))
        };

        // 3f. 复制父任务 trap 上下文（寄存器快照），后续在子任务上修改 sp/a0 等
        let parent_trap_cx = *parent_inner.get_trap_cx();

        // 3g. 继承父进程的亲缘关系（线程继承，独立进程重置）
        let inherited_parent = parent_inner.parent.clone();           // 接收 SIGCHLD 的父进程
        let inherited_real_parent = parent_inner.real_parent.clone(); // 实际创建者

        // 3h. 继承调度、信号掩码、凭据等属性
        let sched_policy = parent_inner.sched_policy;
        let sched_priority = parent_inner.sched_priority;
        let blocked = parent_inner.blocked;               // 信号阻塞掩码
        let nsproxy = parent_inner.nsproxy.clone();       // 命名空间代理
        let cred = parent_inner.cred.clone();              // 有效凭据
        let real_cred = parent_inner.real_cred.clone();   // 真实凭据
        let personality = parent_inner.personality;
        let locked_bytes = parent_inner.locked_bytes;
        let comm = parent_inner.comm;
        let pgid = parent_inner.pgid;
        let sid = parent_inner.sid;
        drop(parent_inner); // 释放父任务锁，避免后续分配 PID/内核栈时持锁

        // ── 4. 分配新任务标识 ──
        let pid = Arc::new(pid_alloc());     // 全局唯一 PID
        let tgid = if clone_thread {
            // 线程：tgid 与父任务相同，getpid() 返回同一个值
            self.tgid.clone()
        } else {
            // 独立进程：pid == tgid
            pid.clone()
        };

        // ── 5. 分配内核栈，并在栈顶保留 trap 上下文 ──
        let kernel_stack = kstack_alloc();
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
        let kernel_stack_top = trap_cx_addr;

        // RISC-V 陷入时仍使用用户页表，因此只借用映射包含真实内核
        // TrapContext 的栈顶页；物理页始终由 KernelStack 所有。
        #[cfg(target_arch = "riscv64")]
        {
            let trap_cx_va = VirtAddr::from(trap_cx_addr);
            let trap_cx_ppn = KERNEL_SPACE
                .exclusive_access()
                .translate(trap_cx_va.std_floor())
                .expect("kernel TrapContext is not mapped")
                .ppn();
            let mut memory = child_mm.exclusive_access();
            memory.install_trap_context_page(trap_cx_va, trap_cx_ppn);
        }

        // ── 6. 构造子任务 TaskStruct ──
        let thread_group_leader = self.group_leader.clone();
        let child = Arc::new_cyclic(|child_weak| {
            // 线程组 leader：线程继承父的 group_leader，独立进程以自身为 leader
            let group_leader = if clone_thread {
                thread_group_leader.clone()
            } else {
                child_weak.clone()
            };
            TaskStruct {
                pid: pid.clone(),
                tgid: tgid.clone(),
                group_leader: group_leader.clone(),
                inner: MPSafeCell::new(TaskStructInner {
                    on_main_hart: false,
                    nsproxy,
                    thread: ThreadStruct {
                        trap_ctx: trap_cx_addr,
                        task_ctx: TaskContext::goto_trap_return(kernel_stack_top),
                    },
                    group_leader,
                    kernel_stack,
                    // 亲缘关系：线程继承父的 parent/real_parent，独立进程将 self 设为父
                    real_parent: if clone_thread {
                        inherited_real_parent
                    } else {
                        Arc::downgrade(self)
                    },
                    parent: if clone_thread {
                        inherited_parent
                    } else {
                        Arc::downgrade(self)
                    },
                    children: Vec::new(),
                    pgid,
                    sid,
                    state: TaskStatus::Ready,
                    exit_state: 0,
                    exit_code: 0,
                    exit_signal: (flags & CSIGNAL) as i32, // 退出时向父进程发送的信号
                    flags: 0,
                    errno: 0,
                    sched_policy,
                    sched_priority,
                    mm: Some(child_mm),
                    fs: child_fs,
                    files: child_files,
                    signal: child_signal,
                    signal_hand: child_signal_hand,
                    blocked,
                    pending: Sigpending::new(),             // 子任务的私有挂起信号为空
                    signal_interrupted: false,
                    signal_mask_backup: Vec::new(),
                    trap_ctx_backup: Vec::new(),
                    signal_user_context_backup: Vec::new(),
                    term_signal: None,
                    frozen: false,
                    cred,
                    real_cred,
                    start_time: get_time_us() as u64,
                    start_boottime: get_time_us() as u64,
                    on_cpu: false,
                    on_rq: false,
                    cpu: 0,
                    clear_child_tid: if flags & CLONE_CHILD_CLEARTID != 0 {
                        ctid  // 退出时清零此地址并 futex 唤醒
                    } else {
                        0
                    },
                    personality,
                    locked_bytes,
                    comm,
                }),
            }
        });

        // ── 7. 设置子任务的 trap 上下文 ──
        {
            let child_inner = child.inner_exclusive_access();
            let trap_cx = child_inner.get_trap_cx();
            // 从父任务复制寄存器快照
            *trap_cx = parent_trap_cx;
            // RISC-V：更新内核栈指针
            #[cfg(target_arch = "riscv64")]
            {
                trap_cx.kernel_sp = kernel_stack_top;
            }
            // 设置用户栈指针
            if stack != 0 {
                trap_cx.set_sp(stack);
            }
            // CLONE_SETTLS：将 TLS 写入线程指针寄存器（RISC-V: x4/tp, LoongArch: r2/tp）
            if flags & CLONE_SETTLS != 0 {
                trap_cx.set_tls(tls);
            }
            // ★ 关键：子任务从 clone() 返回 0
            trap_cx.set_a0(0);
        }

        // ── 8. 向用户空间写入 TID（按标志位分别处理） ──
        let child_tid = child.gettid() as u32;
        // CLONE_PARENT_SETTID：向父地址空间的 *ptid 写入子 TID
        if flags & CLONE_PARENT_SETTID != 0
            && !crate::mm::try_translated_write(parent_token, ptid as *mut u32, child_tid)
        {
            return Errno::EFAULT.as_isize();
        }
        // CLONE_CHILD_SETTID：向子地址空间的 *ctid 写入自身 TID
        if flags & CLONE_CHILD_SETTID != 0 {
            let child_token = {
                let child_inner = child.inner_exclusive_access();
                let child_memory = child_inner.mm.as_ref().unwrap().exclusive_access();
                child_memory.token()
            };
            if !crate::mm::try_translated_write(child_token, ctid as *mut u32, child_tid) {
                return Errno::EFAULT.as_isize();
            }
        }
        // 注意：CLONE_CHILD_CLEARTID 在步骤 6 中将 ctid 保存到 clear_child_tid 字段，
        // 实际的清零 + futex 唤醒操作在子任务退出时由 exit 路径完成。

        // ── 9. 建立父子关系并加入调度 ──
        // 只有独立进程才加入父进程的 children 链表（线程属于同一线程组，不需要）
        if !clone_thread {
            self.inner_exclusive_access().children.push(child.clone());
        }
        // 注册到全局 TID→TaskStruct 映射，并加入调度就绪队列
        warn!("do_clone: adding child task with PID {} and TID {}", child.getpid(), child.gettid());
        add_task(child);

        // 返回子任务的 PID（父任务视角）
        pid.0 as isize
    }
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
    pub fn get_policy_and_priority(&self) -> (isize, i32) {
        let inner = self.inner_exclusive_access();
        (inner.sched_policy, inner.sched_priority)
     }
    pub fn recycle_on_exit(&self, exit_code: i32) {
        remove_from_tid2task(self.gettid());

        let mut inner = self.inner_exclusive_access();
        #[cfg(target_arch = "riscv64")]
        if let Some(mm) = inner.mm.as_ref() {
            mm.exclusive_access()
                .remove_trap_context_page(VirtAddr::from(inner.thread.trap_ctx));
        }
        inner.exit_code = exit_code;
        inner.errno = 0;
        inner.state = TaskStatus::Zombie;
        inner.pending = Sigpending::new();
        inner.mm.take();
        let files = core::mem::replace(
            &mut inner.files,
            Arc::new(MPSafeCell::new(FileDescriptorTable::empty())),
        );
        drop(inner);
        drop(files);
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
    pub fs: Arc<MPSafeCell<FsStruct>>,       // 进程当前目录、根目录信息
    pub files: Arc<MPSafeCell<FileDescriptorTable>>, // 进程打开的文件描述符表

    /*7. 信号处理相关 */
    pub signal: Arc<MPSafeCell<Signal>>,  // 信号处理相关信息
    pub signal_hand: Arc<MPSafeCell<SigHand>>, // 信号处理函数相关信息
    pub blocked: SignalFlags, // 当前阻塞的信号集
    pub pending: Sigpending, // 当前挂起的信号集 
    pub signal_interrupted: bool,
    pub signal_mask_backup: Vec<SignalFlags>,
    pub trap_ctx_backup: Vec<TrapContext>,
    pub signal_user_context_backup: Vec<usize>,
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