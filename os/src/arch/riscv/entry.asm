//! sbi会自动把hartid放在a0寄存器中
//! 另外，新规范中，sbi在机器启动时会先只启动一个核，需要内核用sbicall启动其余的核
//! 具体需查询规范手册
    .equ BOOT_STACK_SHIFT, 16
    .equ BOOT_HARTS, 8
    .section .text.entry
    .globl _start
_start:
    mv tp, a0 // 将hartid存入tp
    slli t0, a0, BOOT_STACK_SHIFT //等效于*65536
    la sp, boot_stack_top
    sub sp, sp, t0 //为每个核分配4KB*16的栈空间
    call rust_main

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space (1 << BOOT_STACK_SHIFT) * BOOT_HARTS
    .globl boot_stack_top
boot_stack_top: