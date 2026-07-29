    .section .text
    .globl tlb_refill_handler
    # TLB 重填例外入口地址[11:0]位。只读恒为 0，写被忽略
    # 需要 4KB 对齐
    .align 12
tlb_refill_handler:
    // 临时保存 t0 寄存器，否则会被覆盖
    csrwr $t0, 0x8B
    // PGD 由硬件按 TLBRBADV 所在的高/低半地址空间选择 PGDH 或 PGDL
    csrrd $t0, 0x1B
    // 摘自手册：
    // “LDDIR、LDPTE指令执行所需的出错虚地址信息
    // 将来自于CSR.TLBRBADV”
    // 根据触发异常的va逐级遍历dir2,dir1,pt
    lddir $t0, $t0, 2
    // LDDIR 返回的是目录 PTE，低 12 位为权限等标志而不是下级页表地址。
    srli.d $t0, $t0, 12
    slli.d $t0, $t0, 12
    lddir $t0, $t0, 1
    srli.d $t0, $t0, 12
    slli.d $t0, $t0, 12
    // la64“双页”，奇偶分别处理
    ldpte $t0, 0
    ldpte $t0, 1
    tlbfill
    
    csrrd $t0, 0x8B
    ertn
