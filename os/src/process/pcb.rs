#![allow(unused)]
use super::*;
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, IdHandle, SignalActions, SignalFlags, TaskContext};
use schedule::*;

use crate::{
    arch::trap::{TrapContext, trap_handler, current_trap_cx_user_va, trap_cx_va_by_tid},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, translated_refmut},
    sync::MPSafeCell,
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use crate::arch::config::*;

const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;

pub struct ProcessControlBlock {
    pub pid: Arc<IdHandle>,
    pub inner: MPSafeCell<ProcessControlBlockInner>,
}

impl ProcessControlBlock {
    pub fn inner_exclusive_access(&self) -> spin::MutexGuard<'_, ProcessControlBlockInner> {
        self.inner.exclusive_access()
    }
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        println!("[kernel] TaskControlBlock::new: start creating a new process");
        let (memory_set, user_sp, entry_point, _phdr, _phnum, _phent)
            = MemorySet::from_elf(elf_data);
        debug!(
            "TaskControlBlock::new: entry_point={:#x}, user_sp={:#x}",
            entry_point, user_sp
        );
        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            trap_cx_pa.into()
        };
        // alloc a pid and a kernel stack in kernel space
        // 注意：push_on_top已经被修改，请及时改回！！！！！！！！！！！！！！！！！！！！！！！！！！！！！！
        let pid_handle = Arc::new(pid_alloc());
        let tid_handle = Arc::new(tid_alloc());
        let kernel_stack = kstack_alloc();

        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;

        debug!("TaskControlBlock::new: kernel_stack_top={:#x}", kernel_stack.get_top());

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        // 进程控制块
        let proc_control_block = Arc::new(ProcessControlBlock {
            pid: pid_handle.clone(),// 注意：实际上只克隆了指针
            inner: MPSafeCell::new(ProcessControlBlockInner {
                pname: String::from("initproc"),
                base_size: user_sp,
                memory_set,
                parent: None,
                children: Vec::new(),
                heap_bottom: user_sp,
                program_brk: user_sp,
                // 初始化 fd_table，预先放入 stdin 和 stdout
                fd_table: vec![Some(Arc::new(Stdin)), Some(Arc::new(Stdout))],
                cwd: ROOT_DENTRY.clone(),
                uid: 0,
                gid: 0,
                euid: 0,
                egid: 0,
                clear_child_tid: 0,
                tasks: Vec::new(),
            })
        });
        // 为pcb创建主线程
        let task_control_block = TaskControlBlock{
            process: Arc::downgrade(&proc_control_block),
            tid: tid_handle.clone(),
            kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                signal_mask: SignalFlags::empty(),
                handling_sig: -1,
                signal_actions: SignalActions::default(),
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: SignalFlags::empty(),

            })
        };

        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        // 发现问题：这样解引用写入会炸
        // 已解决：只分配了256MB内存，之前的实现写到了有效区之外
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        debug!("TaskControlBlock::new: finished creating a new process");
        proc_control_block.inner.exclusive_access().tasks.push(Arc::new(task_control_block));
        // 返回PCB
        proc_control_block
    }

        /// Create a new process
    /// 为la64修改
    /// At present, it is only used for the creation of initproc
    

    /// Load a new elf to replace the original application address space and 
    /// 待修改
    pub fn exec(self: &Arc<ProcessControlBlock>, elf_data: &[u8], args: Vec<String>) {
        // 1. 加载 ELF 文件生成新的地址空间
        let (memory_set, mut user_sp, entry_point, phdr_addr, phnum, phent) = MemorySet::from_elf(elf_data);
        debug!(
            "[kernel] task::exec: entry_point={:#x}, user_sp={:#x}",
            entry_point, user_sp
        );
        #[cfg(target_arch = "riscv64")]
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(current_trap_cx_user_va()).into())
            .unwrap()
            .ppn();
        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            let trap_cx_addr = trap_cx_pa.0;
            trap_cx_addr
        };
        let memory_top = user_sp;

        // --- 开始构造符合 ABI 标准的用户栈 ---
        
        // 2. 首先压入具体的字符串内容（高地址）
        let mut argv_ptrs: Vec<usize> = Vec::new();
        for arg in args.iter() {
            user_sp -= arg.len() + 1; // +1 是为了结尾的 '\0'
            let mut p = user_sp;
            for c in arg.as_bytes() {
                *translated_refmut(memory_set.token(), p as *mut u8) = *c;
                p += 1;
            }
            *translated_refmut(memory_set.token(), p as *mut u8) = 0; // 写入结尾 0
            argv_ptrs.push(user_sp);
        }

        // 随机字符串 (AT_RANDOM 使用) 16 字节
        user_sp -= 16;
        let random_at = user_sp;
        for i in 0..16 {
            *translated_refmut(memory_set.token(), (user_sp + i) as *mut u8) = 0x23; // 任意填充
        }

        // 3. 对齐栈指针到 8 字节
        user_sp -= user_sp % core::mem::size_of::<usize>();

        // --- 构造 AUX Vector ---
        let mut auxv = Vec::new();
        auxv.push((AT_PHDR, phdr_addr));
        auxv.push((AT_PHENT, phent));
        auxv.push((AT_PHNUM, phnum));
        auxv.push((AT_PAGESZ, 4096));
        auxv.push((AT_ENTRY, entry_point));
        auxv.push((AT_RANDOM, random_at));
        auxv.push((0, 0)); // AT_NULL

        // 压入 AUXV
        for (id, val) in auxv.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            *translated_refmut(memory_set.token(), user_sp as *mut usize) = *val;
            user_sp -= core::mem::size_of::<usize>();
            *translated_refmut(memory_set.token(), user_sp as *mut usize) = *id;
        }

        // 4. 压入 envp 数组：目前只压入一个 NULL (0)
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = 0;

        // 5. 压入 argv 数组：先压入一个 NULL (0) 作为结尾
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = 0;

        // 逆序压入 argv 的指针
        for arg_ptr in argv_ptrs.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            *translated_refmut(memory_set.token(), user_sp as *mut usize) = *arg_ptr;
        }

        // 此时 user_sp 即为 argv[0] 的地址
        let argv_base = user_sp;

        // 6. 最后压入 argc
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = args.len();

        // --- 压栈结束，此时 user_sp 指向 argc ---

        // 7. 更新 PCB 内部信息
        let mut inner = self.inner_exclusive_access();
        inner.memory_set = memory_set;
    
        inner.heap_bottom = memory_top;
        inner.program_brk = memory_top;

        let kernel_stack = kstack_alloc();
        
        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = inner.trap_cx_addr;

        
        // 8. 设置初始异常上下文 (TrapContext)
        let mut trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp, // 让用户程序一进来 sp 就指向 argc
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        
        // 虽然 crt.S 会用 sp 覆盖 a0，但我们还是按照惯例填好 a0 和 a1
        trap_cx.set_a0(args.len());
        trap_cx.set_a1(argv_base);
        
        //*inner.get_trap_cx() = trap_cx;

        let new_task = TaskControlBlock {
            process: Arc::downgrade(self),
            tid: Arc::new(tid_alloc()),
            kernel_stack: kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                signal_mask: SignalFlags::empty(),
                handling_sig: -1,
                signal_actions: SignalActions::default(),
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: SignalFlags::empty(),
            }),
        };

        let task_inner = new_task.inner_exclusive_access();
        *task_inner.get_trap_cx() = trap_cx;
        
    }

    /// Fork from parent to child
    /// 已编辑，添加了stack参数 
    pub fn fork(self: &Arc<ProcessControlBlock>, caller_task: Arc<TaskControlBlock>, sp: Option<usize>) -> Arc<Self> {
        // ---- hold parent PCB lock
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let memory_set = MemorySet::from_existed_user(&parent_inner.memory_set);
        let pid_handle = Arc::new(pid_alloc());
        let tid_handle = Arc::new(tid_alloc());
        #[cfg(target_arch = "riscv64")]
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(trap_cx_va_by_tid(tid_handle.0)).into())
            .unwrap()
            .ppn();
        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            let trap_cx_addr: usize = trap_cx_pa.into();
            trap_cx_addr
        };
        // alloc a pid and a kernel stack in kernel space
        
        let kernel_stack = kstack_alloc();

        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;
        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        #[cfg(target_arch = "loongarch64")]
        let parent_trap_cx = *parent_inner.get_trap_cx();
        // copy fd table
        let mut new_fd_table: Vec<Option<Arc<dyn File + Send + Sync>>> = Vec::new();
        for fd in parent_inner.fd_table.iter() {
            if let Some(file) = fd {
                new_fd_table.push(Some(file.clone()));
            } else {
                new_fd_table.push(None);
            }
        }
        let proc_control_block = Arc::new(ProcessControlBlock {
            pid: pid_handle.clone(),
            inner: MPSafeCell::new(ProcessControlBlockInner {
                pname: parent_inner.pname.clone(),
                base_size: parent_inner.base_size,
                memory_set,
                parent: Some(Arc::downgrade(self)),
                children: Vec::new(),
                heap_bottom: parent_inner.heap_bottom,
                program_brk: parent_inner.program_brk,
                fd_table: new_fd_table,
                cwd: parent_inner.cwd.clone(),
                uid: parent_inner.uid,
                gid: parent_inner.gid,
                euid: parent_inner.euid,
                egid: parent_inner.egid,
                clear_child_tid: parent_inner.clear_child_tid,
                tasks: Vec::new(),
            })
        });

        let new_task = TaskControlBlock {
            process: Arc::downgrade(&proc_control_block),
            tid: tid_handle.clone(),
            kernel_stack: kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                signal_mask: caller_task.inner_exclusive_access().signal_mask,
                handling_sig: caller_task.inner_exclusive_access().handling_sig,
                signal_actions: caller_task.inner_exclusive_access().signal_actions.clone(),
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: caller_task.inner_exclusive_access().signals,
            }),
        };
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
        #[cfg(target_arch = "loongarch64")]
        {
            *trap_cx = parent_trap_cx;
        }
        #[cfg(target_arch = "riscv64")]{
            *trap_cx = *caller_task.inner_exclusive_access().get_trap_cx();
            trap_cx.kernel_sp = kernel_stack_top;
        }
        if let Some(sp) = sp {
            trap_cx.set_sp(sp);
        }
        let new_task_arc = Arc::new(new_task);
        // 将任务加入全局任务池, 暂未实现
        add_task_into_pool(new_task_arc.clone());
        // 把任务加入进程的线程列表
        proc_control_block.inner.exclusive_access().tasks.push(new_task_arc.clone());
        // add child
        parent_inner.children.push(proc_control_block.clone());
        // return
        proc_control_block
        // **** release child PCB
        // ---- release parent PCB
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
        let size: i32 = i32::try_from(addr).unwrap() - self.inner_exclusive_access().program_brk as i32;
        let mut inner = self.inner_exclusive_access();
        let heap_bottom = inner.heap_bottom;
        let _old_break = inner.program_brk;
        let new_brk = addr as isize;
        if new_brk < heap_bottom as isize {
            return Err(-1);
        }
        let result = if size < 0 {
            inner
                .memory_set
                .shrink_to(VirtAddr(heap_bottom), VirtAddr(new_brk as *const () as usize))
        } else {
            inner
                .memory_set
                .append_to(VirtAddr(heap_bottom), VirtAddr(new_brk as *const () as usize))
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
        prot: mmap::MMapProt
    ) -> Result<usize, i32> {
        let mut inner = self.inner_exclusive_access();
        inner.memory_set.mmap(addr, length, prot)
    }
    /// 处理munmap
    pub fn munmap(&self, addr: usize, length: usize) -> Result<(), i32> {
        let mut inner = self.inner_exclusive_access();
        inner.memory_set.munmap(addr, length)
    }
}

pub struct ProcessControlBlockInner {
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
    pub program_brk: usize,// 注意需要在exec中维护，rcore忽略了这点，运行测例时brk失效，已修复
    pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,

    pub cwd: Arc<Dentry>, // 当前工作目录

    pub uid: u32,  // 真实用户 ID
    pub gid: u32,  // 真实组 ID
    pub euid: u32, // 有效用户 ID (Effective)
    pub egid: u32, // 有效组 ID (Effective)
    pub clear_child_tid: usize,// 线程清理指针
    pub tasks: Vec<Arc<TaskControlBlock>>,
}

impl ProcessControlBlockInner {
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    pub fn get_asid(&self) -> usize {
        self.memory_set.asid()
    }
    pub fn alloc_fd(&mut self) -> usize {
        if let Some(fd) = (0..self.fd_table.len()).find(|fd| self.fd_table[*fd].is_none()) {
            fd
        } else {
            self.fd_table.push(None);
            self.fd_table.len() - 1
        }
    }
}

