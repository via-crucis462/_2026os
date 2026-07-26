// 正在为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html
use crate::process::scheduler::processor::current_user_asid;
mod context;

use crate::{KERNEL_STACK_SIZE, PAGE_SIZE, get_hart_id};
use crate::mm::{PageTable, VirtAddr};
use crate::syscall::syscall;
use crate::arch::timer::get_time_ms;
use crate::arch::mm::flush_tlb_for_asid;
use crate::task::{
    KernelStack, SignalFlags,
    current_add_signal, current_task, current_tid, current_trap_cx,
    current_user_token, exit_current_and_run_next,
    suspend_current_and_run_next, handle_signals
};
use crate::net::net_poll;
use alloc::sync::Arc;
use core::arch::{asm, global_asm};
global_asm!(include_str!("trap.S"));

extern "C" {
    fn __alltraps();
}

#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
/// 在我们现在的实现中，io设备不需要使用这个函数，先不删除
/// 改为调试信息打印
pub fn trap_from_kernel() -> ! {
    let estat: usize;
    let era: usize;
    let badv: usize;
    let badi: usize;
    unsafe {
        asm!("csrrd {estat}, 0x5", estat = out(reg) estat);
        asm!("csrrd {era}, 0x6", era = out(reg) era);
        asm!("csrrd {badv}, 0x7", badv = out(reg) badv);
        asm!("csrrd {badi}, 0x8", badi = out(reg) badi);
    }
    let hart_id = get_hart_id();
    let (ecode_name, ecode, esubcode, timer_pending) = decode_estat(estat);
    error!(
        "[kernel][panic] trap_from_kernel: hart={}, ESTAT={:#x}, ECODE={}({:#x}), ESUBCODE={:#x}, timer_pending={}, ERA={:#x}, BADV={:#x}, BADI={:#x}",
        hart_id,
        estat,
        ecode_name,
        ecode,
        esubcode,
        timer_pending,
        era,
        badv,
        badi
    );
    if let Some(task) = current_task() {
        let mm = task.inner_exclusive_access().mm.as_ref().cloned();
        let Some(mm) = mm else {
            error!("[kernel][panic] current task has no user mm");
            loop {}
        };
        let memory_set = mm.exclusive_access();
        let heap_bottom = memory_set.areas()[memory_set.brk_index()]
            .get_vpn_range()
            .get_start()
            .0
            * PAGE_SIZE;
        error!(
            "[kernel][panic] current task snapshot: pid={}, tid={}, heap_bottom={:#x}, program_brk={:#x}",
            task.getpid(),
            task.gettid(),
            heap_bottom,
            memory_set.current_brk(),
        );
        memory_set.debug_dump_areas(Some(badv), Some(era));
    } else {
        error!("[kernel][panic] no current task on this hart");
    }
    debug!(
        "a trap from kernel! hart={}, estat={:#x}, ecode={}({:#x}), esubcode={:#x}, timer_pending={}, era={:#x}, badv={:#x}, badi={:#x}",
        hart_id,
        estat,
        ecode_name,
        ecode,
        esubcode,
        timer_pending,
        era,
        badv,
        badi
    );
    loop{}
}

/// Initialize trap handling
pub fn init() {
    unsafe{
        let mut crmd: usize;
        asm!("csrrd {}, 0x0", out(reg) crmd);
        crmd &= !(1 << 2); // 先关闭中断
        asm!("csrwr {}, 0x0", in(reg) crmd);
    }
    set_kernel_trap_entry();
}

// 从内核trap时的入口
fn set_kernel_trap_entry() {
    let target = trap_from_kernel as *const () as usize;
    let trap_handler: usize = target;
    unsafe {
        asm!(
            "csrwr {trap_handler},0xc",
            trap_handler = inout(reg) trap_handler => _,
        );
    }
    //debug!("[kernel] set_kernel_trap_entry: trap_handler address = {:#x}", trap_handler);
}

/// 插入__all_trap的地址
/// 当发生trap时，硬件会切换权限级，这时窗口映射生效
/// 直接访问0x9开始物理地址即可

/*fn set_user_trap_entry() {
    let target = __alltraps as *const () as usize;
    let mut trap: usize = target;
    unsafe {
        asm!(
            "csrwr {trap},0xc",
            trap = inout(reg) trap,
        );
    }
}*/

