# 相较于riscv版本，其实只有语法不同，逻辑是一致的
    .section .text.entry
    .globl _start
    .align 4
_start:
    # ===== 全 L1 缓存刷新 =====
    # 遍历 0~64KB 索引空间，每次步进 cache line 大小 (64B)
    # 64KB 足够覆盖 LA264 最大 L1 D/I cache
    li.d    $t0, 0
    li.d    $t1, 0x10000             # 64KB 索引空间
1:
    cacop   0x00, $t0, 0             # D-Cache Index Invalidate
    cacop   0x08, $t0, 0             # I-Cache Index Invalidate
    addi.d  $t0, $t0, 64             # 下一 cache line
    bltu    $t0, $t1, 1b
    # 内存栅障：确保缓存操作完成，后续取指可见
    dbar 0
    ibar 0
    # ===== 获取 hart id，设置启动栈 =====
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
boot_stack_lower_bound:
    .space 4096 * 32 * 4
    .globl boot_stack_top
boot_stack_top:
