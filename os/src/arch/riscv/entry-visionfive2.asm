    .equ BOOT_WINDOW, 0xffffffc000000000
    .equ EARLY_PHYS_BASE, 0x40000000
    .equ EARLY_NUM_GIB, 4
    .equ EARLY_PTE_FLAGS, 0xcf

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

    la t4, early_pg_dir
    li t5, EARLY_NUM_GIB
    li t3, EARLY_PHYS_BASE
    li t6, EARLY_PTE_FLAGS
1:
    srli t0, t3, 2
    or t0, t0, t6
    srli t1, t3, 30
    slli t1, t1, 3
    add t1, t4, t1
    sd t0, 0(t1)
    li t2, 2048
    add t1, t1, t2
    sd t0, 0(t1)
    li t2, 0x40000000
    add t3, t3, t2
    addi t5, t5, -1
    bnez t5, 1b

    la t0, early_pg_dir
    srli t0, t0, 12
    li t1, 0x8000000000000000
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    li t2, BOOT_WINDOW
    la t0, 2f
    add t0, t0, t2
    jr t0
2:
    la sp, boot_stack_top
    andi sp, sp, -16
    call rust_main

3:
    wfi
    j 3b

    .section .data.early_pg, "aw", @progbits
    .balign 4096
    .globl early_pg_dir
early_pg_dir:
    .space 4096

    .section .bss.stack
    .align 12
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space 4096 * 16

    .globl boot_stack_top
boot_stack_top:
