//！ TODO：需要仔细核对并修改exec和fork的实现

use super::*;
use super::{kstack_alloc, pid_alloc, tid_alloc, KernelStack, PidHandle, SignalActions, SignalFlags, TaskContext};
use schedule::*;

use crate::{
    arch::trap::{TrapContext, trap_handler, trap_cx_va_by_kernel_stack},
    fs::{Dentry, File, ROOT_DENTRY,Stdin, Stdout, Stderr},
    mm::{KERNEL_SPACE, MemorySet, PhysAddr, VirtAddr, mmap, 
        translated_refmut, MapArea, MapPermission, MapType},
    sync::{MPSafeCell, WaitQueue},
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use crate::arch::{config::*, trap};
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
        //info!("TaskControlBlock::new: translated trap_cx_addr = {:#x}", trap_cx_addr);
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
                on_main_hart: true, // initproc和shell默认在主核运行
                pname: String::from("initproc"),
                base_size: user_sp,
                memory_set,
                parent: None,
                children: Vec::new(),
                heap_bottom: user_sp,
                program_brk: user_sp,
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
                is_zombie: false,
                egid: 0,
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
    pub fn exec(self: &Arc<ProcessControlBlock>, caller_task: Arc<TaskControlBlock>, elf_data: &[u8],interp_data: Option<&[u8]>, args: Vec<String>, on_main_hart: bool) {
        // 生成新地址空间
        let (mut memory_set, mut user_sp,  entry_point, phdr_addr, phnum, phent) = MemorySet::from_elf(elf_data);
        let mut final_entry_point = entry_point; // 默认入口为主程序入口
        const INTERP_BASE: usize = 0x40000000;   // 给解释器找一个宽敞的基地址（避开主程序）
        const AT_BASE: usize = 7;                // 辅助向量里代表解释器基址的 ID
        
        
        if let Some(interp) = interp_data {
            // 解析解释器的 ELF
            let elf = xmas_elf::ElfFile::new(interp).unwrap();
            // 新的入口点 = 解释器的基地址 + 解释器 ELF 里的偏移
            final_entry_point = INTERP_BASE + elf.header.pt2.entry_point() as usize;

            // 遍历解释器的所有段，把 LOAD 段映射进当前进程的页表
            for ph in elf.program_iter() {
                if ph.get_type() == Ok(xmas_elf::program::Type::Load) {
                    let start_va = INTERP_BASE + ph.virtual_addr() as usize;
                    let end_va = start_va + ph.mem_size() as usize;
                    
                    let mut map_perm = crate::mm::MapPermission::U; // 记得确认你的 MapPermission 路径
                    let ph_flags = ph.flags();
                    if ph_flags.is_read() { map_perm |= crate::mm::MapPermission::R; }
                    if ph_flags.is_write() { map_perm |= crate::mm::MapPermission::W; }
                    if ph_flags.is_execute() { map_perm |= crate::mm::MapPermission::X; }
                    
                    let map_area = crate::mm::MapArea::new(
                        start_va.into(),
                        end_va.into(),
                        crate::mm::MapType::Framed,
                        map_perm,
                    );
                    
                    let offset = ph.offset() as usize;
                    let file_size = ph.file_size() as usize;
                    let data = &interp[offset..offset + file_size];
                    debug!("[kernel] task::exec: mapping interp segment: [{:#x}, {:#x}), offset={:#x}, file_size={:#x}", start_va, end_va, offset, file_size);
                    memory_set.push(map_area, Some(data),start_va); 
                }
            }
        }
        
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
         // AT_NULL
        // 压入 AUXV
        if interp_data.is_some() {
            auxv.push((AT_BASE, INTERP_BASE));
        }
        auxv.push((0, 0));
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
        let kernel_stack = &caller_task.kernel_stack;
        let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(kernel_stack).into();
        memory_set.push(
            MapArea::new(trap_cx_va, VirtAddr::from(trap_cx_va.0 + KERNEL_STACK_SIZE),
                MapType::Framed, MapPermission::R | MapPermission::W),
            None,
            trap_cx_va.0,
        );
        #[cfg(target_arch = "riscv64")]
        // 重新获取一次trap_cx_addr，因为原内存空间将被销毁
        let trap_cx_addr: usize = {
            {
                let trap_cx_ppn = memory_set
                    .translate(trap_cx_va.into())
                    .unwrap()
                    .ppn();
                let trap_cx_pa: PhysAddr = trap_cx_ppn.into();
                trap_cx_pa.into()
            }
        };
        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr: usize = caller_task.inner_exclusive_access().trap_cx_addr;
        proc_inner.memory_set = memory_set;
        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = caller_task.kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        // 修改trap上下文
        let mut trap_cx = TrapContext::app_init_context(
            final_entry_point,
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
        proc_inner.tasks.retain(|t: &Arc<TaskControlBlock>| Arc::ptr_eq(t, &caller_task));
        proc_inner.alive_task_count = 1;
        for i in proc_inner.memory_set.areas().iter() {
            debug!("exec: map_area: [{:#x}, {:#x})", i.get_vpn_range().get_start().0, i.get_vpn_range().get_end().0);
        }
        
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

        let trap_cx_va: VirtAddr = trap_cx_va_by_kernel_stack(&kernel_stack).into();
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
            let trap_cx_addr: usize = trap_cx_pa.into();
            trap_cx_addr
        };
        #[cfg(target_arch = "loongarch64")]
        let trap_cx_addr = kernel_stack.push_on_top(TrapContext::new_bare()) as usize;

        #[cfg(target_arch = "riscv64")]
        let kernel_stack_top = kernel_stack.get_top();
        #[cfg(target_arch = "loongarch64")]
        let kernel_stack_top = trap_cx_addr;

        // copy fd table
        let new_fd_table = parent_inner.fd_table.clone();
        // println!("[kernel] ProcessControlBlock::fork: copied fd_table with {} entries", new_fd_table.len());
        let proc_control_block = Arc::new(ProcessControlBlock {
            pid: pid_handle.clone(),
            inner: MPSafeCell::new(ProcessControlBlockInner {
                on_main_hart: false, // LA 侧先固定到主核，避免未收敛的跨核执行路径
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
                sid:parent_inner.sid,
                egid: parent_inner.egid,
                pgid: parent_inner.pgid,
                fd_rlmt: parent_inner.fd_rlmt.clone(),
                tasks: Vec::new(),
                is_zombie: false,
                alive_task_count: 1,
            })
        });
        let caller_inner = caller_task.inner_exclusive_access();
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
        info!("fork: created new task with tid {}", new_task.gettid());
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
        flags: mmap::MMapFlags
    ) -> Result<usize, i32> {
        let mut inner = self.inner_exclusive_access();
        inner.memory_set.mmap(addr, length, prot, flags)
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
    pub program_brk: usize,// 注意需要在exec中维护，rcore忽略了这点，运行测例时brk失效，已修复

    pub fd_rlmt: Rlimit64, // cur_lmt, max_lmt

    pub fd_table: Vec<FileDescriptor>,
    
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
    pub sid: usize,
    // 新增：进程组 ID
    pub pgid: usize,
    pub is_zombie: bool,
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
        self.alive_task_count <= 0
    }
}

