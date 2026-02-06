//! clone相关函数的实现
//! 尚未完成
use crate::task::*;

///__clone(fn, stack, flags, NULL, NULL, NULL);
///测例中默认不指定ctid和ptid
///flags暂时未使用
pub fn do_clone(func: usize, stack: usize, _flags: usize) -> isize {
    // 调试信息
    println!("[K] do_clone: func={:#x}, stack={:#x}, flags={:#x}", func, stack, _flags);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork(
        if stack != 0 { Some(stack) } 
        else { None }
    );
    let new_pid = new_task.pid.0;
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, clone returns 0(参考rcore的实现)
    trap_cx.x[10] = 0;
    // 设置子进程的起始函数，如果指定
    //if func != 0 {
    //    trap_cx.sepc = func;
    //}
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}