# loongarch64 entry point
    .section .text.entry
    .globl _start
    .align 4
_start:
    la.global $sp, boot_stack_top
    call36 rust_main

1:
    b 1b

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space 4096 * 16
    .globl boot_stack_top
boot_stack_top:
