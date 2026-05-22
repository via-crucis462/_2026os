//！ TODO：需要仔细核对并修改exec和fork的实现

use super::*;
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle, SignalActions, SignalFlags, TaskContext};
use schedule::*;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::{
    arch::trap::{TrapContext, trap_handler, trap_cx_va_by_kernel_stack},
    fs::{open_file, Dentry, File, OpenFlags, ROOT_DENTRY,Stdin, Stdout, Stderr},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, 
        translated_write, MapArea, MapPermission, MapType},
    sync::{MPSafeCell, WaitQueue},
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use crate::arch::{config::*, trap};
use crate::arch::mm::flush_tlb_for_asid;
use spin::Mutex;

const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct  Rlimit64 {
    pub cur_lmt: usize,
    pub max_lmt: usize,
}


#[derive(Clone)]
pub struct FileDescriptor {
    pub file: Option<Arc<dyn File + Send + Sync>>,
    pub cloexec: bool,
    pub status: usize,
}

impl FileDescriptor {
    pub fn empty() -> Self {
        Self {
            file: None,
            cloexec: false,
            status: 0,
        }
    }

    pub fn new(file: Arc<dyn File + Send + Sync>, cloexec: bool, status: usize) -> Self {
        Self {
            file: Some(file),
            cloexec,
            status,
        }
    }
}

pub struct ProcessControlBlock {
    pub pid: Arc<PidHandle>,
    pub oom_score_adj: AtomicI32,
    pub inner: MPSafeCell<ProcessControlBlockInner>,
}

impl ProcessControlBlock {
    /// 获取进程块的独占访问权限
    pub fn inner_exclusive_access(&self) -> MPSafeGuard<'_, ProcessControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// 用于创建初始化进程
    /// Create a new process
    /// 为la64修改
    /// At present, it is only used for the creation of initproc
    /// 现在会返回新创建的PCB及其主线程TCB（均为arc）
    pub fn new(elf_data: &[u8]) -> (Arc<Self>, Arc<TaskControlBlock>) {
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
        let tid_handle = Arc::new(tid_alloc());
        //println!("[kernel] TaskControlBlock::new: allocated TID {}", tid_handle.0);
        let kernel_stack = kstack_alloc();
        
        let trap_cx_va: VirtAddr;
        let trap_cx_addr: usize;
        let kernel_stack_top: usize;
        let initial_user_sp = user_sp;
        #[cfg(target_arch = "riscv64")]{
            // 异常上下文映射页，riscv会在进入跳板前将上下文压入用户地址空间，所以要在用户地址空间写入
            // 将用户空间的上下文与内核栈唯一映射的原因是：这样可以为每一个线程分配唯一的上下文栈
            // 什么？你说你问为什么不同的进程明明独立，但唯一标识符的异常上下文栈位置也一定不相同，这样做是不是有些粗糙
            // 答案是确实粗糙
            trap_cx_va = trap_cx_va_by_kernel_stack(&kernel_stack).into();
            info!("TaskControlBlock::new: calculated trap_cx_va = {:#x}", trap_cx_va.0);
            memory_set.push(
                MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                    MapType::Framed, MapPermission::R | MapPermission::W),
                None,
                trap_cx_va.0,
            );
            trap_cx_addr = {
                let trap_cx_ppn = memory_set
                    .translate(trap_cx_va.into())
                    .unwrap()
                    .ppn();
                let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
                trap_cx_pa.into()
            };

            // 内核栈顶地址，即切换到内核任务流后内核执行栈的初始值（内核sp）
            kernel_stack_top = kernel_stack.get_top();
            //println!("TaskControlBlock::new: calculated trap_cx_addr = {:#x}, kernel_stack_top = {:#x}, user_sp = {:#x}", trap_cx_addr, kernel_stack_top, initial_user_sp);
        }

       
        //info!("TaskControlBlock::new: translated trap_cx_addr = {:#x}", trap_cx_addr);
        #[cfg(target_arch = "loongarch64")]{
            // loongarch在进入跳板前会将上下文压入内核地址空间的内核栈，所以直接在内核栈上分配TrapContext即可
            trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
            // 由于loongarch会把异常上下文压入内核地址空间，所以会比riscv少一个trap_cx的映射页，因此内核栈顶地址即为trap_cx_addr
            kernel_stack_top = trap_cx_addr;
        }

        debug!("TaskControlBlock::new: kernel_stack_top={:#x}", kernel_stack.get_top());
        // 进程控制块
        let proc_control_block = Arc::new(ProcessControlBlock {
            pid: pid_handle.clone(),// 注意：实际上只克隆了指针
            oom_score_adj: AtomicI32::new(0),
            inner: MPSafeCell::new(ProcessControlBlockInner {
                on_main_hart: true, // initproc和shell默认在主核运行
                pname: String::from("initproc"),
                base_size: initial_user_sp,
                memory_set,
                parent: None,
                children: Vec::new(),
                heap_bottom: heap_bottom,
                program_brk: heap_bottom,
                fd_rlmt: Rlimit64 { cur_lmt: 1024, max_lmt: 1024 }, // 默认允许打开的最大文件描述符数量
                // 初始化 fd_table，预先放入 stdin 和 stdout
                fd_table: vec![
                    FileDescriptor::new(Arc::new(Stdin), false, 0),
                    FileDescriptor::new(Arc::new(Stdout), false, 0),
                    FileDescriptor::new(Arc::new(Stderr), false, 0),
                ],
                cwd: ROOT_DENTRY.clone(),
                signals: SignalFlags::empty(),
                signal_actions: SignalActions::default(),
                exit_code: 0,
                uid: 0,
                gid: 0,
                sid:0,
                euid: 0,
                egid: 0,
                umask: 0o022,
                pgid: pid_handle.0,
                alive_task_count: 0,
                tasks: Vec::new(),
            })
        });
        // 为pcb创建主线程
        let task_control_block = Arc::new(TaskControlBlock{
            process: Arc::downgrade(&proc_control_block),
            tid: tid_handle.clone(),
            kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                owner_hart: None,
                signal_mask: SignalFlags::empty(),
                handling_sig: -1,
                killed: false,
                signal_mask_backup: None,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: SignalFlags::empty(),
                clear_child_tid: 0,

            })
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
        
        proc_control_block.inner.exclusive_access().tasks.push(task_control_block.clone());
        //println!("[kernel] TaskControlBlock::new: created init process with PID {}, main thread TID {}, entry_point={:#x}", proc_control_block.getpid(), task_control_block.gettid(), entry_point);
        // 返回PCB和主线程
        (proc_control_block, task_control_block)
    }

        

    /// Load a new elf to replace the original application address space and 
    /// 待修改
    pub fn exec(self: &Arc<ProcessControlBlock>, caller_task: Arc<TaskControlBlock>, elf_data: &[u8], args: Vec<String>, envs: Vec<String>, on_main_hart: bool) {
        let cwd = self.inner_exclusive_access().cwd.clone();
        let mut has_interp = false;
        let Some((mut memory_set, heap_bottom, mut user_sp, final_entry_point, main_entry_point, phdr_addr, phnum, phent, interp_base)) =
            MemorySet::from_elf_with_interp_loader(elf_data, |interp_path| {
                debug!("[kernel] sys_exec: loading interpreter at '{}'", interp_path);
                open_file(cwd.clone(), interp_path, OpenFlags::RDONLY).map(|inode| {
                    has_interp = true;
                    inode.read_all()
                })
            }) else {
                return;
            };
        const AT_BASE: usize = 7;                // 辅助向量里代表解释器基址的 ID

        let trap_cx_addr: usize;
        let kernel_stack_top: usize;

        #[cfg(target_arch = "riscv64")]
        {
            let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(&caller_task.kernel_stack).into();
            memory_set.push(
                MapArea::new(
                    trap_cx_va,
                    VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                    MapType::Framed,
                    MapPermission::R | MapPermission::W,
                ),
                None,
                trap_cx_va.0,
            );
            trap_cx_addr = {
                let trap_cx_ppn = memory_set.translate(trap_cx_va.into()).unwrap().ppn();
                let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
                trap_cx_pa.into()
            };
            kernel_stack_top = caller_task.kernel_stack.get_top();
        }

        #[cfg(target_arch = "loongarch64")]
        {
            trap_cx_addr = caller_task.inner_exclusive_access().trap_cx_addr;
            kernel_stack_top = trap_cx_addr;
        }
        
        debug!(
            "[kernel] task::exec: entry_point={:#x}, user_sp={:#x}",
            final_entry_point, user_sp
        );

        
        let memory_top = heap_bottom;
        // 压入具体的字符串内容（高地址）
        let mut argv_ptrs: Vec<usize> = Vec::new();
        for arg in args.iter() {
            user_sp -= arg.len() + 1; // +1 是为了结尾的 '\0'
            let mut p = user_sp;
            for c in arg.as_bytes() {
                translated_write(memory_set.token(), p as *mut u8 , *c);
                p += 1;
            }
            translated_write(memory_set.token(), p as *mut u8, 0); // 写入结尾 0
            argv_ptrs.push(user_sp);
        }
        // 环境变量字符串也放在高地址区域，后续在指针区单独压入 envp[]
        let mut envp_ptrs: Vec<usize> = Vec::new();
        for env in envs.iter() {
            user_sp -= env.len() + 1; // +1 是为了结尾的 '\0'
            let mut p = user_sp;
            for c in env.as_bytes() {
                translated_write(memory_set.token(), p as *mut u8 , *c);
                p += 1;
            }
            translated_write(memory_set.token(), p as *mut u8, 0); // 写入结尾 0
            envp_ptrs.push(user_sp);
        }
        // 随机字符串 (AT_RANDOM 使用) 16 字节
        user_sp -= 16;
        let random_at = user_sp;
        for i in 0..16 {
            translated_write(memory_set.token(), (user_sp + i) as *mut u8, 0x23); // 任意填充
        }
        // 对齐栈指针
        user_sp -= user_sp % core::mem::size_of::<usize>();
        // 构造 AUX Vector
        let mut auxv = Vec::new();
        auxv.push((AT_PHDR, phdr_addr));
        auxv.push((AT_PHENT, phent));
        auxv.push((AT_PHNUM, phnum));
        auxv.push((AT_PAGESZ, 4096));
        auxv.push((AT_ENTRY, main_entry_point));
        auxv.push((AT_RANDOM, random_at));
         // AT_NULL
        // 压入 AUXV
        if has_interp {
            if let Some(interp_base) = interp_base {
                auxv.push((AT_BASE, interp_base));
            }
            info!(
                "exec: dynamic-link branch, injected AT_BASE={:#x}, first_jump={:#x}, file_entry={:#x}",
                interp_base.unwrap_or(0),
                final_entry_point,
                main_entry_point
            );
        } else {
            info!(
                "exec: fallback branch, no AT_BASE, first_jump={:#x}, file_entry={:#x}",
                final_entry_point,
                main_entry_point
            );
        }
        auxv.push((0, 0));
        for (id, val) in auxv.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            translated_write(memory_set.token(), user_sp as *mut usize, *val);
            user_sp -= core::mem::size_of::<usize>();
            translated_write(memory_set.token(), user_sp as *mut usize, *id);
        }
        // 压入 envp 数组：压入一个 NULL (0) 作为结尾
        user_sp -= core::mem::size_of::<usize>();
        translated_write(memory_set.token(), user_sp as *mut usize, 0usize);
        // 逆序压入 envp 的指针
        for env_ptr in envp_ptrs.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            translated_write(memory_set.token(), user_sp as *mut usize, *env_ptr);
        }
        // 压入 argv 数组：先压入一个 NULL (0) 作为结尾
        user_sp -= core::mem::size_of::<usize>();
        translated_write(memory_set.token(), user_sp as *mut usize, 0usize);
        // 逆序压入 argv 的指针
        for arg_ptr in argv_ptrs.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            translated_write(memory_set.token(), user_sp as *mut usize, *arg_ptr);
        }
        // 此时 user_sp 即为 argv[0] 的地址
        let argv_base = user_sp;
        // 压入 argc
        user_sp -= core::mem::size_of::<usize>();
        translated_write(memory_set.token(), user_sp as *mut usize, args.len());
        // 调整锁序：先拿tcb锁再拿pcb锁，避免死锁
        let mut task_inner = caller_task.inner_exclusive_access();
        // 更新 PCB 内部信息
        let mut proc_inner = self.inner_exclusive_access();
        for fd in 0..proc_inner.fd_table.len() {
            if proc_inner.fd_table[fd].cloexec {
                proc_inner.clear_fd(fd);
            }
        }
        proc_inner.base_size = memory_top;
        proc_inner.heap_bottom = memory_top;
        proc_inner.program_brk = memory_top;
        proc_inner.on_main_hart = on_main_hart;
        proc_inner.signal_actions = SignalActions::default();
        if let Some(argv0) = args.first() {
            proc_inner.pname = argv0.clone();
        }
        // 内核栈无须改变（fork时已经分配了新的）但需要重新映射
        proc_inner.memory_set = memory_set;

        // 修改trap上下文
        let mut trap_cx = TrapContext::app_init_context(
            final_entry_point,
            user_sp, // 让用户程序一进来 sp 就指向 argc
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );

        // 虽然简单用户库入口会直接使用 a0/a1，但标准 ELF 启动更依赖 sp 指向的初始栈。
        // 这里先保留现有行为，并把最终交给用户态的入口现场打出来，便于对比 RV/LA。
        trap_cx.set_a0(args.len());
        trap_cx.set_a1(argv_base);

        // 更新tcb信息
        task_inner.trap_cx_addr = trap_cx_addr;
        *task_inner.get_trap_cx() = trap_cx;

        // 删除其他线程（如果有）
        proc_inner.tasks.retain(|t: &Arc<TaskControlBlock>| Arc::ptr_eq(t, &caller_task));
        proc_inner.alive_task_count = 1;
        /*for i in proc_inner.memory_set.areas().iter() {
            println!("exec: map_area: [{:#x}, {:#x})", i.get_vpn_range().get_start().0, i.get_vpn_range().get_end().0);
        }*/
        
    }


    /// Fork from parent to child
    /// 已编辑，添加了stack参数 
    /// 现在会返回新创建的PCB及其主线程TCB（均为arc）
    pub fn fork(self: &Arc<ProcessControlBlock>, sp: Option<usize>, caller_task: Arc<TaskControlBlock>)-> (Arc<Self>, Arc<TaskControlBlock>) {
        // fix:锁序调整，先拿tcb锁再拿pcb锁
        let caller_inner = caller_task.inner_exclusive_access();
        // ---- hold parent PCB lock
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let mut memory_set = MemorySet::from_existed_user(&mut parent_inner.memory_set);
        flush_tlb_for_asid(parent_inner.memory_set.asid());
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = Arc::new(pid_alloc());
        //println!("[kernel] TaskControlBlock::fork: allocated PID {}", pid_handle.0);
        let tid_handle = Arc::new(tid_alloc());
        //println!("[kernel] TaskControlBlock::fork: allocated TID {}", tid_handle.0);
        let kernel_stack = kstack_alloc();
        let trap_cx_addr: usize;
        #[cfg(target_arch = "riscv64")]{
        let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(&kernel_stack).into();
            memory_set.push(
                MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                    MapType::Framed, MapPermission::R | MapPermission::W),
                None,
                trap_cx_va.0,
            );
        trap_cx_addr = {
            let trap_cx_ppn = memory_set
                .translate(trap_cx_va.into())
                .unwrap()
                .ppn();
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            let trap_cx_addr: usize = trap_cx_pa.into();
            trap_cx_addr
            };
        }
        
        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        // copy fd table
        let new_fd_table = parent_inner.fd_table.clone();
        //println!("[kernel] ProcessControlBlock::fork: copied fd_table with {} entries", new_fd_table.len());
        let proc_control_block = Arc::new(ProcessControlBlock {
            pid: pid_handle.clone(),
            oom_score_adj: AtomicI32::new(self.oom_score_adj.load(Ordering::SeqCst)),
            inner: MPSafeCell::new(ProcessControlBlockInner {
                on_main_hart: false, 
                pname: parent_inner.pname.clone(),
                base_size: parent_inner.base_size,
                memory_set,
                parent: Some(Arc::downgrade(self)),
                children: Vec::new(),
                heap_bottom: parent_inner.heap_bottom,
                program_brk: parent_inner.program_brk,
                fd_table: new_fd_table,
                cwd: parent_inner.cwd.clone(),
                signals: SignalFlags::empty(),
                signal_actions: parent_inner.signal_actions.clone(),
                exit_code: 0,
                uid: parent_inner.uid,
                gid: parent_inner.gid,
                euid: parent_inner.euid,
                umask: parent_inner.umask, 
                sid:parent_inner.sid,
                egid: parent_inner.egid,
                pgid: parent_inner.pgid,
                fd_rlmt: parent_inner.fd_rlmt.clone(),
                tasks: Vec::new(),
                alive_task_count: 1, // 初始有一个线程
            })
        });
        let new_task = Arc::new(TaskControlBlock {
            process: Arc::downgrade(&proc_control_block),
            tid: tid_handle.clone(),
            kernel_stack: kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                signal_mask_backup: None,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                owner_hart: None,
                signal_mask: caller_inner.signal_mask,
                handling_sig: caller_inner.handling_sig,
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: SignalFlags::empty(),
                clear_child_tid: 0,
            }),
        });
        //println!("fork: created new task with tid {}", new_task.gettid());
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
            *trap_cx = *caller_inner.get_trap_cx();
        #[cfg(target_arch = "riscv64")]
        {
            trap_cx.kernel_sp = kernel_stack_top;
        }
        if let Some(sp) = sp {
            trap_cx.set_sp(sp);
        }
        // 把任务加入进程的线程列表
        proc_control_block.inner.exclusive_access().tasks.push(new_task.clone());
        // add child
        parent_inner.children.push(proc_control_block.clone());
        // return
        (proc_control_block, new_task)
        // **** release child PCB
        // ---- release parent PCB
    }

    pub fn clone_thread(
        self: &Arc<ProcessControlBlock>,
        stack: Option<usize>,
        caller_task: Arc<TaskControlBlock>,
    ) -> Arc<TaskControlBlock> {
        let tid_handle = Arc::new(tid_alloc());
        let kernel_stack = kstack_alloc();

        let trap_cx_addr: usize;
        #[cfg(target_arch = "riscv64")]
        {
            let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(&kernel_stack).into();
            let mut proc_inner = self.inner_exclusive_access();
            proc_inner.memory_set.push(
                MapArea::new(
                    trap_cx_va,
                    VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                    MapType::Framed,
                    MapPermission::R | MapPermission::W,
                ),
                None,
                trap_cx_va.0,
            );
            trap_cx_addr = {
                let trap_cx_ppn = proc_inner
                    .memory_set
                    .translate(trap_cx_va.into())
                    .unwrap()
                    .ppn();
                let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
                trap_cx_pa.into()
            };
        }

        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        let caller_inner = caller_task.inner_exclusive_access();
        let new_task = Arc::new(TaskControlBlock {
            process: Arc::downgrade(self),
            tid: tid_handle,
            kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                owner_hart: None,
                exit_code: 0,
                signals: SignalFlags::empty(),
                signal_mask: caller_inner.signal_mask,
                handling_sig: caller_inner.handling_sig,
                signal_mask_backup: None,
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                clear_child_tid: 0,
            }),
        });

        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
        *trap_cx = *caller_inner.get_trap_cx();
        #[cfg(target_arch = "riscv64")]
        {
            trap_cx.kernel_sp = kernel_stack_top;
        }
        if let Some(stack) = stack {
            trap_cx.set_sp(stack);
        }
        trap_cx.set_a0(0);
        drop(caller_inner);

        let mut proc_inner = self.inner_exclusive_access();
        proc_inner.tasks.push(new_task.clone());
        proc_inner.alive_task_count += 1;
        drop(proc_inner);

        new_task
    }

    /// get pid of process
    pub fn getpid(&self) -> usize {
        self.pid.0
    }

    /// 获取parent的pid
    pub fn getppid(&self) -> usize {
        let inner = self.inner_exclusive_access();
        if let Some(parent_weak) = &inner.parent{
            if let Some(parent) = parent_weak.upgrade(){
                parent.pid.0
            } else {
                0
            }
        } else {
            0
        }
    
    }

    /// change the location of the program break. return None if failed.
    /// rcore自带, 修改断点（增量形式）
    pub fn change_program_brk(&self, addr: usize) -> Result<usize, i32> {
        if addr == 0{
            // 返回当前断点
            return Ok(self.inner_exclusive_access().program_brk);
        }
        // 超范围panic
        let size: isize = addr as isize - self.inner_exclusive_access().program_brk as isize;
        let mut inner = self.inner_exclusive_access();
        let heap_bottom = inner.memory_set.areas()[inner.memory_set.brk_index()].get_vpn_range().get_start().0 * PAGE_SIZE;
        //let heap_bottom = inner.heap_bottom;
        debug!("change_program_brk: addr={:#x}, current_brk={:#x}, current_heap_bottom={:#x}, size={}", addr, inner.program_brk, heap_bottom, size);
        let _old_break = inner.program_brk;
        let new_brk = addr as isize;
        if new_brk < heap_bottom as isize {
            return Err(-1);
        }
        let result = if size < 0 {
            debug!("change_program_brk: before shrink_to, heap_bottom={:#x}, new_brk={:#x}", heap_bottom, new_brk);
            inner
                .memory_set
                .shrink_to(VirtAddr(heap_bottom), VirtAddr(new_brk as *const () as usize))
        } else {
            debug!("change_program_brk: before append_to, heap_bottom={:#x}, new_brk={:#x}", heap_bottom, new_brk);
            for i in inner.memory_set.areas().iter() {
                trace!("change_program_brk: map_area: [{:#x}, {:#x})", i.get_vpn_range().get_start().0, i.get_vpn_range().get_end().0);
            }
            inner
                .memory_set
                .append_to(VirtAddr(heap_bottom), VirtAddr(new_brk as *const () as usize));
            
            for i in inner.memory_set.areas().iter() {
                trace!("change_program_brk: map_area: [{:#x}, {:#x})", i.get_vpn_range().get_start().0, i.get_vpn_range().get_end().0);
            }
             true
        };
        //println!("brk: change from {:#x} to {:#x}", _old_break, new_brk);
        if result {
            inner.program_brk = new_brk as *const () as usize;
            Ok(addr)
        } else {
            Err(-1)
        }
    }
    /// 处理mmap
    pub fn mmap(
        &self,
        addr: usize,
        length: usize,
        prot: mmap::MMapProt,
        flags: mmap::MMapFlags,
        file_inner: Option<Arc<dyn File + Send + Sync>>,
        offset: usize,
    ) -> Result<usize, i32> {
        let mut inner = self.inner_exclusive_access();
        inner.memory_set.mmap(addr, length, prot, flags, file_inner, offset)
    }
    /// 处理munmap
    pub fn munmap(&self, addr: usize, length: usize) -> Result<(), i32> {
        let mut inner = self.inner_exclusive_access();
        inner.memory_set.munmap(addr, length)
    }
}

