// 正在为la64重写
// 参考https://godones.github.io/rCoreloongArch/app.html
use crate::process::processor::current_user_asid;
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
#[cfg(board = "virt")]
use crate::net::net_poll;
use alloc::sync::Arc;
use core::arch::{asm, global_asm};
global_asm!(include_str!("trap.S"));

extern "C" {
    fn __alltraps();
    fn __k_alltraps();
}

use core::sync::atomic::{AtomicUsize, Ordering};

static UBOOT_TRAP_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// 诊断：k_trap_handler 被调用的次数
pub static K_TRAP_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn get_uboot_trap_handler_from_csr() {
    let eentry: usize;
    unsafe { asm!("csrrd {}, 0xc", out(reg) eentry); }
    UBOOT_TRAP_HANDLER.store(eentry, Ordering::SeqCst);
}

pub fn set_uboot_trap_handler_to_csr() {
    let eentry = UBOOT_TRAP_HANDLER.load(Ordering::SeqCst);
    if eentry == 0 { return; }
    unsafe { asm!("csrwr {}, 0xc", in(reg) eentry); }
}

/// 内核态trap处理入口，由 __k_alltraps 调用
/// trap_cx 指向栈上保存的 TrapContext
/// 目前主要用于处理未对齐访存异常（ALE）
#[no_mangle]
pub extern "C" fn k_trap_handler(trap_cx: *mut TrapContext) {
    // 诊断用，递增调用计数
    K_TRAP_COUNT.fetch_add(1, Ordering::SeqCst);
    trace!("[kernel] k_trap_handler called, count={:#x}", K_TRAP_COUNT.load(Ordering::SeqCst));

    // 防止嵌套 trap 循环
    set_uboot_trap_handler_to_csr();

    let cx = unsafe { &mut *trap_cx };

    let estat: usize;
    let badv: usize;
    unsafe {
        asm!("csrrd {}, 0x5", out(reg) estat);
        asm!("csrrd {}, 0x7", out(reg) badv);
    }

    let ecode = (estat >> 16) & 0x3f;

    match ecode {
        0x9 => { // ALE — 未对齐访存

            // FIX ME：如果只保存rd rj rk，可以大幅提高性能
            // 现在通过一个完整trap处理
            
            let era = cx.get_rt();
            let bad_ins = unsafe { *(era as *const u32) };
            let rd = (bad_ins & 0x1F) as usize;
            let opcode_10 = bad_ins >> 22;

            // 下面这部分当前是LLM批量实现版本，可能有优化空间
            match opcode_10 {
                // si12 类型: ld.h/w/d, st.h/w/d, ld.hu/wu
                0x0A1 | 0x0A2 | 0x0A3 | 0x0A5 | 0x0A6 | 0x0A7 | 0x0A9 | 0x0AA => {
                    let rj = ((bad_ins >> 5) & 0x1F) as usize;
                    let si12 = (bad_ins >> 10) & 0xFFF;
                    let si12_ext = ((si12 & 0xFFF) as i64) << 52 >> 52;
                    let vaddr = (cx.r[rj] as i64).wrapping_add(si12_ext) as usize;
                    // 诊断：对比硬件 badv
                    if vaddr != badv {
                        panic!("[ALE] si12 vaddr mismatch: vaddr=0x{:x}, badv=0x{:x}, rj={}, cx.r[rj]=0x{:x}, si12_ext=0x{:x}",
                            vaddr, badv, rj, cx.r[rj], si12_ext);
                    }
                    match opcode_10 {
                        0x0A1 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u16).read_unaligned() } as i16 as i64 as usize; } }       // LD.H
                        0x0A2 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u32).read_unaligned() } as i32 as i64 as usize; } }       // LD.W
                        0x0A3 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u64).read_unaligned() } as usize; } }                     // LD.D
                        0x0A5 => { let v = if rd != 0 { cx.r[rd] as u16 } else { 0 }; unsafe { (vaddr as *mut u16).write_unaligned(v); } } // ST.H
                        0x0A6 => { let v = if rd != 0 { cx.r[rd] as u32 } else { 0 }; unsafe { (vaddr as *mut u32).write_unaligned(v); } } // ST.W
                        0x0A7 => { let v = if rd != 0 { cx.r[rd] as u64 } else { 0 }; unsafe { (vaddr as *mut u64).write_unaligned(v); } } // ST.D
                        0x0A9 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u16).read_unaligned() } as usize; } }                      // LD.HU
                        0x0AA => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u32).read_unaligned() } as usize; } }                      // LD.WU
                        _ => {}
                    }
                    // 绕过触发trap的指令
                    cx.set_rt(era + 4);
                }
                0x0A0 | 0x0A4 | 0x0A8 => { // 字节访存永远不应触发 ALE
                    panic!("[kernel] ALE on byte access: badv=0x{:x}, era=0x{:x}", badv, era);
                }
                // rk 类型: ldx/stx, opcode 在 bits 31-16
                _ => {
                    let opcode_16 = bad_ins >> 16;
                    match opcode_16 {
                        0x3804 | 0x3808 | 0x380C | 0x3814 | 0x3818 | 0x381C | 0x3824 | 0x3828 => {
                            let rj = ((bad_ins >> 5) & 0x1F) as usize;
                            let rk = ((bad_ins >> 10) & 0x1F) as usize;
                            let vaddr = cx.r[rj].wrapping_add(cx.r[rk]);
                            match opcode_16 {
                                0x3804 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u16).read_unaligned() } as i16 as i64 as usize; } }       // LDX.H
                                0x3808 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u32).read_unaligned() } as i32 as i64 as usize; } }       // LDX.W
                                0x380C => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u64).read_unaligned() } as usize; } }                     // LDX.D
                                0x3814 => { let v = if rd != 0 { cx.r[rd] as u16 } else { 0 }; unsafe { (vaddr as *mut u16).write_unaligned(v); } } // STX.H
                                0x3818 => { let v = if rd != 0 { cx.r[rd] as u32 } else { 0 }; unsafe { (vaddr as *mut u32).write_unaligned(v); } } // STX.W
                                0x381C => { let v = if rd != 0 { cx.r[rd] as u64 } else { 0 }; unsafe { (vaddr as *mut u64).write_unaligned(v); } } // STX.D
                                0x3824 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u16).read_unaligned() } as usize; } }                      // LDX.HU
                                0x3828 => { if rd != 0 { cx.r[rd] = unsafe { (vaddr as *const u32).read_unaligned() } as usize; } }                      // LDX.WU
                                _ => {}
                            }
                            cx.set_rt(era + 4);
                        }
                        0x3800 | 0x3810 | 0x3820 => {
                            panic!("[kernel] ALE on byte access (rk): badv=0x{:x}, era=0x{:x}", badv, era);
                        }
                        _ => {
                            panic!("[kernel] unhandled ALE (rk): bad_ins=0x{:08x}, opcode_16=0x{:04x}, era=0x{:x}, badv=0x{:x}",
                                bad_ins, opcode_16, era, badv);
                        }
                    }
                }
            }
        }
        _ => {
            panic!("[kernel] unhandled kernel trap: ecode=0x{:x}, era=0x{:x}, badv=0x{:x}",
                ecode, cx.get_rt(), badv);
        }
    }

    // 恢复内核 trap 入口
    set_kernel_trap_entry();

    // 这里会自动返回到 __k_restore
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
        "[kernel][panic] trap_from_kernel: hart={}, ESTAT=0x{:x}, ECODE={}(0x{:x}), ESUBCODE=0x{:x}, timer_pending={}, ERA=0x{:x}, BADV=0x{:x}, BADI=0x{:x}",
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
        let proc = task.process();
        let inner = proc.inner_exclusive_access();
        error!(
            "[kernel][panic] current task snapshot: pid={}, tid={}, heap_bottom=0x{:x}, program_brk=0x{:x}",
            task.getpid(),
            task.gettid(),
            inner.heap_bottom,
            inner.program_brk,
        );
        inner.memory_set.debug_dump_areas(Some(badv), Some(era));
    } else {
        error!("[kernel][panic] no current task on this hart");
    }
    debug!(
        "a trap from kernel! hart={}, estat=0x{:x}, ecode={}(0x{:x}), esubcode=0x{:x}, timer_pending={}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}",
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
    get_uboot_trap_handler_from_csr();
    let target = __k_alltraps as *const () as usize;
    // 诊断：打印 __k_alltraps 地址和 U-Boot EENTRY 值
    let uboot_eentry = UBOOT_TRAP_HANDLER.load(Ordering::SeqCst);
    println!("[kernel] trap::init: uboot_eentry=0x{:x}, __k_alltraps=0x{:x}", uboot_eentry, target);
    set_kernel_trap_entry();
    // 回读确认
    let verify: usize;
    unsafe { asm!("csrrd {}, 0xc", out(reg) verify); }
    println!("[kernel] trap::init: EENTRY after set=0x{:x}", verify);
}

