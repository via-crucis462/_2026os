//! la64的内存管理模块
//! 处理龙芯与riscv内存管理的差异部分
//! 尚不完善

pub mod address;
pub mod page_table;

use crate::arch::config::{PAGE_SIZE, PAGE_SIZE_BITS};

const PA_WIDTH_SV39: usize = 56;
const VA_WIDTH_SV39: usize = 39;

// 为使虚拟地址结构与SV39一致，定义如下常量
// 这些参数需要在内存初始化时写进寄存器
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

/// 启动时内存有关寄存器初始化，后续完善后在init()中调用
/// token: 根页表物理地址
pub fn la64_init_mem(token: usize) {
    unsafe {
        // 设置页表项宽度等参数
        asm!("mtcr pwcl, {}", in(reg) PWCL_VAL);
        asm!("mtcr pwch, {}", in(reg) PWCH_VAL);
        // 设置PGD寄存器，指向根页表
        asm!("mtcr pgdl, {}", in(reg) token);
        asm!("mtcr pgdh, 0");
    }
}

/// TLB重填，未完成
pub fn do_tlb_refill(va: VirtAddr) {
    // TODO
}

// TLB重填异常处理
pub fn tlb_refill_handler() {
    // TODO
}