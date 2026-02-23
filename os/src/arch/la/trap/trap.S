# 模仿riscv版写成
.altmacro
.macro SAVE_GP n
    st.d $r\n, $sp, \n*8
.endm
.macro LOAD_GP n
    ld.d $r\n, $sp, \n*8
.endm
    .section .text.trampoline
    .globl __alltraps
    .globl __restore
    .globl csr_info 
    .align 4
__alltraps:
    # csrrw sp, sscratch, sp
    # 需要交换 sp 与 0x30(SAVE0) 寄存器的值
    csrwr $sp, 0x30 # 将用户栈地址写入0x30寄存器，供后续保存到TrapContext
    # now sp->*TrapContext in user space, 0x30->user stack
    st.d $r1, $sp, 1*8 
    st.d $r2, $sp, 2*8
    # 跳过sp(r3)
    # 保存r4~r20
    .set n, 4
    .rept 17
        SAVE_GP %n
        .set n, n+1
    .endr
    # 跳过r21(定义为不可分配)
    # 保存r22~r31
    .set n, 22
    .rept 10
        SAVE_GP %n
        .set n, n+1
    .endr

    # 将部分csr读取到t寄存器
    csrrd $t0, 0x1 # prmd
    csrrd $t1, 0x6
    # 将读取到的csr保存到TrapContext
    st.d $t0, $sp, 32*8
    st.d $t1, $sp, 33*8
    # 读取用户栈地址并保存到TrapContext
    csrrd $t2, 0x30 # SAVE0寄存器
    st.d $t2, $sp, 3*8
    # 从TrapContext中加载kernel_satp到t0
    ld.d $t0, $sp, 34*8 # 被预先保存在这里
    # 从TrapContext中加载trap_handler到t1
    ld.d $t1, $sp, 36*8 # 被预先保存在这里
    # 从TrapContext中加载kernel_sp到sp，切换到内核栈
    ld.d $sp, $sp, 35*8 # 被预先保存在这里
    # 切换到内核空间 (写入内核页表基址到 PGDH 0x1A)
    csrwr $t0, 0x1A 
    # 刷新TLB
    invtlb 0, $zero, $zero
    # 跳转到trap_handler
    jr $t1

# a0: trap_cx_ptr, a1: user_satp
__restore:
    la.global $t0, tlb_refill_handler
    csrwr $t0, 0x88 # 设置tlb重填异常处理函数
    move $t0, $a1
    csrwr $t0, 0x19 # 设置低半部为用户根页表
    la.global $t0, __alltraps
    csrwr $t0, 0xc # 例外入口地址
    invtlb 0, $zero, $zero # 刷新TLB

    # la的csrwr是交换，不能直接写入，需要备份到t0
    move $t0, $a0
    csrwr $t0, 0x30
    move $sp, $a0
    
    # 恢复之前保存的csr
    ld.d $t0, $sp, 32*8
    ld.d $t1, $sp, 33*8
    csrwr $t0, 0x1 #prmd
    csrwr $t1, 0x6

    # 恢复之前保存的通用寄存器
    ld.d $r1, $sp, 1*8
    ld.d $r2, $sp, 2*8
    .set n, 4
    .rept 17
        LOAD_GP %n
        .set n, n+1
    .endr
    .set n, 22
    .rept 10
        LOAD_GP %n
        .set n, n+1
    .endr
    ld.d $sp, $sp, 3*8 # 恢复trap前用户栈

    bl csr_info
    // 刷新tlb
    invtlb 0, $zero, $zero
    ertn