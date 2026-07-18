# 相较于riscv版本，其实只有语法不同，逻辑是一致的
    .section .text.entry
    .globl _start
    .align 8

_start:
    csrrd $tp, 0x20 # CSR_CPUNUM = 0x20
    la.global $sp, boot_stack_top
    # 每个核分配自己的启动栈（128KB，调试用临时增大）
    li.d $t0, 4096 * 32
    mul.d $t0, $t0, $tp
    sub.d $sp, $sp, $t0
    move $a0, $tp
    bl rust_main

    .section .bss.stack
    .globl boot_stack_lower_bound
    .align 8
boot_stack_lower_bound:
    .space 4096 * 32 * 4
    .globl boot_stack_top
boot_stack_top:
