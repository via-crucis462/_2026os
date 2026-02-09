# LA64
# 思路：保存当前TC，切换内核栈，恢复下一个TC
.altmacro
.macro SAVE_SN n
    st.d $s\n, $a0, (\n+2)*8
.endm
.macro LOAD_SN n
    ld.d $s\n, $a1, (\n+2)*8
.endm
    .section .text
    .globl __switch
__switch:
    # __switch(
    #     current_task_cx_ptr: *mut TaskContext,
    #     next_task_cx_ptr: *const TaskContext
    # )
    # save kernel stack of current task
    st.d $sp, $a0, 8
    # save ra & s0~s11 of current execution
    st.d $ra, $a0, 0
    .set n, 0 
    .rept 10
        SAVE_SN %n
        .set n, n + 1
    .endr
    # restore ra & s0~s11 of next execution
    ld.d $ra, $a1, 0
    .set n, 0
    .rept 10
        LOAD_SN %n
        .set n, n + 1
    .endr
    # restore kernel stack of next task
    ld.d $sp, $a1, 8
    ret
