//! Types related to task management & Functions for completely changing TCB

use super::{kstack_alloc, pid_alloc, KernelStack, PidHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    arch::trap::{TrapContext, trap_handler},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, translated_refmut},
    sync::UPSafeCell,
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
#[allow(unused)]
use crate::arch::config::*;
use core::cell::RefMut;


const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;

/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// Process identifier
    pub pid: PidHandle,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    inner: UPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> RefMut<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// Get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        let inner = self.inner_exclusive_access();
        inner.memory_set.token()
    }
}

pub struct TaskControlBlockInner {
    /// 此处改为直接保存地址
    pub trap_cx_addr: usize,

    /// Application data can only appear in areas
    /// where the application address space is lower than base_size
    pub base_size: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// Application address space
    pub memory_set: MemorySet,

    /// Parent process of the current process.
    /// Weak will not affect the reference count of the parent
    pub parent: Option<Weak<TaskControlBlock>>,

    /// A vector containing TCBs of all child processes of the current process
    pub children: Vec<Arc<TaskControlBlock>>,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,
    pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,
    pub signals: SignalFlags,
    pub signal_mask: SignalFlags,
    // the signal which is being handling
    pub handling_sig: isize,
    // Signal actions
    pub signal_actions: SignalActions,
    // if the task is killed
    pub killed: bool,
    // if the task is frozen by a signal
    pub frozen: bool,
    pub trap_ctx_backup: Option<TrapContext>,

    /// Heap bottom
    pub heap_bottom: usize,

    /// Program break
    pub program_brk: usize,// 注意需要在exec中维护，rcore忽略了这点，运行测例时brk失效，已修复

    pub cwd: Arc<Dentry>, // 当前工作目录
}

impl TaskControlBlockInner {
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        PhysAddr(self.trap_cx_addr).get_mut()
    }
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
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

impl TaskControlBlock {
    /// Create a new process
    /// 为la64修改
    /// At present, it is only used for the creation of initproc
    pub fn new(elf_data: &[u8]) -> Self {
        println!("[kernel] TaskControlBlock::new: start creating a new process");
        let (memory_set, user_sp, entry_point, _phdr, _phnum, _phent)
            = MemorySet::from_elf(elf_data);
        println!(
            "[kernel] TaskControlBlock::new: entry_point={:#x}, user_sp={:#x}",
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
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();

        println!("[kernel] TaskControlBlock::new");

        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;

        println!("[kernel] TaskControlBlock::new: kernel_stack_top={:#x}", kernel_stack.get_top());

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        // push a task context which goes to trap_return to the top of kernel stack
        let task_control_block = Self {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_addr,
                    base_size: user_sp,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: None,
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: vec![
                        // 0 -> stdin
                        Some(Arc::new(Stdin)),
                        // 1 -> stdout
                        Some(Arc::new(Stdout)),
                        // 2 -> stderr
                        Some(Arc::new(Stdout)),
                    ],
                    signals: SignalFlags::empty(),
                    signal_mask: SignalFlags::empty(),
                    handling_sig: -1,
                    signal_actions: SignalActions::default(),
                    killed: false,
                    frozen: false,
                    trap_ctx_backup: None,
                    heap_bottom: user_sp,
                    program_brk: user_sp,
                    cwd: ROOT_DENTRY.clone(),
                })
            },
        };
        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        // 发现问题：这样解引用写入会炸
        // 已解决：只分配了128MB内存，之前的实现写到了有效区之外
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as *const () as usize,
        );
        println!("[kernel] TaskControlBlock::new: finished creating a new process");
        task_control_block
    }

    /// Load a new elf to replace the original application address space and start execution
    pub fn exec(&self, elf_data: &[u8], args: Vec<String>) {
        // 1. 加载 ELF 文件生成新的地址空间
        let (memory_set, mut user_sp, entry_point, phdr_addr, phnum, phent) = MemorySet::from_elf(elf_data);
        info!(
            "[kernel] task::exec: entry_point={:#x}, user_sp={:#x}",
            entry_point, user_sp
        );
        #[cfg(target_arch = "riscv64")]
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
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

        // 7. 更新 TCB 内部信息
        let mut inner = self.inner_exclusive_access();
        inner.memory_set = memory_set;
        #[cfg(target_arch = "riscv64")]
        {
            inner.trap_cx_addr = trap_cx_addr;
        }
        inner.heap_bottom = memory_top;
        inner.program_brk = memory_top;

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = self.kernel_stack.get_top();
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
        
        *inner.get_trap_cx() = trap_cx;
    }

    /// Fork from parent to child
    /// 已编辑，添加了stack参数
    pub fn fork(self: &Arc<TaskControlBlock>, sp: Option<usize>) -> Arc<TaskControlBlock> {
        // ---- hold parent PCB lock
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let memory_set = MemorySet::from_existed_user(&parent_inner.memory_set);
        #[cfg(target_arch = "riscv64")]
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        #[cfg(target_arch = "riscv64")]
        let trap_cx_addr = {
            let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
            let trap_cx_addr: usize = trap_cx_pa.into();
            trap_cx_addr
        };
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
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
        let task_control_block = Arc::new(TaskControlBlock {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_addr,
                    base_size: parent_inner.base_size,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: new_fd_table,
                    signals: SignalFlags::empty(),
                    // inherit the signal_mask and signal_action
                    signal_mask: parent_inner.signal_mask,
                    handling_sig: -1,
                    signal_actions: parent_inner.signal_actions.clone(),
                    killed: false,
                    frozen: false,
                    trap_ctx_backup: None,
                    heap_bottom: sp.unwrap_or(parent_inner.heap_bottom),
                    program_brk: parent_inner.program_brk,
                    cwd: parent_inner.cwd.clone(),
                })
            },
        });
        // add child
        parent_inner.children.push(task_control_block.clone());
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        #[cfg(target_arch = "loongarch64")]
        {
            *trap_cx = parent_trap_cx;
        }
        
        trap_cx.kernel_sp = kernel_stack_top;
        if let Some(sp) = sp {
            trap_cx.set_sp(sp);
        }
        // return
        task_control_block
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
        println!("brk: change from {:#x} to {:#x}", _old_break, new_brk);
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

#[derive(Copy, Clone, PartialEq)]
/// task status: UnInit, Ready, Running, Exited
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Zombie,
}