pub struct ProcessControlBlockInner {
    // 进程是否在主核运行，initproc和shell默认在主核运行
    pub on_main_hart: bool,

    pub pname: String,

    /// Application data can only appear in areas
    /// where the application address space is lower than base_size
    pub base_size: usize,

    /// Application address space
    pub memory_set: MemorySet,

    /// Parent process of the current process.
    /// Weak will not affect the reference count of the parent
    pub parent: Option<Weak<ProcessControlBlock>>,

    /// A vector containing TCBs of all child processes of the current process
    pub children: Vec<Arc<ProcessControlBlock>>,

    /// Heap bottom
    pub heap_bottom: usize,

    /// Program break
    pub program_brk: usize, // 注意需要在exec中维护，rcore忽略了这点，运行测例时brk失效，已修复

    pub fd_rlmt: Rlimit64, // cur_lmt, max_lmt

    pub fd_table: Vec<FileDescriptor>,
    
    pub cwd: Arc<Dentry>, // 当前工作目录

    // 进程收到的信号
    pub signals: SignalFlags,

    // Signal actions
    pub signal_actions: SignalActions,

    pub exit_code: i32, // 进程退出码，默认为0，只有当进程状态为Zombie时才有意义

    pub uid: u32,  // 用户 ID
    pub gid: u32,  // 用户组 ID
    pub euid: u32, // 有效用户 ID
    pub egid: u32, // 有效用户组 ID
    pub umask: u32, // 文件模式创建掩码

