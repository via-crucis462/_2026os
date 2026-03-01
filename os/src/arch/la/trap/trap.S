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
    .align 2
__alltraps:
    # csrrw sp, sscratch, sp
    # 需要交换 sp 与 0x502(SAVE0) 寄存器的值
    csrwr $sp, 0x502 # 将用户栈地址写入0x502寄存器，供后续保存到TrapContext
    # now sp->*TrapContext, 0x502->user stack
    addi.d  $sp, $sp, -8*35 # 为TrapContext预留空间

    st.d $r1, $sp, 1*8 
    st.d $r2, $sp, 2*8
    # 跳过sp(r3)
    # 保存r4~r20
    .set n, 4
    .rept 28
        SAVE_GP %n
        .set n, n+1
    .endr

    # 将部分csr读取到t寄存器
    csrrd $t0, 0x1 # prmd
    csrrd $t1, 0x6 # era

    # 将读取到的csr保存到TrapContext
    st.d $t0, $sp, 8*32
    st.d $t1, $sp, 8*33

    # 读取用户栈地址并保存到TrapContext
    csrrd $t2, 0x502 #读出用户栈指针
    st.d $t2, $sp, 8*3

    bl trap_handler

# a0: trap_cx_ptr, a1: user_satp
__restore:
    ///一系列csr设置
    la.global $t0, tlb_refill_handler
    csrwr $t0, 0x88 # 设置tlb重填异常处理函数
    move $t0, $a1
    csrwr $t0, 0x19 # 设置低半部为用户根页表
    la.global $t0, __alltraps
    csrwr $t0, 0xc # 例外入口地址
    ///......

    move $sp, $a0
    
    # 恢复之前保存的csr
    ld.d $t0, $sp, 8*32
    ld.d $t1, $sp, 8*33
    csrwr $t0, 0x1 #prmd
    csrwr $t1, 0x6

    # 放置内核栈（tc）指针到0x502寄存器
    ld.d $t2, $sp, 8*3
    csrwr $t2, 0x502

    # 恢复之前保存的通用寄存器
    ld.d $r1, $sp, 8*1
    ld.d $r2, $sp, 8*2
    # 跳过sp(r3)
    .set n, 4
    .rept 28
        LOAD_GP %n
        .set n, n+1
    .endr
    addi.d $sp, $sp, 35*8

    # 交换栈指针
    csrwr $sp, 0x502
    # now sp->user_stack, 0x502->*TrapContext
    ertn