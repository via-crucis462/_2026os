use super::{frame_alloc, FrameTracker, PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum, PTEFlags, pte::*};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use crate::arch::config::PAGE_SIZE;
use crate::arch::trap::{current_trap_cx_user_va, TrapContext};
use crate::process::{current_task, current_user_token};
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
        if (flags & PTEFlags::W) != PTEFlags::empty() {
            pte.set_dirty();
        }
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
    pub fn set_flags(&mut self, vpn: VirtPageNum, flags: PTEFlags) {
        let ppn = self.translate(vpn).unwrap().ppn();
        self.set_entry(vpn, ppn, flags);
    }
    #[cfg(target_arch = "riscv64")]
    pub fn set_entry(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte(vpn).unwrap();
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_entry(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte(vpn).unwrap();
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
        if (flags & PTEFlags::W) != PTEFlags::empty() {
            pte.set_dirty();
        }
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
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() => pte.ppn(),
            _ => {
                if token != current_user_token() {
                    return Vec::new();
                }
                let task = current_task().unwrap();
                let process = task.process();
                let trap_cx_va = current_trap_cx_user_va();
                let Some(trap_cx_pa) = page_table.translate_va(VirtAddr::from(trap_cx_va)) else {
                    return Vec::new();
                };
                let sp = trap_cx_pa.get_ref::<TrapContext>().get_sp();
                let mut proc_inner = process.inner_exclusive_access();
                if proc_inner.memory_set.handle_page_fault(start, sp) {
                    page_table.translate(vpn).unwrap().ppn()
                } else {
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

fn prepare_user_read(token: usize, ptr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr;
    let end = start + len;
    let mut ready = true;
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() && pte.readable() => {}
            _ => {
                ready = false;
                break;
            }
        }
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        start = end_va.into();
    }
    if ready {
        return true;
    }
    if token != current_user_token() {
        return false;
    }
    let task = current_task().unwrap();
    let process = task.process();
    #[cfg(target_arch = "riscv64")]
    let sp = {
        let trap_cx_va = current_trap_cx_user_va();
        let Some(trap_cx_pa) = page_table.translate_va(VirtAddr::from(trap_cx_va)) else {
            return false;
        };
        trap_cx_pa.get_ref::<TrapContext>().get_sp()
    };
    #[cfg(target_arch = "loongarch64")]
    let sp = crate::task::current_trap_cx().get_sp();
    let mut proc_inner = process.inner_exclusive_access();
    proc_inner.memory_set.ensure_readable_user_range(ptr, len, sp)
}

fn prepare_user_write(token: usize, ptr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr;
    let end = start + len;
    let mut ready = true;
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() && pte.writable() => {}
            _ => {
                ready = false;
                break;
            }
        }
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        start = end_va.into();
    }
    if ready {
        return true;
    }
    if token != current_user_token() {
        println!("prepare_user_write: token mismatch, token = {:#x}, current_user_token = {:#x}", token, current_user_token());
        return false;
    }
    let task = current_task().unwrap();
    let process = task.process();
    #[cfg(target_arch = "riscv64")]
    let sp = {
        let trap_cx_va = current_trap_cx_user_va();
        let Some(trap_cx_pa) = page_table.translate_va(VirtAddr::from(trap_cx_va)) else {
            println!("prepare_user_write: failed to translate trap_cx_va {:#x}", trap_cx_va);
            return false;
        };
        trap_cx_pa.get_ref::<TrapContext>().get_sp()
    };
    #[cfg(target_arch = "loongarch64")]
    let sp = crate::task::current_trap_cx().get_sp();
    let mut proc_inner = process.inner_exclusive_access();
    proc_inner.memory_set.ensure_writable_user_range(ptr, len, sp)
}

pub fn translated_byte_buffer_mut(token: usize, ptr: *const u8, len: usize) -> Vec<&'static mut [u8]> {
    if !prepare_user_write(token, ptr as usize, len) {
        return Vec::new();
    }
    translated_byte_buffer(token, ptr, len)
}

/// Translate&Copy a ptr[u8] array end with `\0` to a `String` Vec through page table
pub fn translated_str(token: usize, ptr: *const u8) -> String {
    assert!(prepare_user_read(token, ptr as usize, 1), "translated_str: user ptr is not readable");
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        assert!(prepare_user_read(token, va, 1), "translated_str: user ptr is not readable");
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

/// +错误处理
pub fn try_translated_str(token: usize, ptr: *const u8) -> Option<String> {
    if !prepare_user_read(token, ptr as usize, 1) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        if !prepare_user_read(token, va, 1) {
            return None;
        }
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
    Some(string)
}

/// Translate a ptr[u8] array through page table and return a reference of T
pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let len = core::mem::size_of::<T>();
    assert!(prepare_user_read(token, ptr as usize, len), "translated_ref: user ptr is not readable");
    let page_table = PageTable::from_token(token);
    let pa = page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    debug!("translated_ref: start_pa = {:#x}, end_pa = {:#x}, len = {:#x}", pa.0, pa.0 + len - 1, len);
    if pa.floor() == PhysAddr(pa.0 + len - 1).floor() {
        pa.get_ref()
    } else {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(translated_read(token, ptr)))
    }
}

/// 从给定地址读取数据并返回T
pub fn translated_read<T>(token: usize, ptr: *const T) -> T {
    let len = core::mem::size_of::<T>();
    assert!(prepare_user_read(token, ptr as usize, len), "translated_read: user ptr is not readable");
    let page_table = PageTable::from_token(token);
    let mut data = vec![0u8; len];
    let start_va = VirtAddr::from(ptr as usize);
    // 页内快路径：保持原有低开销行为
    if start_va.page_offset() + len <= PAGE_SIZE {
        let pa = page_table
            .translate_va(start_va)
            .unwrap();
        let start = pa.0;
        let end = start + len;
        for (idx, addr) in (start..end).enumerate() {
            data[idx] = unsafe { *(addr as *const u8) };
        }
    } else {
        // 跨页路径：按虚拟地址逐字节翻译，避免假设物理地址连续
        for idx in 0..len {
            let va = VirtAddr::from((ptr as usize) + idx);
            let pa = page_table.translate_va(va).unwrap();
            data[idx] = unsafe { *(pa.0 as *const u8) };
        }
    }
    unsafe { core::ptr::read(data.as_ptr() as *const T) }
}

/// 将用户空间的T写入给定地址
pub fn translated_write<T>(token: usize, ptr: *mut T, value: T) {
    let len = core::mem::size_of::<T>();
    assert!(prepare_user_write(token, ptr as usize, len), "translated_write: user ptr is not writable");
    let page_table = PageTable::from_token(token);
    let data = unsafe { core::slice::from_raw_parts((&value as *const T) as *const u8, len) };
    let start_va = VirtAddr::from(ptr as usize);
    // 页内快路径：保持原有低开销行为
    if start_va.page_offset() + len <= PAGE_SIZE {
        let pa = page_table
            .translate_va(start_va)
            .unwrap();
        let start = pa.0;
        let end = start + len;
        for (idx, addr) in (start..end).enumerate() {
            unsafe { *(addr as *mut u8) = data[idx] };
        }
    } else {
        // 跨页路径：按虚拟地址逐字节翻译，避免假设物理地址连续
        for idx in 0..len {
            let va = VirtAddr::from((ptr as usize) + idx);
            let pa = page_table.translate_va(va).unwrap();
            unsafe { *(pa.0 as *mut u8) = data[idx] };
        }
    }
}
