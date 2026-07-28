# 相较于riscv版本，其实只有语法不同，逻辑是一致的
    .equ BOOT_STACK_SIZE, 4096 * 16
    .equ BOOT_HARTS, 8
    .section .text.entry
    .globl _start
    .align 4
_start:
    csrrd $tp, 0x20 # CSR_CPUNUM = 0x20
    la.global $sp, boot_stack_top
    # 每个核分配自己的启动栈
    li.d $t0, BOOT_STACK_SIZE
    mul.d $t0, $t0, $tp
    sub.d $sp, $sp, $t0
    move $a0, $tp
    bl rust_main

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space BOOT_STACK_SIZE * BOOT_HARTS
    .globl boot_stack_top
boot_stack_top:
