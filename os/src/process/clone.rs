//! clone相关函数的实现
//! 尚未完成
use crate::task::*;



///__clone(fn, stack, flags, NULL, NULL, NULL);
///测例中默认不指定ctid和ptid
///flags暂时未使用
pub fn do_fork(_func: usize, stack: usize, _flags: usize) -> isize {
    let current_task = current_task().unwrap();
    let current_proc = current_task.process();
    let (new_proc, new_task) = current_proc.fork(
        if stack != 0 { Some(stack) } 
        else { None }, current_task.clone()
    );
    let new_pid = new_proc.pid.0;
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, clone returns 0(参考rcore的实现)
    trap_cx.set_a0(0);
    // 设置子进程的起始函数，如果指定
    //if func != 0 {
    //    trap_cx.sepc = func;
    //}
    // add new task to scheduler
    add_process(new_proc);
    add_task(new_task);
    //println!("forked process: {}", new_pid);
    new_pid as isize
}

pub fn do_clone_thread(_func: usize, stack: usize, _flags: usize, _ptid: usize) -> isize {
    let current_task = current_task().unwrap();
    let current_proc = current_task.process();
    let new_task = current_proc.clone_thread((stack != 0).then_some(stack), current_task);
    let new_tid = new_task.gettid();
    add_task(new_task);
    new_tid as isize
}