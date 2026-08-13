//! 参考 linux 的实现，将内核链接到高半地址空间，在启动阶段构造临时页表
//! 
//! 临时页表采用 1GB 大页映射 16G RAM 两次（分别为恒等和带窗口），32 个页表项

    .equ BOOT_STACK_SHIFT, 16
    .equ BOOT_HARTS, {boot_harts}
    // 内核高半窗口基址，必须与 KERNEL_WINDOW_BASE 一致
    // 链接脚本 BASE_ADDRESS = BOOT_WINDOW + 0x80200000
    .equ BOOT_WINDOW, 0xffffffc000000000

    // RAM 物理起点
    .equ EARLY_PHYS_BASE, 0x80000000
    // RAM 大小
    .equ EARLY_NUM_GIB, 16 // 16 个 1GB 页表项
    // 早期 1GB 叶 PTE 标志：V|R|W|X|A|D
    .equ EARLY_PTE_FLAGS, 0xcf

    .section .text.entry
    .globl _start
_start:
    // 刚开机时使用还未开启分页，通过物理地址访存
    // 走到这里时 pc 是链接脚本中的 KERNEL_PHYS_LOAD

    // OpenSBI 会将 hart_id 传入 a0
    mv   tp, a0 // 将 id 保存到 tp
    slli t0, a0, BOOT_STACK_SHIFT
    la   sp, boot_stack_top
    sub  sp, sp, t0

    // 这里会使用 pc 相对寻址找 early_pg_dir 符号的地址
    la   t4, early_pg_dir 
    li   t5, EARLY_NUM_GIB
    li   t3, EARLY_PHYS_BASE
    li   t6, EARLY_PTE_FLAGS
1:
    // 0x8000_0000 = [2] * 2^30
    // 16G RAM 对应索引范围 [2,17)，即循环 16 次
    // 高半地址空间需要加上 KERNEL_WINDOW_BASE = [256] * 2^30

    // 构造 PTE
    srli t0, t3, 2  // 将目标物理地址与 PTE 的要求对齐
    or   t0, t0, t6 // 设置 PTE 标志
    srli t1, t3, 30 // 由地址计算页表索引
    slli t1, t1, 3  // 从页表索引得到字节偏移
    add  t1, t4, t1 // 页表基址 + 偏移 = 页表项地址
    sd   t0, 0(t1)  // 写入页表项
    li   t2, 2048
    add  t1, t1, t2 // + 256 * 8 得到高半窗口映射
    sd   t0, 0(t1)  // 写入页表项

    // 跳到下一个 1GB 循环
    li   t2, 0x40000000
    add  t3, t3, t2
    addi t5, t5, -1
    bnez t5, 1b // 执行 16 次后 16 被减为0
                // b 表示向后搜索最近的标签

    li   t2, BOOT_WINDOW

    // 设置 satp 寄存器，开启 Sv39 分页
    la   t0, early_pg_dir
    srli t0, t0, 12 // 根表 PPN
    li   t1, 0x8000000000000000 // satp.MODE = SV39
    or   t0, t0, t1
    csrw satp, t0
    sfence.vma

    // 进入高半链接地址
    la   t0, 3f // f 表示向前搜索最近的标签
    add  t0, t0, t2 // 添加窗口
    jr   t0 // 跳转进入高半地址空间
3:
    slli t0, tp, BOOT_STACK_SHIFT
    la   sp, boot_stack_top
    sub  sp, sp, t0
    call rust_main // rust_main 符号为带窗口的虚拟地址

4:  j    4b // 无限循环，防止 rust_main 返回

    // 将早期页表放在 .data 段
    .section .data.early_pg, "aw", @progbits
    .balign 4096
    .globl early_pg_dir
early_pg_dir:
    .space 4096

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space (1 << BOOT_STACK_SHIFT) * BOOT_HARTS
    .globl boot_stack_top
boot_stack_top:
