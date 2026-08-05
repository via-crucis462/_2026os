.altmacro
.macro SAVE_GP n
    sd x\n, \n*8(sp)
.endm
.macro LOAD_GP n
    ld x\n, \n*8(sp)
.endm
    .section .text.trampoline
    .globl __alltraps
    .globl __restore
    .globl __call_sig_rt
    .global debug_info
    .align 2
__alltraps:
    csrrw sp, sscratch, sp
    # 这时 sp 指向TrapContext，sscratch 指向用户栈
    sd x1, 1*8(sp)
    # x2是栈指针，要等数据都压入栈再最后压
    sd x3, 3*8(sp)
    sd x4, 4*8(sp)
    # load kernel tp(hart id) from TrapContext extension field
    ld x4, 37*8(sp)
    # save x5~x31
    .set n, 5
    .rept 27
        SAVE_GP %n
        .set n, n+1
    .endr
    # we can use t0/t1/t2 freely, because they have been saved in TrapContext
    csrr t0, sstatus
    csrr t1, sepc
    sd t0, 32*8(sp)
    sd t1, 33*8(sp)
    # read user stack from sscratch and save it in TrapContext
    csrr t2, sscratch
    sd t2, 2*8(sp)
    # load trap_handler into t1
    ld t1, 36*8(sp)
    # move to kernel_sp 实际上是内核栈顶，初始化后不再变化，所以restore时不需要保存
    ld sp, 35*8(sp)
    # jump to trap_handler
    jr t1

__restore:
    # trap_return 内已经 switch_mm，这里不需要也不能再切换 satp
    csrw sscratch, a0
    mv sp, a0
    # restore from the kernel-owned TrapContext through its transition mapping
    # restore sstatus/sepc
    ld t0, 32*8(sp)
    ld t1, 33*8(sp)
    csrw sstatus, t0
    csrw sepc, t1
    # save kernel tp(hart id) for next trap entry before restoring user tp
    sd x4, 37*8(sp)
    ld x4, 4*8(sp)
    # restore general purpose registers except x0/sp
    ld x1, 1*8(sp)
    ld x3, 3*8(sp)
    
    .set n, 5
    .rept 27
        LOAD_GP %n
        .set n, n+1
    .endr
    
    # back to user stack
    ld sp, 2*8(sp)
    sret

# 信号处理后恢复原本上下文
__call_sig_rt:
    li a7, 139
    ecall
