//! la64的内存管理模块
//! 处理龙芯与riscv内存管理的差异部分
//! 尚不完善
pub mod pte;

use crate::mm::address::*;
use crate::arch::config::*;
use core::arch::asm;

// 摘自手册：当CSR.CRMD的DA=0且PG=1时，处理器核的MMU处于映射地址翻译模式。具体又分为直接映射
// 地址翻译模式（简称“直接映射模式”）和页表映射地址翻译模式（简称“页表映射模式”）两种。
// 0x1设置特权级plv0，0x10设置缓存开启
const DMW0_VAL: usize = UNCHACHED_KERNEL_BASE | 0x1;
const DMW1_VAL: usize = KERNEL_BASE | 0x11;
const DMW2_VAL: usize = 0 | 0x1;

// 本来是56,la64 qemu改为48
pub const PA_WIDTH_SV39: usize = 48;
pub const VA_WIDTH_SV39: usize = 39;

// 为使虚拟地址结构与SV39一致，定义如下常量
// qemu使用的物理地址只到48位
const PA_LEN : usize = PA_WIDTH_SV39;
const VA_LEN : usize = VA_WIDTH_SV39;
// PT可以理解为dir0
const PT_BASE : usize = PAGE_SIZE_BITS;//页大小4K对应12位
const PT_WIDTH: usize = 9;
const DIR1_BASE: usize = PT_BASE + PT_WIDTH;
const DIR1_WIDTH: usize = 9;
const DIR2_BASE: usize = DIR1_BASE + DIR1_WIDTH;
const DIR2_WIDTH: usize = 9;

const PTE_WIDTH_VAL: usize = 0;// 64位宽页表项对应0 

// 定义虚拟内存的低位布局，设置第0~2页表
const PWCL_VAL: usize = (PT_BASE << 0) |
                        (PT_WIDTH << 5) |
                        (DIR1_BASE << 10) |
                        (DIR1_WIDTH << 15) |
                        (DIR2_BASE << 20) |
                        (DIR2_WIDTH << 25) |
                        (PTE_WIDTH_VAL << 30);

// 弃用3/4级页表，给控制高位部分的寄存器置零
const PWCH_VAL: usize = 0; 

/// 内核启动时设置映射窗口，特别地，屏蔽掉0开头的地址映射
pub fn la_kernel_init_mem() {
    // 设置直接映射配置窗口
    unsafe {
        asm!("csrwr {}, 0x180", in(reg) DMW0_VAL);
        asm!("csrwr {}, 0x181", in(reg) DMW1_VAL);
        asm!("csrwr {}, 0x182", in(reg) DMW2_VAL);
        let mut t: usize;
        asm!("csrrd {}, 0x0", out(reg) t);
        t |= 1 << 4;
        t &= !(1 << 3);
        asm!("csrwr {}, 0x0", in(reg) t);
    }
}

/// 内存相关寄存器初始化，需要在启动应用时调用，尚未完善
/// token: 当前内存空间根页表物理地址
pub fn la_app_init_mem(token: usize) {
    unsafe {
        // 设置页表项宽度等参数
        asm!("csrwr {}, 0x1c", in(reg) PWCL_VAL); // PWCL
        asm!("csrwr {}, 0x1d", in(reg) PWCH_VAL); // PWCH
        // 设置PGD寄存器保存根页表物理地址
        asm!("csrwr {}, 0x19", in(reg) token);// PGDL 低半地址空间，对应用户态
        asm!("csrwr {}, 0x1a", in(reg) token);// PGDH 临时也指向用户页表，保证内核态访问trap_ctx生效
        // asm!("csrwr {}, 0x1a", in(reg) 0);
        // 设置TLB重填处理函数地址
        asm!("csrwr {}, 0x88", in(reg) tlb_refill_handler as *const() as usize); // TLBRENTRY
    }
}

/// TLB重填软件逻辑，相比硬件处理效率较低，暂不实现
#[allow(unused)]
pub fn do_tlb_refill(_va: VirtAddr) {
    // TODO
}

/// TLB重填异常处理
#[no_mangle]
pub fn tlb_refill_handler() {
    // 硬件会自动保存异常虚拟地址到TLBRBADV
    unsafe {
        asm!(
            // 临时保存 t0 寄存器，否则会被覆盖
            "csrwr $t0, 0x8B",
            // 加载根页表（dir2）地址
            // 默认均为用户态发生缺页，从PGDL加载
            "csrrd $t0, 0x19",
            // 摘自手册：
            // “LDDIR、LDPTE指令执行所需的出错虚地址信息
            // 将来自于CSR.TLBRBADV”
            // 根据触发异常的va逐级遍历dir2,dir1,pt
            "lddir $t0, $t0, 2",
            "lddir $t0, $t0, 1",
            // la64“双页”，奇偶分别处理
            "ldpte $t0, 0",
            "ldpte $t0, 1",
            // 执行重填并返回
            "tlbfill",
            "csrrd $t0, 0x8B",
            "ertn",
        );
    }
}