/// enable timer interrupt in supervisor mode
/// 可能有问题，后续修复
pub fn enable_timer_interrupt() {
    crate::arch::timer::init_board_freq();
    unsafe {
        asm!("csrwr {}, 0x44", in(reg) 1);// 清除定时器中断
        let mut ecfg: usize;
        asm!("csrrd {}, 0x4", out(reg) ecfg);
        asm!("csrwr {}, 0x4", in(reg) ecfg | (1 << 11)); // 使能定时器中断
        // 与riscv侧思路一致：这里只打开中断源，不在内核态全局开中断位。
        // 内核态保持IE关闭，可避免空闲内核代码被时钟中断打入trap_from_kernel。
    }
}


#[derive(Debug)]
enum Cause {
    Syscall,
    TimeInterrupt,
    Other,
}

const BRK_PROCESS_NAME: &str = "brk";
const BRK_PRINTF_START: usize = 0x13e8;
const BRK_PRINTF_END: usize = 0x16bc;
const SYS_WRITE: usize = 64;
const SYS_BRK: usize = 214;

fn decode_estat(estat: usize) -> (&'static str, usize, usize, bool) {
    let ecode = (estat >> 16) & 0x3f;
    let esubcode = (estat >> 22) & 0x1ff;
    let timer_pending = ((estat >> 11) & 1) != 0;

    let name = match ecode {
        0 => "INT",
        1 => "PIL",
        2 => "PIS",
        3 => "PIF",
        4 => "PME",
        5 => "PNR",
        6 => "PNX",
        7 => "PPI",
        8 => match esubcode {
            0 => "ADEF",
            1 => "ADEM",
            _ => "ADE",
        },
        9 => "ALE",
        10 => "BCE",
        11 => "SYS",
        12 => "BRK",
        13 => "INE",
        14 => "IPE",
        15 => "FPD",
        16 => "SXD",
        17 => "ASXD",
        18 => match esubcode {
            0 => "FPE",
            1 => "VFPE",
            _ => "FPE?",
        },
        19 => match esubcode {
            0 => "WPEF",
            1 => "WPEM",
            _ => "WATCH",
        },
        20 => "BTDIS",
        21 => "BTE",
        22 => "GSPR",
        23 => "HVC",
        24 => match esubcode {
            0 => "GCSC",
            1 => "GCHC",
            _ => "GCM",
        },
        25 => "SE",
        _ => "UNKNOWN",
    };

    (name, ecode, esubcode, timer_pending)
}

