use super::frame::push_signal_frame;
use crate::process::{current_task, SignalFlags, TaskControlBlock, TaskStructInner};
use crate::process::trap::TrapContext;
use alloc::sync::Arc;

pub(crate) fn inner_has_pending_sigkill(inner: &TaskStructInner) -> bool {
    inner.pending.contains(SignalFlags::SIGKILL)
        || inner
            .signal
            .exclusive_access()
            .pending_flags()
            .contains(SignalFlags::SIGKILL)
}

pub(crate) fn has_pending_sigkill(task: &Arc<TaskControlBlock>) -> bool {
    let inner = task.inner_exclusive_access();
    inner_has_pending_sigkill(&inner)
}

/// 给当前任务加上信号
pub fn current_add_signal(signal: SignalFlags) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    task_inner.pending.insert(signal);
    // println!(
    //     "[K] current_add_signal:: current task sigflag {:?}",
    //     task_inner.signals
    // );
}

pub fn mark_signal_interrupted(task: &Arc<TaskControlBlock>) {
    let mut task_inner = task.inner_exclusive_access();
    task_inner.signal_interrupted = true;
}

pub fn take_current_signal_interrupted() -> bool {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let interrupted = task_inner.signal_interrupted;
    task_inner.signal_interrupted = false;
    interrupted
}

/// 处理信号
/// bug：目前的实现一次只处理一个信号，效率可能较低
pub fn handle_signals() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();


    // 不可被屏蔽的信号集
    let unmaskable = (SignalFlags::SIGKILL | SignalFlags::SIGSTOP);
    // 从掩码中移除不可屏蔽
    task_inner.blocked.remove(unmaskable);
    let thread_signals = task_inner.pending.flags();
    let signal = task_inner.signal.clone();
    let shared_signals = signal.exclusive_access().pending_flags();
    let raw_signals = thread_signals | shared_signals;
    let mask = task_inner.blocked;
    let pending = {
        let mut copy = raw_signals;
        copy.remove(mask);
        copy
    };

    let pending_bits = pending.bits();

    if pending_bits != 0 {
        // 内核m号信号flag刚好对应尾部m个0
        // 内核的处理函数统一用减一后的编号
        let sig = pending_bits.trailing_zeros() as usize;
        let flag = SignalFlags::from_bits(1 << sig).unwrap();
        if thread_signals.contains(flag) {
            task_inner.pending.remove(flag);
        } else {
            // A process-directed signal is consumed by exactly one unblocked
            // member of the thread group.
            signal.exclusive_access().remove_pending(flag);
        }
        drop(task_inner);
        // 跳到处理函数
        call_signal_handler(sig, flag);
    } else {
        if let Some(saved_mask) = task_inner.sigsuspend_saved_mask.take() {
            task_inner.blocked = saved_mask;
        }
        // 无待处理信号
        if raw_signals.bits() != 0 {
            warn!("[SIG PROBE] Signals exist ({:#x}) but fully masked ({:#x})", raw_signals, mask);
        }
    }
}