// 从内核trap时的入口
fn set_kernel_trap_entry() {
    let target = __k_alltraps as *const () as usize;
    unsafe {
        asm!(
            "csrwr {},0xc",
            inout(reg) target => _,
        );
    }
}

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
            let proc = task.process();
            let inner = proc.inner_exclusive_access();
            inner.pname == BRK_PROCESS_NAME || inner.pname.ends_with("/brk")
        })
        .unwrap_or(false)
}

fn debug_dump_user_stack_window(tag: &str, token: usize, sp: usize, words: usize) {
    let page_table = PageTable::from_token(token);
    error!(
        "[kernel] brk_stack {}: sp=0x{:x}, dumping {} words",
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
                    "[kernel] brk_stack {}: [0x{:x}] = 0x{:x}",
                    tag,
                    va,
                    value
                );
            }
            None => {
                error!(
                    "[kernel] brk_stack {}: [0x{:x}] = <unmapped>",
                    tag,
                    va
                );
            }
        }
    }
}

fn debug_dump_brk_snapshot(tag: &str, cx: &TrapContext, token: usize) {
    error!(
        "[kernel] brk_trace {}: era=0x{:x}, user_sp=0x{:x}, a0=0x{:x}, a1=0x{:x}, a2=0x{:x}, a3=0x{:x}, a4=0x{:x}, a5=0x{:x}, a6=0x{:x}, a7=0x{:x}",
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
    trace!("[kernel] called trap_handler");
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
            trace!("[kernel] trap_handler: syscall_id={}, pid={}, tid={}, hart_id={}, era=0x{:x} , ra=0x{:x}, sp=0x{:x}", syscall_id, current_task().unwrap().getpid(), current_tid(), get_hart_id(), cx.get_rt(), cx.r[1], cx.r[2]);
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
                    let process_inner = process.inner_exclusive_access();

                    for task in process_inner.tasks.iter() {
                        let mut task_inner = task.inner_exclusive_access();
                        task_inner.signals |= crate::task::SignalFlags::SIGALRM;
                        if task_inner.task_status == crate::task::TaskStatus::Blocked {
                            task_inner.signal_interrupted = true;
                            task_inner.task_status = crate::task::TaskStatus::Ready;
                            crate::task::add_task(Arc::clone(task));
                        }
                    }
                }
            }
            #[cfg(board = "virt")]
            net_poll();
            // crate::mm::mmap::tick_sync();
            suspend_current_and_run_next();
        }
        _ => {
            debug!("[trap] unhandled trap: hart_id={}, estat=0x{:x}, ecode={}(0x{:x}), esubcode=0x{:x}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}",
                get_hart_id(),
                estat,
                ecode_name,
                ecode,
                esubcode,
                era,
                badv,
                badi
            );
            if let Some(task) = current_task() {
                let proc = task.process();
                let mut inner = proc.inner_exclusive_access();
                let sp = current_trap_cx().r[3];
                let vpn = VirtAddr::from(badv).std_floor();
                if ecode == 4 {
                    if let Some(pte) = inner.memory_set.translate(vpn) {
                        if pte.is_valid() && pte.writable() && inner.memory_set.set_pte_dirty(vpn) {
                            drop(inner);
                            drop(proc);
                            drop(task);
                            trap_return();
                        }
                    }
                }
                if inner.memory_set.handle_cow_fault(badv) {
                    drop(inner);
                    drop(proc);
                    drop(task);
                    trap_return();
                }
                if inner.memory_set.handle_page_fault(badv, sp) {
                    drop(inner);
                    drop(proc);
                    drop(task);
                    trap_return();
                }
                match inner.memory_set.translate(vpn) {
                    Some(pte) => {
                        trace!(
                            "[kernel] user_fault_pte: current hart id={}, estat=0x{:x}, ecode={}(0x{:x}), esubcode=0x{:x}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}, ra=0x{:x}, sp=0x{:x}, vpn=0x{:x}, pte_bits=0x{:x}, valid={}, r={}, w={}, x={}",
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
                            "[kernel] user_fault_pte: current hart id={}, estat=0x{:x}, ecode={}(0x{:x}), esubcode=0x{:x}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}, ra=0x{:x}, sp=0x{:x}, vpn=0x{:x}, pte=<none>",
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
                            "[BRK诊断] hw_pgdl=0x{:x}, hw_asid=0x{:x}, 软件pgdl=0x{:x}",
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
                                    "[BRK诊断] era=0x{:x} vpn=0x{:x} 软件ppn=0x{:x} pte=0x{:x} 物理指令={:#010x} badi={:#010x}",
                                    era, era_vpn.0, era_ppn.0, era_pte.bits, w, badi
                                );
                                error!(
                                    "[BRK诊断] 硬件页表查找: hw_ppn=0x{:x} hw_pte=0x{:x}",
                                    hw_ppn_val, hw_pte_bits
                                );
                            }
                            None => {
                                error!("[BRK诊断] era=0x{:x}, era_vpn=0x{:x}, 软件页表无PTE!", era, era_vpn.0);
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
                let proc = task.process();
                let inner = proc.inner_exclusive_access();
                trace!(
                    "[kernel] trap_handler: pid={}, tid={}, heap_bottom=0x{:x}, program_brk=0x{:x}",
                    task.getpid(),
                    task.gettid(),
                    inner.heap_bottom,
                    inner.program_brk,
                );
                // inner.memory_set.debug_dump_areas(Some(badv), Some(era));
            } else {
                trace!("[kernel] trap_handler: no current task");
            }
            current_add_signal(SignalFlags::SIGSEGV);
        }
    }
    /*println!(
        "[trap_handler] before handle_signals: cause={:?}, estat=0x{:x}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}",
        cause, estat, era, badv, badi
    );
    handle_signals();
    println!(
        "[trap_handler] after handle_signals: cause={:?}, estat=0x{:x}, era=0x{:x}, badv=0x{:x}, badi=0x{:x}",
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
        "[trap_return] estat=0x{:x}, era_csr=0x{:x}, badv=0x{:x}, badi=0x{:x}, next_era=0x{:x}, ra=0x{:x}, sp=0x{:x}",
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
    trace!("[kernel] trap_return: returning to user space");
    handle_signals();
    let term_signal = {
        let task = current_task().unwrap();
        let inner = task.inner_exclusive_access();
        if inner.killed {
            inner.term_signal.unwrap_or(1)
        } else {
            0
        }
    };
    if term_signal != 0 {
        exit_current_and_run_next(-term_signal);
    }

    // 直接用物理地址
    let trap_cx_ptr = current_trap_cx() as *mut TrapContext;
    let user_satp = current_user_token();
    let id = current_user_asid();
    //println!("[kernel] trap_return: user_satp=0x{:x}, id=0x{:x}, trap_cx_ptr=0x{:x}", user_satp, id, trap_cx_ptr as usize);
    unsafe {
        let mut euen: usize;
        asm!("csrrd {}, 0x2", out(reg) euen);
        euen |= 0x1;
        asm!("csrwr {}, 0x2", in(reg) euen);
    }
    flush_tlb_for_asid(id);
    crate::arch::mm::prepare_user_tlb();
    // crate::arch::mm::la_app_init_mem(user_satp); //改为在restore中设置
    trace!("trap_return: going to user mode, satp = 0x{:x}", user_satp);
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
    //println!("[kernel] calling __restore, address: 0x{:x}", restore);

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

pub use context::TrapContext;

pub fn current_trap_cx_user_va() -> usize {
    current_task().unwrap().kernel_stack.get_top() - KERNEL_STACK_SIZE
}

pub fn trap_cx_va_by_kernel_stack(kernel_stack: &KernelStack) -> usize {
    let kernel_stack_top = kernel_stack.get_top();
    kernel_stack_top - KERNEL_STACK_SIZE
}