    pub sid: usize,
    pub pgid: usize, // 进程组 ID
    
    // 进程下的线程数
    pub tasks: Vec<Arc<TaskControlBlock>>, 
    // 存活进程数，等于0相当于僵尸进程
    pub alive_task_count: isize,
}

impl ProcessControlBlockInner {
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    pub fn get_asid(&self) -> usize {
        self.memory_set.asid()
    }
    pub fn alloc_fd(&mut self) -> Option<usize> {
        // 1. 先尝试在现有的表中寻找被 close 空出来的坑位
        if let Some(fd) = (0..self.fd_table.len()).find(|fd| self.fd_table[*fd].file.is_none()) {
            self.fd_table[fd].cloexec = false;
            self.fd_table[fd].status = 0;
            return Some(fd);
        } 
        
        // 2. 如果没有空闲坑位，检查是否已经达到上限
        if self.fd_table.len() >= self.fd_rlmt.cur_lmt {
            return None; // 拒绝分配，触发 EMFILE
        }
        
        // 3. 没到上限，扩充 fd_table
        self.fd_table.push(FileDescriptor::empty());
        Some(self.fd_table.len() - 1)
    }
    pub fn clear_fd(&mut self, fd: usize) {
        self.fd_table[fd] = FileDescriptor::empty();
    }
    pub fn set_fd(
        &mut self,
        fd: usize,
        file: Arc<dyn File + Send + Sync>,
        cloexec: bool,
        status: usize,
    ) {
        self.fd_table[fd] = FileDescriptor::new(file, cloexec, status);
    }
    /// 回收被close的fd，压缩fd_table
    pub fn recycle_fd(&mut self) {
        self.fd_table.retain(|fd| fd.file.is_some());
    }
    pub fn get_rlimit64(&self) -> Rlimit64 {
        self.fd_rlmt.clone()
    }
    pub fn set_rlimit64(&mut self, new_rlmt: Rlimit64) {
        self.fd_rlmt = new_rlmt;
    }
    pub fn is_zombie(&self) -> bool {
        self.alive_task_count == 0
    }
    pub fn info_map_areas(&self) {
            println!("mapping asid {}:", self.get_asid());
        for i in self.memory_set.areas().iter() {
            println!("mapping: {:#x} -> {:#x}; permission: {:?}", i.get_vpn_range().get_start().0, i.get_vpn_range().get_end().0, i.get_map_permission());
        }
    }
}

