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
    // 懒分配时对应虚拟页的目录可能还没建立，读到的目录项为空，
    // 给对应 tlb 项填 0 主动触发一次缺页异常
    beqz $t0, .Lrefill_invalid
    // LDDIR 返回的是目录 PTE，低 12 位为权限等标志而不是下级页表地址
    srli.d $t0, $t0, 12
    slli.d $t0, $t0, 12
    lddir $t0, $t0, 1
    // 同上，读到的目录项为 0，给对应 tlb 项填 0 主动触发一次缺页异常
    beqz $t0, .Lrefill_invalid
    srli.d $t0, $t0, 12
    slli.d $t0, $t0, 12
    // la64“双页”，奇偶分别处理
    ldpte $t0, 0
    ldpte $t0, 1
    tlbfill

.Lrefill_restore:
    csrrd $t0, 0x8B
    ertn

.Lrefill_invalid:
    // 主动置 0，在 tlbfill 后相应 tlb 项会被填为 0
    csrwr $zero, 0x8C // TLBRELO0
    csrwr $zero, 0x8D // TLBRELO1
    tlbfill
    // 回到用户态对同一地址访存不再触发 tlb 重填，
    // 而是因权限问题触发缺页异常，在缺页异常中建立页表，
    // 然后就能正常 tlb 重填了
    b .Lrefill_restore
