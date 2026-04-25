use super::{frame_alloc, FrameTracker, PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum, PTEFlags, pte::*};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use crate::arch::config::PAGE_SIZE;
use crate::process::current_task;
#[allow(unused)]


/// page table structure
pub struct PageTable {
    // LA64也要求PAGE_SIZE对齐   
    root_ppn: PhysPageNum,
    // 这里用同样的结构体记录分配的页帧
    frames: Vec<FrameTracker>,
}

/// Assume that it won't oom when creating/mapping.
impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        let frame = frame_alloc().unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    /// Temporarily used to get arguments from user space.
    /// LA64根页表地址存储在CSR.PGDL或H，
    /// 这里存储的是2级页表的物理地址，因为弃用了3，4级页表
    /// 参考rv64的rcore理解即可
    pub fn from_token(token: usize) -> Self {
        Self {
            #[cfg(target_arch = "riscv64")]
            root_ppn: PhysPageNum::from(token & ((1usize << 44) - 1)),
            #[cfg(target_arch = "loongarch64")]
            root_ppn: PhysAddr(token).floor(),
            frames: Vec::new(),
        }
    }
    /// Find PageTableEntry by VirtPageNum, create a frame for a 4KB page table if not exist
    #[cfg(target_arch = "riscv64")]
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {  
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {

                let frame = frame_alloc().unwrap();
                *pte = PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }
    #[cfg(target_arch = "loongarch64")]
    // 参考了loongarch rocre
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {  
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if pte.is_empty() {
                let frame = frame_alloc().unwrap();
                *pte = PageTableEntry::new_dir(frame.ppn);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }

    /// Find PageTableEntry by VirtPageNum
    #[cfg(target_arch = "riscv64")]
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            //println!("find_pte: vpn = {:?}, i = {}", vpn, i);
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        result
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            //println!("find_pte: vpn = {:?}, i = {}", vpn, i);
            if pte.is_empty() {
                //println!("find_pte: vpn = {:?}, i = {}, pte is empty", vpn, i);
                return None;
            }
            if i == 2 {
                result = Some(pte);
                break;
            }
            ppn = pte.ppn();
        }
        result
    }
    /// set the map between virtual page number and physical page number
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(!pte.is_valid(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
    }
    #[allow(unused)]
    #[cfg(target_arch = "loongarch64")]
    
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(pte.is_empty(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new_defualt(ppn);
        *pte = PageTableEntry { bits: pte.bits | from_riscv_flags(flags).bits() as usize};
        // 直接设置为脏，后续可能需要修改
        pte.set_dirty();
    }
    /// remove the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        let pte = self.find_pte(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }
    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|pte| *pte)
    }
    pub fn translate_create(&mut self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte_create(vpn).map(|pte| *pte)
    }
    /// get the physical address from the virtual address
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.find_pte(va.clone().floor()).map(|pte| {
            let aligned_pa: PhysAddr = pte.ppn().into();
            let offset = va.page_offset();
            let aligned_pa_usize: usize = aligned_pa.into();
            (aligned_pa_usize + offset).into()
        })
    }
    /// get the token from the page table
    #[cfg(target_arch = "riscv64")]
    pub fn token(&self, asid: usize) -> usize {
        8usize << 60 | ((asid & 0xffff) << 44) | self.root_ppn.0
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn token(&self) -> usize {
        PhysAddr::from(self.root_ppn).into()
    }
}

/// Translate&Copy a ptr[u8] array with LENGTH len to a mutable u8 Vec through page table
/// 其中ptr是用户空间地址
pub fn translated_byte_buffer(token: usize, ptr: *const u8, len: usize) -> Vec<&'static mut [u8]> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    let task = current_task().unwrap();
    let process = task.process();
    let mut inner = task.inner_exclusive_access();
    let sp = inner.get_trap_cx().get_sp();
    let mut proc_inner = process.inner_exclusive_access();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() => pte.ppn(),
            _ => {
                // 尝试用你的 handle_page_fault 修复它（比如触发 Lazy Allocation）
                // 注意：这里需要传入当前的 sp 供栈扩张逻辑使用
               if proc_inner.memory_set.handle_page_fault(start, sp) {
                    page_table.translate(vpn).unwrap().ppn()
                } else {
                    // 真的越界了，返回空 Vec，上层会返回 -EFAULT
                    return Vec::new();
                }
            }
        };
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    v
}

/// Translate&Copy a ptr[u8] array end with `\0` to a `String` Vec through page table
pub fn translated_str(token: usize, ptr: *const u8) -> String {
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        let ch: u8 = *(page_table
            .translate_va(VirtAddr::from(va))
            .unwrap()
            .get_mut());
        if ch == 0 {
            break;
        }
        string.push(ch as char);
        va += 1;
    }
    string
}

/// Translate a ptr[u8] array through page table and return a reference of T
pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let len = core::mem::size_of::<T>();
    let page_table = PageTable::from_token(token);
    let pa = page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    debug!("translated_ref: start_pa = {:#x}, end_pa = {:#x}, len = {:#x}", pa.0, pa.0 + len - 1, len);
    // 确保访问的物理地址范围内没有跨页
    assert!(pa.floor() == PhysAddr(pa.0 + len - 1).floor(), "translated_refmut: access crosses page boundary");
    pa.get_ref()
}

/// 从给定地址读取数据并返回T
pub fn translated_read<T>(token: usize, ptr: *const T) -> T {
    let len = core::mem::size_of::<T>();
    let page_table = PageTable::from_token(token);
    let pa = page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    let start = pa.0;
    let end = start + len;
    let mut data = vec![0u8; len];
    for (idx, addr) in (start..end).enumerate() {
        data[idx] = unsafe { *(addr as *const u8) };
    }
    unsafe { core::ptr::read(data.as_ptr() as *const T) }
}

/// 将用户空间的T写入给定地址
pub fn translated_write<T>(token: usize, ptr: *mut T, value: T) {
    let len = core::mem::size_of::<T>();
    let page_table = PageTable::from_token(token);
    let pa = page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    let start = pa.0;
    let end = start + len;
    let data = unsafe { core::slice::from_raw_parts((&value as *const T) as *const u8, len) };
    for (idx, addr) in (start..end).enumerate() {
        unsafe { *(addr as *mut u8) = data[idx] };
    }
}

/// Translate a ptr[u8] array through page table and return a mutable reference of T
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> &'static mut T {
    let len = core::mem::size_of::<T>();
    let page_table = PageTable::from_token(token);
    let pa = 
    page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    //debug!("translated_refmut: start_pa = {:#x}, end_pa = {:#x}, len = {:#x}", pa.0, pa.0 + len - 1, len);
    // 确保访问的物理地址范围内没有跨页
    assert!(pa.floor() == PhysAddr(pa.0 + len - 1).floor(), "translated_refmut: access crosses page boundary");

    pa.get_mut()
}
