//! la64的页表项定义

use crate::arch::config::*;
use crate::arch::mm::*;

use crate::mm::{PhysPageNum, PTEFlags};

bitflags!{
    /// page table entry flags
    /// LA64标准
    pub struct PTEFlagsLA64: u64 {
        const V = 1 << 0;
        const D = 1 << 1;
        const PLV0 = 1 << 2;
        const PLV1 = 1 << 3;
        const MAT0 =  1 << 4;
        const MAT1 =  1 << 5;
        const G =  1 << 6;
        const P =  1 << 7;
        const W =  1 << 8;
        const NR = 1 << 61;
        const NX = 1 << 62;
        const RPLV = 1 << 63;
    }
}

impl PTEFlagsLA64{
    fn default() -> Self {
        PTEFlagsLA64::V | PTEFlagsLA64::MAT0 | PTEFlagsLA64::P
    }
}

pub fn from_riscv_flags(riscv_flags: PTEFlags) -> PTEFlagsLA64 {
    // 默认设置
    let mut la64_flags = PTEFlagsLA64::V | PTEFlagsLA64::MAT0 | PTEFlagsLA64::P;
    if (riscv_flags & PTEFlags::V) != PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::V;
    }
    if (riscv_flags & PTEFlags::D) != PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::D;
    }
    if (riscv_flags & PTEFlags::G) != PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::G;
    }
    if (riscv_flags & PTEFlags::R) == PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::NR;
    }
    if (riscv_flags & PTEFlags::W) != PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::W;
        la64_flags |= PTEFlagsLA64::D;
    }
    if (riscv_flags & PTEFlags::X) == PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::NX;
    }
    if (riscv_flags & PTEFlags::U) != PTEFlags::empty() {
        la64_flags |= PTEFlagsLA64::PLV0;
        la64_flags |= PTEFlagsLA64::PLV1;
    }
    la64_flags
}

#[derive(Copy, Clone)]
#[repr(C)]
/// 对于LA64，目录项和页表项格式类似，但目录项无权限位，需要注意
pub struct PageTableEntry {
    /// bits of page table entry
    pub bits: usize,
}

// 按LA64标准作部分修改
impl PageTableEntry {
    /// Create a new page table entry
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        let bits = ppn.0 << PAGE_SIZE_BITS;
        let la64_flags = from_riscv_flags(flags);
        PageTableEntry { bits: bits | la64_flags.bits as usize }
    }
    // la64目录项不含权限位
    pub fn new_dir(ppn: PhysPageNum) -> Self {
        let bits = ppn.0 << PAGE_SIZE_BITS;
        PageTableEntry {bits: bits}
    }
    pub fn new_defualt(ppn: PhysPageNum) -> Self {
        let bits = ppn.0 << PAGE_SIZE_BITS;
        let la64_flags = PTEFlagsLA64::default();
        PageTableEntry { bits: bits | la64_flags.bits as usize }
    }
    /// Create an empty page table entry
    pub fn empty() -> Self {
        PageTableEntry { bits: 0 }
    }
    /// Get the physical page number from the page table entry
    pub fn ppn(&self) -> PhysPageNum {
        // 设置12字节偏移
        (self.bits >> PAGE_SIZE_BITS & ((1usize << (PA_WIDTH - PAGE_SIZE_BITS)) - 1)).into()
    }
    /// Get the flags from the page table entry
    pub fn flags(&self) -> PTEFlagsLA64 {
        PTEFlagsLA64::from_bits_truncate(self.bits as u64)//修改为只截取flags部分
    }
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }
    /// The page pointered by page table entry is valid?
    pub fn is_valid(&self) -> bool {
        (self.flags() & PTEFlagsLA64::V) != PTEFlagsLA64::empty()
    }
    /// The page pointered by page table entry is readable?
    pub fn readable(&self) -> bool {
        (self.flags() & PTEFlagsLA64::NR) == PTEFlagsLA64::empty()
    }
    /// The page pointered by page table entry is writable?
    pub fn writable(&self) -> bool {
        (self.flags() & PTEFlagsLA64::W) != PTEFlagsLA64::empty()
    }
    /// The page pointered by page table entry is executable?
    pub fn executable(&self) -> bool {
        (self.flags() & PTEFlagsLA64::NX) == PTEFlagsLA64::empty()
    }
    //设置脏位
    pub fn set_dirty(&mut self) {
        self.bits |= PTEFlagsLA64::D.bits() as usize;
    }
}

