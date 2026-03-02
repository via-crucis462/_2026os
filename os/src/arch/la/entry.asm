# 相较于riscv版本，其实只有语法不同，逻辑是一致的
    .section .text.entry
    .globl _start
    .align 4
_start:
    la.global $sp, boot_stack_top
    bl rust_main

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space 4096 * 16
    .globl boot_stack_top
boot_stack_top:
