    .section .text.entry
    .globl _start

_start:
    # 暂时关闭所有 S 态中断源
    csrw sie, zero

    # 关闭 S 态全局中断使能位 sstatus.SIE
    li t0, 2
    csrc sstatus, t0

    # U-Boot `go` 进入时，a0 是 argc，不是 hartid。
    # 当前只启动逻辑 hart 0。
    li tp, 0
    li a0, 0

    # 当前没有可信的设备树地址
    li a1, 0

    # 使用单核启动栈，并保证 16 字节对齐
    la sp, boot_stack_top
    andi sp, sp, -16

    call rust_main

    # rust_main 的返回类型是 -> !，正常情况下不会到这里
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