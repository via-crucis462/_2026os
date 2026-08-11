//! la64的内存管理模块
//! 处理龙芯与riscv内存管理的差异部分
//! 尚不完善
pub mod pte;
pub mod info;
pub mod tlb;

use crate::arch::config::*;
use core::arch::asm;
use core::sync::atomic::AtomicUsize;

/// 诊断用：记录 CSR.MISC 写入前后的值，验证 ALCL 位是否可写
pub static MISC_BEFORE_WRITE: AtomicUsize = AtomicUsize::new(0);
pub static MISC_AFTER_WRITE: AtomicUsize = AtomicUsize::new(0);

// 摘自手册：当CSR.CRMD的DA=0且PG=1时，处理器核的MMU处于映射地址翻译模式。具体又分为直接映射
// 地址翻译模式（简称“直接映射模式”）和页表映射地址翻译模式（简称“页表映射模式”）两种。
// 0x1设置特权级plv0，0x10设置缓存开启
const DMW0_VAL: usize = UNCACHED_KERNEL_BASE | 0x1;
const DMW1_VAL: usize = CACHED_KERNEL_BASE | 0x11;//0b10001
//const DMW1_VAL: usize = KERNEL_BASE | 0x1; //暂时不启用缓存
const DMW2_VAL: usize = 0 | 0x1;
const DMW3_VAL: usize = 0;

// 本来是56,la64 qemu改为48(由cpucfg读取)
pub const PA_WIDTH: usize = 48;
pub const VA_WIDTH: usize = 39;

// 为使虚拟地址结构与SV39一致，定义如下常量
// qemu使用的物理地址只到48位
const PA_LEN : usize = PA_WIDTH;
const VA_LEN : usize = VA_WIDTH;
// PT可以理解为dir0
const PT_BASE : usize = PAGE_SIZE_BITS;//页大小4K对应12位
const PT_WIDTH: usize = 9;
const DIR1_BASE: usize = PT_BASE + PT_WIDTH;
const DIR1_WIDTH: usize = 9;
const DIR2_BASE: usize = DIR1_BASE + DIR1_WIDTH;
const DIR2_WIDTH: usize = 9;

const PTE_WIDTH: usize = 0; // 页表项位宽64

// 定义虚拟内存的低位布局，设置第0~2页表
const PWCL_VAL: usize = (PT_BASE << 0) |
                        (PT_WIDTH << 5) |
                        (DIR1_BASE << 10) |
                        (DIR1_WIDTH << 15) |
                        (DIR2_BASE << 20) |
                        (DIR2_WIDTH << 25) |
                        (PTE_WIDTH<< 30);

// 弃用3/4级页表，给控制高位部分的寄存器置零
const PWCH_VAL: usize = 0; 

/// 内核启动时设置映射窗口，特别地，屏蔽掉0开头的地址映射
pub fn la_kernel_init_mem() {
    // 设置直接映射配置窗口
    unsafe {
        asm!("csrwr {dmw0}, 0x180", dmw0 = inout(reg) DMW0_VAL => _);
        asm!("csrwr {dmw1}, 0x181", dmw1 = inout(reg) DMW1_VAL => _);
        asm!("csrwr {dmw2}, 0x182", dmw2 = inout(reg) DMW2_VAL => _);
        asm!("csrwr {dmw3}, 0x183", dmw3 = inout(reg) DMW3_VAL => _);
        let mut t: usize;
        asm!("csrrd {}, 0x0", out(reg) t);
        t |= 1 << 4;
        t &= !(1 << 3);
        // TLB 重填会进入 DA=1状态，此时页表 load 的缓存属性来自 DATM。
        // 设为可缓存，需要内核始终用缓存地址访问页表，否则会出现重填时缓存不一致
        t = (t & !(0b11 << 7)) | (1 << 7);
        asm!("csrwr {crmd}, 0x0", crmd = inout(reg) t => _);

        #[cfg(board = "2k1000")]
        {
            // 需要注意，2k1000的misc寄存器的ALCL位虽然是0，但实际上不支持非对齐访存
            // 下面还是保留设置
            let mut misc: usize;
            asm!("csrrd {}, 0x3", out(reg) misc);
            let misc_old = misc;
            misc &= !(0b1111 << 12); // 清除 ALCL0~ALCL3（位 12~15）
            asm!("csrwr {misc}, 0x3", misc = inout(reg) misc => _);
            // 回读验证：若 ALCL 位仍为 1，说明硬件不支持非对齐访存
            let mut misc_after: usize;
            asm!("csrrd {}, 0x3", out(reg) misc_after);
            // 将 misc_old/misc_after 存入全局变量供后续打印诊断
            crate::arch::la::mm::MISC_BEFORE_WRITE.store(misc_old, core::sync::atomic::Ordering::Relaxed);
            crate::arch::la::mm::MISC_AFTER_WRITE.store(misc_after, core::sync::atomic::Ordering::Relaxed);
        }
    }
    init_tlb();
}

/// 内存相关寄存器初始化，需要在启动应用时调用，尚未完善
fn init_tlb() {
    unsafe {
        asm!("csrwr {pwcl}, 0x1c", pwcl = inout(reg) PWCL_VAL => _); // PWCL
        asm!("csrwr {pwch}, 0x1d", pwch = inout(reg) PWCH_VAL => _); // PWCH
        // 写入页大小
        asm!("csrwr {pgsz}, 0x1e", pgsz = inout(reg) PAGE_SIZE_BITS => _); // STLBPS
        asm!(
            "csrwr {tlbrfl}, 0x88",
            tlbrfl = inout(reg) (tlb_refill_handler as *const() as usize) => _
        );
        // 清空TLB
        asm!("invtlb 0, $r0, $r0");
    }
}

/// Configure the refill page size after early driver initialization and after
/// invalidating stale entries, immediately before entering user space.
pub fn prepare_user_tlb() {
    let mut tlbrehi: usize;
    unsafe {
        asm!("csrrd {value}, 0x8e", value = out(reg) tlbrehi);
        tlbrehi = (tlbrehi & !0x3f) | PAGE_SIZE_BITS;
        asm!("csrwr {value}, 0x8e", value = inout(reg) tlbrehi => _);
    }
}

pub fn flush_tlb_for_asid(asid: usize) {
    unsafe {
        asm!("dbar 0");
        // op 0x4 invalidates non-global entries matching the supplied ASID.
        asm!("invtlb 0x4, {asid}, $r0", asid = in(reg) asid);
        asm!("dbar 0");
    }
}

/// 刷新本核所有非全局 TLB 项
pub fn flush_user_tlb() {
    unsafe {
        asm!("dbar 0");
        asm!("invtlb 0x3, $r0, $r0");
        asm!("dbar 0");
    }
}

// 修改根页表地址
/*
pub fn la_app_init_mem(token: usize) {
    unsafe {
        // 设置PGDL/PGDH，供TLB重填时加载页表根地址
        asm!("csrwr {pgdl}, 0x19", pgdl = inout(reg) token => _); // PGDL
        asm!("csrwr {pgdh}, 0x1a", pgdh = inout(reg) 0usize => _); // PGDH
    }
}
*/

use core::arch::global_asm;
global_asm!(include_str!("refill.S"));

extern  "C" {
    pub fn tlb_refill_handler();
}
