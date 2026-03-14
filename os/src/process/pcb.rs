//！ TODO：需要仔细核对并修改exec和fork的实现

use super::*;
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, IdHandle, SignalActions, SignalFlags, TaskContext};
use schedule::*;

use crate::{
    arch::trap::{TrapContext, trap_handler, trap_cx_va_by_kernel_stack},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, 
        translated_refmut, MapArea, MapPermission, MapType},
    sync::MPSafeCell,
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use crate::arch::{config::*, trap};

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
    /// 获取进程块的独占访问权限
    pub fn inner_exclusive_access(&self) -> spin::MutexGuard<'_, ProcessControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// 用于创建初始化进程
    /// Create a new process
    /// 为la64修改
    /// At present, it is only used for the creation of initproc
    /// 现在会返回新创建的PCB及其主线程TCB（均为arc）
    pub fn new(elf_data: &[u8]) -> (Arc<Self>, Arc<TaskControlBlock>) {
        println!("[kernel] TaskControlBlock::new: start creating a new process");
        let (mut memory_set, user_sp, entry_point, _phdr, _phnum, _phent)
            = MemorySet::from_elf(elf_data);
        debug!(
            "TaskControlBlock::new: entry_point={:#x}, user_sp={:#x}",
            entry_point, user_sp
        );
        
        // alloc a pid and a kernel stack in kernel space
        // 注意：push_on_top已经被修改，请及时改回！！！！！！！！！！！！！！！！！！！！！！！！！！！！！！
        let pid_handle = Arc::new(pid_alloc());
        let tid_handle = Arc::new(tid_alloc());
        let kernel_stack = kstack_alloc();

        let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(&kernel_stack).into();
        info!("TaskControlBlock::new: calculated trap_cx_va = {:#x}", trap_cx_va.0);
        memory_set.push(
            MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                MapType::Framed, MapPermission::R | MapPermission::W),
            None,
            trap_cx_va.0,
        );

        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_ppn = memory_set
                .translate(trap_cx_va.into())
                .unwrap()
                .ppn();
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            trap_cx_pa.into()
        };
        info!("TaskControlBlock::new: translated trap_cx_addr = {:#x}", trap_cx_addr);
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
                signals: SignalFlags::empty(),
                signal_actions: SignalActions::default(),
                exit_code: 0,
                uid: 0,
                gid: 0,
                euid: 0,
                egid: 0,
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
                signal_mask: SignalFlags::empty(),
                handling_sig: -1,
                killed: false,
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
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        debug!("TaskControlBlock::new: finished creating a new process");
        
        proc_control_block.inner.exclusive_access().tasks.push(task_control_block.clone());

        // 返回PCB和主线程
        (proc_control_block, task_control_block)
    }

        

    /// Load a new elf to replace the original application address space and 
    /// 待修改
    pub fn exec(self: &Arc<ProcessControlBlock>, caller_task: Arc<TaskControlBlock>, elf_data: &[u8], args: Vec<String>) {
        // 生成新地址空间
        let (mut memory_set, mut user_sp, entry_point, phdr_addr, phnum, phent) = MemorySet::from_elf(elf_data);
        debug!(
            "[kernel] task::exec: entry_point={:#x}, user_sp={:#x}",
            entry_point, user_sp
        );
        let memory_top = user_sp;
        // 压入具体的字符串内容（高地址）
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
        // 对齐栈指针
        user_sp -= user_sp % core::mem::size_of::<usize>();
        // 构造 AUX Vector
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
        // 压入 envp 数组：目前只压入一个 NULL (0)
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = 0;
        // 压入 argv 数组：先压入一个 NULL (0) 作为结尾
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = 0;
        // 逆序压入 argv 的指针
        for arg_ptr in argv_ptrs.iter().rev() {
            user_sp -= core::mem::size_of::<usize>();
            *translated_refmut(memory_set.token(), user_sp as *mut usize) = *arg_ptr;
        }
        // 此时 user_sp 即为 argv[0] 的地址
        let argv_base = user_sp;
        // 压入 argc
        user_sp -= core::mem::size_of::<usize>();
        *translated_refmut(memory_set.token(), user_sp as *mut usize) = args.len();
        // 更新 PCB 内部信息
        let mut proc_inner = self.inner_exclusive_access();
        proc_inner.heap_bottom = memory_top;
        proc_inner.program_brk = memory_top;
        // 内核栈无须改变（fork时已经分配了新的）但需要重新映射
        let kernel_stack = &caller_task.kernel_stack;
        let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(kernel_stack).into();
        memory_set.push(
            MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                MapType::Framed, MapPermission::R | MapPermission::W),
            None,
            trap_cx_va.0,
        );
        // 重新获取一次trap_cx_addr，因为原内存空间将被销毁
        let trap_cx_addr: usize = {
            #[cfg(target_arch = "riscv64")]
            {
                let trap_cx_ppn = memory_set
                    .translate(trap_cx_va.into())
                    .unwrap()
                    .ppn();
                let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
                trap_cx_pa.into()
            }
        };
        proc_inner.memory_set = memory_set;

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = caller_task.kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = task_inner.trap_cx_addr;

        // 修改trap上下文
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

        // 更新tcb信息
        let mut task_inner = caller_task.inner_exclusive_access();
        task_inner.trap_cx_addr = trap_cx_addr;
        *task_inner.get_trap_cx() = trap_cx;

        // 删除其他线程（如果有）
        proc_inner.tasks.retain(|t| Arc::ptr_eq(t, &caller_task));
        proc_inner.alive_task_count = 1;

        
    }

    /// Fork from parent to child
    /// 已编辑，添加了stack参数 
    /// 现在会返回新创建的PCB及其主线程TCB（均为arc）
    pub fn fork(self: &Arc<ProcessControlBlock>, sp: Option<usize>, caller_task: Arc<TaskControlBlock>)-> (Arc<Self>, Arc<TaskControlBlock>) {
        // ---- hold parent PCB lock
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let mut memory_set = MemorySet::from_existed_user(&parent_inner.memory_set);
    
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = Arc::new(pid_alloc());
        let tid_handle = Arc::new(tid_alloc());
        let kernel_stack = kstack_alloc();

        let trap_cx_va: VirtAddr = (kernel_stack.get_top() - KERNEL_STACK_SIZE).into();
        memory_set.push(
            MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                MapType::Framed, MapPermission::R | MapPermission::W),
            None,
            trap_cx_va.0,
        );
        #[cfg(target_arch = "riscv64")]
        let trap_cx_ppn = memory_set
            .translate(trap_cx_va.into())
            .unwrap()
            .ppn();
        info!("fork: translated trap_cx_ppn = {:#x}", trap_cx_ppn.0);
        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            let trap_cx_addr: usize = trap_cx_pa.into();
            trap_cx_addr
        };
        

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
                signals: parent_inner.signals,
                signal_actions: parent_inner.signal_actions.clone(),
                exit_code: 0,
                uid: parent_inner.uid,
                gid: parent_inner.gid,
                euid: parent_inner.euid,
                egid: parent_inner.egid,
                tasks: Vec::new(),
                alive_task_count: parent_inner.alive_task_count,
            })
        });
        let caller_inner = caller_task.inner_exclusive_access();
        let new_task = Arc::new(TaskControlBlock {
            process: Arc::downgrade(&proc_control_block),
            tid: tid_handle.clone(),
            kernel_stack: kernel_stack,
            inner: MPSafeCell::new(TaskControlBlockInner {
                trap_cx_addr,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                signal_mask: caller_inner.signal_mask,
                handling_sig: caller_inner.handling_sig,
                killed: false,
                frozen: false,
                trap_ctx_backup: None,
                exit_code: 0,
                signals: caller_inner.signals,
                clear_child_tid: caller_inner.clear_child_tid,
            }),
        });
        info!("fork: created new task with tid {}", new_task.gettid());
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
        #[cfg(target_arch = "loongarch64")]
        {
            *trap_cx = parent_trap_cx;
        }
        #[cfg(target_arch = "riscv64")]{
            *trap_cx = *caller_inner.get_trap_cx();
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

    // 进程收到的信号
    pub signals: SignalFlags,

    // Signal actions
    pub signal_actions: SignalActions,

    pub exit_code: i32, // 进程退出码，默认为0，只有当进程状态为Zombie时才有意义

    pub uid: u32,  // 真实用户 ID
    pub gid: u32,  // 真实组 ID
    pub euid: u32, // 有效用户 ID (Effective)
    pub egid: u32, // 有效组 ID (Effective)
    
    pub tasks: Vec<Arc<TaskControlBlock>>, 
    // 存活进程数，等于0相当于僵尸进程
    pub alive_task_count: usize,
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
    pub fn is_zombie(&self) -> bool {
        self.alive_task_count == 0
    }
}

