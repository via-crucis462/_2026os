use crate::mm::PageTableEntry;
use crate::arch::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use core::fmt::{self, Debug, Formatter};

const PA_WIDTH_LA64: usize = 56;
const VA_WIDTH_LA64: usize = 39;
const PPN_WIDTH_LA64: usize = PA_WIDTH_LA64 - PAGE_SIZE_BITS;
const VPN_WIDTH_LA64: usize = VA_WIDTH_LA64 - PAGE_SIZE_BITS;

// 为使虚拟地址结构与SV39一致，定义如下常量
// 这些参数需要在内存初始化时写进寄存器
const PA_LEN : usize = PA_WIDTH_LA64;
const VA_LEN : usize = VA_WIDTH_LA64;
// PT可以理解为dir0
const PT_BASE : usize = PAGE_SIZE_BITS;//页大小4K对应12位
const PT_WIDTH: usize = 9;
const DIR1_BASE: usize = PT_BASE + PT_WIDTH;
const DIR1_WIDTH: usize = 9;
const DIR2_BASE: usize = DIR1_BASE + DIR1_WIDTH;
const DIR2_WIDTH: usize = 9;

// PTE Size: 0 for 8 bytes
const PTE_WIDTH_VAL: usize = 0; 

// 定义虚拟内存的低位布局，设置0，1,2页表
pub const PWCL_VAL: usize = (PT_BASE << 0) |
                            (PT_WIDTH << 5) |
                            (DIR1_BASE << 10) |
                            (DIR1_WIDTH << 15) |
                            (DIR2_BASE << 20) |
                            (DIR2_WIDTH << 25) |
                            (PTE_WIDTH_VAL << 30);

// 弃用3，4级页表，给高部分寄存器置零
pub const PWCH_VAL: usize = 0; 

