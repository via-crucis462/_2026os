    .section .text.entry
    .globl _start

_start:
    csrw sie, zero
    li t0, 2
    csrc sstatus, t0

    li tp, 0
    li a0, 0
    li a1, 0

    la sp, boot_stack_top
    andi sp, sp, -16
    call rust_main

1:
    wfi
    j 1b

    .section .bss.stack
    .align 12
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space 4096 * 16

    .globl boot_stack_top
boot_stack_top:
