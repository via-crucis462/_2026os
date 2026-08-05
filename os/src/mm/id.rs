//! 地址空间标识符(ASID)分配器
//! 为每个地址空间分配一个唯一的ASID
//! 主要用于la64的TLB管理

use crate::process::id::*;
use lazy_static::*;
use crate::sync::MPSafeCell;
#[allow(unused)]
use core::arch::asm;

lazy_static! {
    pub static ref ASID_ALLOCATOR: MPSafeCell<RecycleAllocator> =
        MPSafeCell::new(RecycleAllocator::new());
}

pub struct ASIDHandle(pub usize);

pub fn asid_alloc() -> ASIDHandle {
    ASIDHandle(ASID_ALLOCATOR.exclusive_access().alloc())
}

impl Drop for ASIDHandle {
    fn drop(&mut self) {
        info!("drop asid {}", self.0);
        // RISC-V 的 sfence.vma 只影响执行它的 hart。当前内核尚未实现
        // 跨 hart TLB shootdown，复用 ASID 会让迁移到其他 hart 的新进程
        // 命中旧地址空间的翻译。因此 RISC-V 在本次启动期间不回收 ASID。
        #[cfg(target_arch = "riscv64")]
        return;

        #[cfg(target_arch = "loongarch64")]
        unsafe{
            asm!(
                // 回收时清空对应tlb表项
                "invtlb 0x4, {}, $r0",
                in(reg) self.0
            );
        }

        #[cfg(target_arch = "loongarch64")]
        ASID_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}