fn is_brk_process() -> bool {
    current_task()
        .map(|task| {
            let comm = task.inner_exclusive_access().comm;
            let len = comm.iter().position(|byte| *byte == 0).unwrap_or(comm.len());
            core::str::from_utf8(&comm[..len])
                .map(|name| name == BRK_PROCESS_NAME || name.ends_with("/brk"))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn debug_dump_user_stack_window(tag: &str, token: usize, sp: usize, words: usize) {
    let page_table = PageTable::from_token(token);
    error!(
        "[kernel] brk_stack {}: sp={:#x}, dumping {} words",
        tag,
        sp,
        words
    );
    for index in 0..words {
        let va = sp + index * core::mem::size_of::<usize>();
        match page_table.translate_va(VirtAddr::from(va)) {
            Some(pa) => {
                let value = *pa.get_ref::<usize>();
                error!(
                    "[kernel] brk_stack {}: [{:#x}] = {:#x}",
                    tag,
                    va,
                    value
                );
            }
            None => {
                error!(
                    "[kernel] brk_stack {}: [{:#x}] = <unmapped>",
                    tag,
                    va
                );
            }
        }
    }
}

fn debug_dump_brk_snapshot(tag: &str, cx: &TrapContext, token: usize) {
    error!(
        "[kernel] brk_trace {}: era={:#x}, user_sp={:#x}, a0={:#x}, a1={:#x}, a2={:#x}, a3={:#x}, a4={:#x}, a5={:#x}, a6={:#x}, a7={:#x}",
        tag,
        cx.get_rt(),
        cx.r[3],
        cx.r[4],
        cx.r[5],
        cx.r[6],
        cx.r[7],
        cx.r[8],
        cx.r[9],
        cx.r[10],
        cx.r[11],
    );
    debug_dump_user_stack_window(tag, token, cx.r[3], 24);
}

/// trap handler
/// 从riscv的trap handler改写
/// 参考https://www.loongson.cn/uploads/images/2023041918133323805.%E9%BE%99%E8%8A%AF%E6%9E%B6%E6%9E%84%E5%8F%82%E8%80%83%E6%89%8B%E5%86%8C%E5%8D%B7%E4%B8%80_r1p03.pdf
/// 的111页和97页
#[no_mangle]
pub fn trap_handler() -> ! {
    // 设置内核态异常入口，防止嵌套中断时重入 __alltraps 破坏上下文
    set_kernel_trap_entry();
    //println!("[kernel] called trap_handler");
    let estat :usize;
    let era :usize;
    let badv :usize;
    let badi :usize;


    unsafe {
        asm!("csrrd {}, 0x5", out(reg) estat);
        asm!("csrrd {}, 0x6", out(reg) era);
        asm!("csrrd {}, 0x7", out(reg) badv);
        asm!("csrrd {}, 0x8", out(reg) badi);
    };
    // 具体需要查表，位于手册111页表格
    //11_0000_0000_0000_0000=>页表
    //3_0000_0000_0000_0000=>取指操作页无效例外
    
    let cause = if ((estat >> 11) & 1)  != 0 {
        Cause::TimeInterrupt
    } else if ((estat >> 16) & 0x3fff) == 0xb {
        Cause::Syscall
    } else {
        Cause::Other
    };
    let (ecode_name, ecode, esubcode, _) = decode_estat(estat);
        // 目前实现还不完善
    match cause {
        Cause::Syscall => {
            let mut cx = current_trap_cx();
            let syscall_id = cx.r[11];
            //println!("[kernel] trap_handler: syscall_id={}, pid={}, tid={}, hart_id={}, era={:#x} , ra={:#x}, sp={:#x}", syscall_id, current_task().unwrap().getpid(), current_tid(), get_hart_id(), cx.get_rt(), cx.r[1], cx.r[2]);
            let should_trace = is_brk_process() && matches!(syscall_id, SYS_WRITE | SYS_BRK);
            if should_trace {
                 //debug_dump_brk_snapshot("before_syscall", cx, current_user_token());
            }
            cx.set_rt(cx.get_rt() + 4);
            // get system call return value
            let result = syscall(
                syscall_id,
                [cx.r[4], cx.r[5], cx.r[6], cx.r[7], cx.r[8], cx.r[9]]
            );
            current_task().unwrap().inner_exclusive_access().errno = if result < 0 {
                (-result) as i32
            } else {
                0
            };
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();
            cx.r[4] = result as usize;
            if should_trace {
                //debug_dump_brk_snapshot("after_syscall", cx, current_user_token());
            }
        }
        Cause::TimeInterrupt => {
            unsafe {
                asm!("csrwr {}, 0x44", in(reg) 1);// 清除定时器中断
            }
            let current_ms = get_time_ms();
            let expired_pids = crate::timer::TIMER_MANAGER.lock().tick(current_ms);
            for pid in expired_pids {
                if let Some(process) = crate::task::get_process(pid) {
                    let tasks = crate::process::registry::TID2TCB
                        .exclusive_access()
                        .values()
                        .filter(|task| task.gettgid() == process.gettgid())
                        .cloned()
                        .collect::<alloc::vec::Vec<_>>();
                    for task in tasks {
                        let mut task_inner = task.inner_exclusive_access();
                        task_inner.pending.insert(crate::task::SignalFlags::SIGALRM);
                        if task_inner.state == crate::task::TaskStatus::Blocked {
                            task_inner.signal_interrupted = true;
                            task_inner.state = crate::task::TaskStatus::Ready;
                            drop(task_inner);
                            crate::task::add_task(task);
                        }
                    }
                }
            }
            net_poll();
            crate::mm::mmap::tick_sync();
            suspend_current_and_run_next();
        }
        _ => {
            if let Some(task) = current_task() {
                let mm = task.inner_exclusive_access().mm.as_ref().cloned();
                let Some(mm) = mm else {
                    error!("[kernel] user fault without mm: pid={}, tid={}", task.getpid(), task.gettid());
                    exit_current_and_run_next(-11);
                    panic!("unreachable: exited task without mm");
                };
                let mut memory_set = mm.exclusive_access();
                let sp = current_trap_cx().r[3];
                let vpn = VirtAddr::from(badv).std_floor();
                if ecode == 4 {
                    if let Some(pte) = memory_set.translate(vpn) {
                        if pte.is_valid() && pte.writable() && memory_set.set_pte_dirty(vpn) {
                            drop(memory_set);
                            drop(task);
                            trap_return();
                        }
                    }
                }
                if memory_set.handle_cow_fault(badv) {
                    drop(memory_set);
                    drop(task);
                    trap_return();
                }
                if memory_set.handle_page_fault(badv, sp) {
                    drop(memory_set);
                    drop(task);
                    trap_return();
                }
                match memory_set.translate(vpn) {
                    Some(pte) => {
                        trace!(
                            "[kernel] user_fault_pte: current hart id={}, estat={:#x}, ecode={}({:#x}), esubcode={:#x}, era={:#x}, badv={:#x}, badi={:#x}, ra={:#x}, sp={:#x}, vpn={:#x}, pte_bits={:#x}, valid={}, r={}, w={}, x={}",
                            get_hart_id(),
                            estat,
                            ecode_name,
                            ecode,
                            esubcode,
                            era,
                            badv,
                            badi,
                            current_trap_cx().r[1],
                            current_trap_cx().r[3],
                            vpn.0,
                            pte.bits,
                            pte.is_valid(),
                            pte.readable(),
                            pte.writable(),
                            pte.executable(),
                        );
                    }
                    None => {
                        trace!(
                            "[kernel] user_fault_pte: current hart id={}, estat={:#x}, ecode={}({:#x}), esubcode={:#x}, era={:#x}, badv={:#x}, badi={:#x}, ra={:#x}, sp={:#x}, vpn={:#x}, pte=<none>",
                            get_hart_id(),
                            estat,
                            ecode_name,
                            ecode,
                            esubcode,
                            era,
                            badv,
                            badi,
                            current_trap_cx().r[1],
                            current_trap_cx().r[3],
                            vpn.0,
                        );
                    }
                }
                        // BRK(ecode=0xc): 验证 ERA 处物理页内容
                    /*if ecode == 0xc {
                        let era_vpn = VirtAddr::from(era).floor();
                        let era_offset = era & 0xFFF;
                        // 读取硬件CSR中实际的PGDL值
                        let hw_pgdl: usize;
                        let hw_asid: usize;
                        unsafe {
                            asm!("csrrd {}, 0x19", out(reg) hw_pgdl);
                            asm!("csrrd {}, 0x18", out(reg) hw_asid);
                        }
                        error!(
                            "[BRK诊断] hw_pgdl={:#x}, hw_asid={:#x}, 软件pgdl={:#x}",
                            hw_pgdl, hw_asid, inner.get_user_token()
                        );
                        match inner.memory_set.translate(era_vpn) {
                            Some(era_pte) => {
                                let era_ppn = era_pte.ppn();
                                let page_bytes = era_ppn.get_bytes_array();
                                let w = u32::from_le_bytes([
                                    page_bytes[era_offset],
                                    page_bytes[era_offset+1],
                                    page_bytes[era_offset+2],
                                    page_bytes[era_offset+3],
                                ]);
                                // 也通过硬件PGDL手动遍历页表
                                use crate::mm::PageTable;
                                let hw_pt = PageTable::from_token(hw_pgdl);
                                let hw_pte_result = hw_pt.find_pte(era_vpn);
                                let (hw_ppn_val, hw_pte_bits) = match hw_pte_result {
                                    Some(hw_pte) => (hw_pte.ppn().0, hw_pte.bits),
                                    None => (0xdead, 0x0),
                                };
                                error!(
                                    "[BRK诊断] era={:#x} vpn={:#x} 软件ppn={:#x} pte={:#x} 物理指令={:#010x} badi={:#010x}",
                                    era, era_vpn.0, era_ppn.0, era_pte.bits, w, badi
                                );
                                error!(
                                    "[BRK诊断] 硬件页表查找: hw_ppn={:#x} hw_pte={:#x}",
                                    hw_ppn_val, hw_pte_bits
                                );
                            }
                            None => {
                                error!("[BRK诊断] era={:#x}, era_vpn={:#x}, 软件页表无PTE!", era, era_vpn.0);
                            }
                        }
                    }*/
            }
            /*if is_brk_process() && (BRK_PRINTF_START..BRK_PRINTF_END).contains(&era) {
                let cx = current_trap_cx();
                debug_dump_brk_snapshot("fault_window", cx, current_user_token());
                debug_dump_user_stack_window(
                 "fault_window_varargs",
                    current_user_token(),
                cx.r[3] + 120,
                8,
                );
            }*/
            if let Some(task) = current_task() {
                let mm = task.inner_exclusive_access().mm.as_ref().cloned();
                let Some(mm) = mm else {
                    trace!("[kernel] trap_handler: pid={}, tid={}, no user mm", task.getpid(), task.gettid());
                    exit_current_and_run_next(-11);
                    panic!("unreachable: exited task without mm");
                };
                let memory_set = mm.exclusive_access();
                let heap_bottom = memory_set.areas()[memory_set.brk_index()]
                    .get_vpn_range()
                    .get_start()
                    .0
                    * PAGE_SIZE;
                trace!(
                    "[kernel] trap_handler: pid={}, tid={}, heap_bottom={:#x}, program_brk={:#x}",
                    task.getpid(),
                    task.gettid(),
                    heap_bottom,
                    memory_set.current_brk(),
                );
                // inner.memory_set.debug_dump_areas(Some(badv), Some(era));
            } else {
                trace!("[kernel] trap_handler: no current task");
            }
            current_add_signal(SignalFlags::SIGSEGV);
        }
    }
    /*println!(
        "[trap_handler] before handle_signals: cause={:?}, estat={:#x}, era={:#x}, badv={:#x}, badi={:#x}",
        cause, estat, era, badv, badi
    );
    handle_signals();
    println!(
        "[trap_handler] after handle_signals: cause={:?}, estat={:#x}, era={:#x}, badv={:#x}, badi={:#x}",
        cause, estat, era, badv, badi
    );*/
    /* 
    // check error signals (if error then exit)
    if let Some((errno, msg)) = check_signals_error_of_current() {
        trace!("[kernel] trap_handler: .. check signals {}", msg);
        exit_current_and_run_next(errno);
    }
    */
    /*println!(
        "[trap_return] estat={:#x}, era_csr={:#x}, badv={:#x}, badi={:#x}, next_era={:#x}, ra={:#x}, sp={:#x}",
        estat,
        era,
        badv,
        badi,
        current_trap_cx().get_rt(),
        current_trap_cx().r[1],
        current_trap_cx().r[3],
    );*/
    trap_return();
}


#[no_mangle]
/// return to user space
/// 参考riscv的实现，修改内存相关
pub fn trap_return() -> ! {
    //set_user_trap_entry();
    // 直接用物理地址
    handle_signals();
    let term_signal = current_task()
        .unwrap()
        .inner_exclusive_access()
        .term_signal;
    if let Some(signal) = term_signal {
        exit_current_and_run_next(-signal);
    }
    let trap_cx_ptr = current_trap_cx() as *mut TrapContext;
    let user_satp = current_user_token();
    let id = current_user_asid();
    //println!("[kernel] trap_return: user_satp={:#x}, id={:#x}, trap_cx_ptr={:#x}", user_satp, id, trap_cx_ptr as usize);
    unsafe {
        let mut euen: usize;
        asm!("csrrd {}, 0x2", out(reg) euen);
        euen |= 0x1;
        asm!("csrwr {}, 0x2", in(reg) euen);
    }
    flush_tlb_for_asid(id);
//  crate::arch::mm::la_app_init_mem(user_satp); //改为在restore中设置
    //info!("trap_return: going to user mode, satp = {:#x}", user_satp);
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    // la64因为是先切换特权级再跳，切换特权级时会自动关闭内存窗口可用性，不需要用跳板，restore直接跳转即可
    let restore = __restore as *const() as usize;
    // 调试打印，观察程序内存是否正常映射
    //let debug_buff = translated_byte_buffer(user_satp, 0x20_0000 as *const u8, 128);
    //println!("[kernel] trap_return: debug_buff = {:x?}", debug_buff);
    // 1_001000_0000_0000_0000_0000 访存指令地址错例外
    //println!("[kernel] calling __restore, address: {:#x}", restore);

    unsafe {
        asm!("csrwr {}, 0x18", in(reg) id); // 设置asid为pid
        asm!(
            "dbar 0", // 相当于sfence.vma
            "jr {restore}",
            restore = in(reg) restore,
            in("$a0") trap_cx_ptr,
            in("$a1") user_satp,
            options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn debug_print(){
    error!("[kernel] debug_print called");
}

#[no_mangle]
pub  extern "C" fn csr_info(){
    unsafe {
        let mut csr: usize;
        asm!("csrrd {}, 0x8C", out(reg) csr); // TLBRELO0?
        error!("[kernel] csr_info: TLBRELO0 = {:#x}", csr );
        //11001001001011000110010001
        asm!("csrrd {}, 0x8D", out(reg) csr); // TLBRELO1?
        //11001001001110000110010001
        error!("[kernel] csr_info: TLBRELO1 = {:#x}", csr );
    }
}

pub use context::TrapContext;

pub fn current_trap_cx_user_va() -> usize {
    current_task().unwrap().inner_exclusive_access().kernel_stack.get_top() - KERNEL_STACK_SIZE
}

pub fn trap_cx_va_by_kernel_stack(kernel_stack: &KernelStack) -> usize {
    let kernel_stack_top = kernel_stack.get_top();
    kernel_stack_top - KERNEL_STACK_SIZE
}