/// call user signal handler
/// 跳到用户态的信号处理函数
fn  call_signal_handler(sig: usize, signal: SignalFlags) {
    warn!("[SIG PROBE] Calling handler for sig: {}", sig);
    let task = current_task().unwrap();
    warn!("[SIG PROBE] Selected PID {} TID {}", task.getpid(), task.gettid());
    let mut task_inner = task.inner_exclusive_access();
    warn!("[SIG PROBE] Locked TID {}", task.gettid());
    let action = {
        task_inner.signal_hand.exclusive_access().action(sig)
    };
    let handler = action.handler;
    let mask = action.mask;
    warn!(
        "[SIG PROBE] Action sig={} handler={:#x} flags={:#x} mask={:#x}",
        sig + 1,
        handler,
        action.flags,
        mask.bits()
    );

    // handler如果是0，1 表示默认/忽略
    // 默认，表示由内核处理
    const SIG_DFL: usize = 0;
    // 忽略
    const SIG_IGN: usize = 1;

    let (saved_mask, restore_sigsuspend_mask) =
        match task_inner.sigsuspend_saved_mask.take() {
            Some(mask) => (mask, true),
            None => (task_inner.blocked, false),
        };

    if handler == SIG_IGN {
        if restore_sigsuspend_mask {
            task_inner.blocked = saved_mask;
        }
        return; // 返回，正常trap_return
    }
    
    if handler != SIG_DFL {
        // 非默认，回到用户态处理
        // 先保存 mask 和上下文
        task_inner.signal_mask_backup.push(saved_mask);
        
        // 屏蔽 action 中指定的掩码
        task_inner.blocked |= mask;

        const SA_NODEFER: usize = 0x40000000;
        // 如果没有 SA_NODEFER 标志，则在处理信号时自动屏蔽该信号
        if action.flags & SA_NODEFER == 0 {
            task_inner.blocked.insert(signal);
        }

        let trap_ctx = task_inner.get_trap_cx();
        task_inner.trap_ctx_backup.push(*trap_ctx);
        warn!(
            "[SIG PROBE] Building frame pc={:#x} sp={:#x}",
            trap_ctx.get_rt(),
            trap_ctx.get_sp()
        );
        const SA_ONSTACK: usize = 0x08000000;
        let Some((info_ptr, ucontext_ptr)) = push_signal_frame(
            &mut task_inner,
            sig,
            saved_mask,
            action.flags & SA_ONSTACK != 0,
        ) else {
            warn!("[SIG PROBE] Failed to write signal frame");
            task_inner.term_signal = Some(sig as i32 + 1);
            return;
        };
        warn!(
            "[SIG PROBE] Frame ready info={:#x} uctx={:#x}",
            info_ptr,
            ucontext_ptr
        );

        #[cfg(target_arch = "riscv64")]
        if sig + 1 == 33 {
            warn!(
                "[SIGCANCEL TP] tid={} handler={:#x} pc={:#x} sp={:#x} ra={:#x} tp={:#x} info={:#x} uctx={:#x}",
                task.gettid(),
                handler,
                trap_ctx.get_rt(),
                trap_ctx.get_sp(),
                trap_ctx.x[1],
                trap_ctx.x[4],
                info_ptr,
                ucontext_ptr
            );
        }
        
        trap_ctx.set_rt(handler);
        trap_ctx.set_a0(sig + 1 /* 内核编号->用户编号 */);
        trap_ctx.set_a1(info_ptr);
        trap_ctx.set_a2(ucontext_ptr);
        trap_ctx.set_sp(info_ptr);
        // 保证信号处理完恢复
        set_sig_ret(trap_ctx);
        warn!(
            "[SIG PROBE] Returning to handler pc={:#x} sp={:#x}",
            trap_ctx.get_rt(),
            trap_ctx.get_sp()
        );
    } else { 
        warn!(
            "[SIG PROBE] PID {} (tid {}) default handling for signal {} ({:?})",
            task.getpid(),
            task.gettid(),
            sig,
            signal
        );
        // 由内核处理
        match signal {
            SignalFlags::SIGCHLD 
            | SignalFlags::SIGURG 
            | SignalFlags::SIGWINCH => {
                // 目前的实现这些默认忽略
                info!("[K] ignore default signal {:?}", signal);
            }
             SignalFlags::SIGSTOP => {
                task_inner.frozen = true;
            }
            SignalFlags::SIGCONT => { // continue
                task_inner.frozen = false;
            }
            _ => {
                // 其他信号默认杀死任务
                // 此处标注为kill后稍后会调用exit_current_and_run_next，这里不直接调用
                task_inner.term_signal = Some(sig as i32 + 1);
                let pid = task.getpid();
                warn!("[SIG_DEATH] PID {} killed by signal {} ({:?})", pid, sig as i32 + 1, signal);
            }
        }
        if restore_sigsuspend_mask {
            task_inner.blocked = saved_mask;
        }
    }
}

fn set_sig_ret(trap_ctx: &mut TrapContext) {
    use crate::arch::config::*;
    trap_ctx.set_ra(*SIG_RT_ADDR);
}

/// 检查当前任务是否有未屏蔽的挂起信号
pub fn check_pending_signal() -> bool {
    let pending = get_pending_signals();
    if pending.is_empty() {
        return false;
    }

    const SIG_DFL: usize = 0;
    const SIG_IGN: usize = 1;
    let task = current_task().unwrap();
    let signal_hand = task.inner_exclusive_access().signal_hand.clone();
    let actions = signal_hand.exclusive_access();
    let mut bits = pending.bits();

    while bits != 0 {
        let sig = bits.trailing_zeros() as usize;
        bits &= !(1u64 << sig);
        let action = actions.action(sig);

        if action.handler == SIG_IGN {
            continue;
        }
        if action.handler == SIG_DFL {
            let flag = SignalFlags::from_bits(1u64 << sig)
                .unwrap_or(SignalFlags::empty());
            if matches!(
                flag,
                SignalFlags::SIGCHLD | SignalFlags::SIGURG | SignalFlags::SIGWINCH
            ) {
                continue;
            }
        }
        return true;
    }

    false
}

/// 当前首个会实际递送的信号是否要求重启被中断的系统调用。
pub fn pending_signal_should_restart() -> bool {
    const SIG_DFL: usize = 0;
    const SIG_IGN: usize = 1;
    const SA_RESTART: usize = 0x1000_0000;

    let pending = get_pending_signals();
    let task = current_task().unwrap();
    let signal_hand = task.inner_exclusive_access().signal_hand.clone();
    let actions = signal_hand.exclusive_access();
    let mut bits = pending.bits();

    while bits != 0 {
        let sig = bits.trailing_zeros() as usize;
        bits &= !(1u64 << sig);
        let action = actions.action(sig);
        if action.handler == SIG_IGN {
            continue;
        }
        if action.handler == SIG_DFL {
            let flag = SignalFlags::from_bits(1u64 << sig)
                .unwrap_or(SignalFlags::empty());
            if matches!(
                flag,
                SignalFlags::SIGCHLD | SignalFlags::SIGURG | SignalFlags::SIGWINCH
            ) {
                continue;
            }
            return false;
        }
        return action.flags & SA_RESTART != 0;
    }

    false
}

/// 返回当前私有和共享的未屏蔽的挂起信号集
pub fn get_pending_signals() -> SignalFlags {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    let thread_pending = task_inner.pending.flags();
    let shared_pending = task_inner.signal.exclusive_access().pending_flags();
    let raw_signals = thread_pending | shared_pending;
    let pending = raw_signals.bits() & !(
        task_inner.blocked.bits() & 
        !(SignalFlags::SIGKILL | SignalFlags::SIGSTOP).bits()
    );
    debug!("current task get_pending_signals: {:?}", pending);
    SignalFlags::from_bits(pending).unwrap_or(SignalFlags::empty())
}
