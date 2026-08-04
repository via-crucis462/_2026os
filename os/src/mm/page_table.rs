use super::{
    frame_alloc, pte::*, FrameTracker, PTEFlags, PhysAddr, PhysPageNum, StepByOne, VirtAddr,
    VirtPageNum,
};
use crate::arch::config::PAGE_SIZE;
use crate::mm::MapArea;
use crate::process::{current_task, current_user_token};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// 页大小，单位Bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSize {
    Page4K = 1 << 12,           // 4KB
    Page2M = 1 << (12 + 9),     // 2MB
    Page1G = 1 << (12 + 9 + 9), // 1GB
}

impl PageSize {
    // 页大小对应的字节数
    pub fn size(&self) -> usize {
        self.clone() as usize
    }
    // 页大小对应的位数，如4KB对应12位
    pub fn size_bits(&self) -> usize {
        self.size().trailing_zeros() as usize
    }
    // 对应基本页的倍数
    pub fn num_pages(&self) -> usize {
        self.size() / PAGE_SIZE
    }
    // 页表层数，0表示根页表项直接映射
    pub fn walk_level(&self) -> usize {
        match self {
            PageSize::Page4K => 2,
            PageSize::Page2M => 1,
            PageSize::Page1G => 0,
        }
    }
}

/// 页表结构体
///
/// rv 下所有页表的根页表的 256..512 项会复制内核初始化阶段创建的内容。
/// 用户不能修改这些项，内核在第一个用户进程创建后也不应再修改会改动自身根页表的内容。
pub struct PageTable {
    // LA64也要求PAGE_SIZE对齐
    root_ppn: PhysPageNum,
    /// 页表节点的所有权，析构时会自动释放
    /// 用户不应把内核页表的节点插入
    frames: Vec<FrameTracker>,
}

/// Assume that it won't oom when creating/mapping.
impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        let frame = frame_alloc(PageSize::Page4K).unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    /// 从现有页表创建一个新的页表，复制内核高半的根项
    pub fn alias_of(other: &PageTable) -> Self {
        Self {
            root_ppn: other.root_ppn,
            frames: Vec::new(),
        }
    }
    /// 从token创建一个新的页表
    ///
    /// LA64根页表地址存储在CSR.PGDL或H，
    /// 这里存储的是2级页表的物理地址，因为弃用了3，4级页表
    /// 参考rv64的rcore理解即可
    pub fn from_token(token: usize) -> Self {
        Self {
            #[cfg(target_arch = "riscv64")]
            root_ppn: PhysPageNum::from(token & ((1usize << 44) - 1)),
            #[cfg(target_arch = "loongarch64")]
            root_ppn: PhysAddr(token).std_floor(),
            frames: Vec::new(),
        }
    }
    /// 复制内核根页表的高半根项到当前页表。
    ///
    /// 根项指向的下级表仍由内核页表持有；用户页表只拥有自己的根表
    /// 与低半下级表，因此析构时不会回收这些共享页表。内核栈在根项 511
    /// 下更新，所有用户页表会立即看到同一份映射。
    #[cfg(target_arch = "riscv64")]
    pub fn share_kernel_half(&mut self, kernel: &PageTable) {
        const KERNEL_ROOT_START: usize = 256;

        let mut entries = [PageTableEntry::empty(); KERNEL_ROOT_START];
        entries.copy_from_slice(&kernel.root_ppn.get_pte_array()[KERNEL_ROOT_START..]);
        self.root_ppn.get_pte_array()[KERNEL_ROOT_START..].copy_from_slice(&entries);
    }
    /// Find PageTableEntry by VirtPageNum, create a frame for a 4KB page table if not exist
    #[cfg(target_arch = "riscv64")]
    fn find_pte_create(
        &mut self,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == page_size.walk_level() {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                let frame = frame_alloc(PageSize::Page4K).unwrap();
                *pte = PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }
    #[cfg(target_arch = "loongarch64")]
    // 参考了loongarch rocre
    fn find_pte_create(
        &mut self,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == page_size.walk_level() {
                if i != 2 {
                    // 页表提前结束，设置大页
                    pte.set_huge_page();
                }
                result = Some(pte);
                break;
            }
            if pte.is_empty() {
                let frame = frame_alloc(PageSize::Page4K).unwrap();
                *pte = PageTableEntry::new_dir(frame.ppn);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }

    /// Find PageTableEntry by VirtPageNum
    #[cfg(target_arch = "riscv64")]
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<(&mut PageTableEntry, PageSize)> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut pte_opt: Option<&mut PageTableEntry> = None;
        let mut page_size: Option<PageSize> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            //println!("find_pte: vpn = {:?}, i = {}", vpn, i);
            // 标准页叶子节点需要v，大页叶子节点有rwx任一即可
            if (i == 2 && pte.is_valid())
                || (i < 2 && (pte.readable() || pte.writable() || pte.executable()))
            {
                pte_opt = Some(pte);
                page_size = Some(match i {
                    0 => PageSize::Page1G,
                    1 => PageSize::Page2M,
                    2 => PageSize::Page4K,
                    _ => unreachable!(),
                });
                return Some((pte_opt.unwrap(), page_size.unwrap()));
            }
            // 如果不是叶子节点但无效，说明没有映射
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<(&mut PageTableEntry, PageSize)> {
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
            if i == 2 || (pte.is_huge_page()) {
                return Some((
                    pte,
                    match i {
                        0 => PageSize::Page1G,
                        1 => PageSize::Page2M,
                        2 => PageSize::Page4K,
                        _ => unreachable!(),
                    },
                ));
            }
            ppn = pte.ppn();
        }
        None
    }
    /// set the map between virtual page number and physical page number
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]

    pub fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: PTEFlags,
        page_size: PageSize,
    ) {
        let pte = self.find_pte_create(vpn, page_size).unwrap();
        assert!(!pte.is_valid(), "vpn {:?} is mapped before mapping", vpn);

        let mut final_flags = flags | PTEFlags::V | PTEFlags::A;

        // Supervisor mappings must not depend on the user trap handler to set D.
        if flags.contains(PTEFlags::W) && !flags.contains(PTEFlags::U) {
            final_flags |= PTEFlags::D;
        }

        *pte = PageTableEntry::new(ppn, final_flags);
    }
    #[allow(unused)]
    #[cfg(target_arch = "loongarch64")]
    pub fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: PTEFlags,
        page_size: PageSize,
    ) {
        let pte = self.find_pte_create(vpn, page_size).unwrap();
        assert!(pte.is_empty(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new_defualt(ppn);
        *pte = PageTableEntry {
            bits: pte.bits | from_riscv_flags(flags).bits() as usize,
        };
        if (flags & PTEFlags::W) != PTEFlags::empty() {
            // pte.set_dirty(); // 现在改为写入才在handler里设置
        }
    }
    /// remove the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        let (pte, page_size) = self.find_pte(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }
    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|(pte, size)| *pte)
    }
    pub fn translate_and_get_size(&self, vpn: VirtPageNum) -> Option<(PageTableEntry, PageSize)> {
        self.find_pte(vpn).map(|(pte, size)| (*pte, size))
    }
    pub fn translate_create(
        &mut self,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> Option<PageTableEntry> {
        self.find_pte_create(vpn, page_size).map(|pte| *pte)
    }
    pub fn set_flags(&mut self, vpn: VirtPageNum, flags: PTEFlags, page_size: PageSize) {
        let ppn = self.translate(vpn).unwrap().ppn();
        self.set_entry(vpn, ppn, flags);
    }
    #[cfg(target_arch = "riscv64")]
    pub fn set_entry(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let (pte, _size) = self.find_pte(vpn).unwrap();
        let mut final_flags = flags | PTEFlags::V | PTEFlags::A;
        if flags.contains(PTEFlags::W) && !flags.contains(PTEFlags::U) {
            final_flags |= PTEFlags::D;
        }
        *pte = PageTableEntry::new(ppn, final_flags);
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_entry(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let (pte, size) = self.find_pte(vpn).unwrap();
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
        if (flags & PTEFlags::W) != PTEFlags::empty() {
            pte.set_dirty();
        }
    }
    /// get the physical address from the virtual address
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.find_pte(va.clone().std_floor()).map(|(pte, size)| {
            let aligned_pa: PhysAddr = pte.ppn().into();
            assert!(
                aligned_pa.actual_aligned(size),
                "translate_va: pa {:#x} is not aligned to page size {:#x}",
                aligned_pa.0,
                size.size()
            );
            let offset = va.actual_page_offset(size);
            // PhysAddr 约定存放真实物理地址；需要访存时由调用方自行
            // get_cached_addr()/get_uncached_addr()
            (aligned_pa.0 + offset).into()
        })
    }
    /// get the token from the page table
    #[cfg(target_arch = "riscv64")]
    pub fn token(&self, asid: usize) -> usize {
        8usize << 60 | ((asid & 0xffff) << 44) | self.root_ppn.0
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn token(&self) -> usize {
        PhysAddr::from(self.root_ppn).0
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
        let mut vpn = start_va.std_floor();
        let (ppn, size) = match page_table.translate_and_get_size(vpn) {
            Some((pte, size)) if pte.is_valid() => (pte.ppn(), size),
            _ => {
                if token != current_user_token() {
                    return Vec::new();
                }
                let task = current_task().unwrap();
                let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
                    return Vec::new();
                };
                let sp = crate::task::current_trap_cx().get_sp();
                let mut memory = mm.exclusive_access();
                if memory.handle_page_fault(start, sp) {
                    let (pte, size) = page_table.find_pte(vpn).unwrap();
                    (pte.ppn(), size)
                } else {
                    return Vec::new();
                }
            }
        };
        vpn.step_by(size.num_pages());
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.actual_page_offset(size) == 0 {
            v.push(&mut ppn.get_bytes_array_with_size(size)[start_va.actual_page_offset(size)..]);
        } else {
            v.push(
                &mut ppn.get_bytes_array_with_size(size)
                    [start_va.actual_page_offset(size)..end_va.actual_page_offset(size)],
            );
        }
        start = end_va.into();
    }
    v
}

pub fn try_translated_byte_buffer(
    token: usize,
    ptr: *const u8,
    len: usize,
) -> Option<Vec<&'static mut [u8]>> {
    if !prepare_user_read(token, ptr as usize, len) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.std_floor();
        let (ppn, size) = match page_table.translate_and_get_size(vpn) {
            Some((pte, size)) if pte.is_valid() => (pte.ppn(), size),
            _ => return None,
        };
        vpn.step_by(size.num_pages());
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.actual_page_offset(size) == 0 {
            v.push(&mut ppn.get_bytes_array_with_size(size)[start_va.actual_page_offset(size)..]);
        } else {
            v.push(
                &mut ppn.get_bytes_array_with_size(size)
                    [start_va.actual_page_offset(size)..end_va.actual_page_offset(size)],
            );
        }
        start = end_va.into();
    }
    Some(v)
}

pub fn prepare_user_read(token: usize, ptr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr;
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let mut ready = true;
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.std_floor();
        let p_s = page_table.find_pte(vpn);
        let size = match p_s {
            Some((pte, size)) if pte.is_valid() && pte.readable() => size,
            _ => {
                ready = false;
                break;
            }
        };
        vpn.step_by(size.num_pages());
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        start = end_va.into();
    }
    if ready {
        return true;
    }
    if token != current_user_token() {
        warn!(
            "prepare_user_read: token mismatch, token = {:#x}, current_user_token = {:#x}",
            token,
            current_user_token()
        );
    }
    let task = current_task().unwrap();
    let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
        return false;
    };
    let sp = crate::task::current_trap_cx().get_sp();
    let result = mm
        .exclusive_access()
        .ensure_readable_user_range(ptr, len, sp);
    result
}

pub fn prepare_user_write(token: usize, ptr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr;
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let mut ready = true;
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.std_floor();
        let p_s = page_table.find_pte(vpn);
        let size = match p_s {
            Some((pte, size)) if pte.is_valid() && pte.writable() => {
                size
            }
            _ => {
                ready = false;
                break;
            }
        };
        vpn.step_by(size.num_pages());
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        start = end_va.into();
    }
    if ready {
        return true;
    }
    if token != current_user_token() {
        warn!(
            "prepare_user_write: token mismatch, token = {:#x}, current_user_token = {:#x}",
            token,
            current_user_token()
        );
    }
    let task = current_task().unwrap();
    let Some(mm) = task.inner_exclusive_access().mm.as_ref().cloned() else {
        return false;
    };
    let sp = crate::task::current_trap_cx().get_sp();
    let result = mm
        .exclusive_access()
        .ensure_writable_user_range(ptr, len, sp);
    result
}

pub fn translated_byte_buffer_mut(
    token: usize,
    ptr: *const u8,
    len: usize,
) -> Vec<&'static mut [u8]> {
    if !prepare_user_write(token, ptr as usize, len) {
        return Vec::new();
    }
    translated_byte_buffer(token, ptr, len)
}

pub fn try_translated_byte_buffer_mut(
    token: usize,
    ptr: *mut u8,
    len: usize,
) -> Option<Vec<&'static mut [u8]>> {
    if !prepare_user_write(token, ptr as usize, len) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.std_floor();
        let (ppn, size) = match page_table.translate_and_get_size(vpn) {
            Some((pte, size)) if pte.is_valid() => (pte.ppn(), size),
            _ => return None,
        };
        vpn.step_by(size.num_pages());
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.actual_page_offset(size) == 0 {
            v.push(&mut ppn.get_bytes_array_with_size(size)[start_va.actual_page_offset(size)..]);
        } else {
            v.push(
                &mut ppn.get_bytes_array_with_size(size)
                    [start_va.actual_page_offset(size)..end_va.actual_page_offset(size)],
            );
        }
        start = end_va.into();
    }
    Some(v)
}

/// Translate&Copy a ptr[u8] array end with `\0` to a `String` Vec through page table
pub fn translated_str(token: usize, ptr: *const u8) -> String {
    assert!(
        prepare_user_read(token, ptr as usize, 1),
        "translated_str: user ptr is not readable"
    );
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        assert!(
            prepare_user_read(token, va, 1),
            "translated_str: user ptr is not readable"
        );
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
    if ptr as isize <= 0 {
        return None;
    }
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
/*pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let len = core::mem::size_of::<T>();
    assert!(prepare_user_read(token, ptr as usize, len), "translated_ref: user ptr is not readable");
    let page_table = PageTable::from_token(token);
    let pa = page_table
        .translate_va(VirtAddr::from(ptr as usize))
        .unwrap();
    debug!("translated_ref: start_pa = 0x{:x}, end_pa = 0x{:x}, len = 0x{:x}", pa.0, pa.0 + len - 1, len);
    if pa.std_floor() == PhysAddr(pa.0 + len - 1).std_floor() {
        pa.get_ref()
    } else {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(translated_read(token, ptr)))
    }
}*/

/// 从给定地址读取数据并返回T
pub fn translated_read<T>(token: usize, ptr: *const T) -> T {
    try_translated_read(token, ptr)
        .unwrap_or_else(|| panic!("translated_read: failed to read from user space"))
}

pub fn try_translated_read<T>(token: usize, ptr: *const T) -> Option<T> {
    let len = core::mem::size_of::<T>();
    if !prepare_user_read(token, ptr as usize, len) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let mut data = vec![0u8; len];
    let start_va = VirtAddr::from(ptr as usize);
    let (pte, size) = page_table.find_pte(start_va.std_floor()).unwrap();
    // 页内快路径：保持原有低开销行为
    if start_va.std_page_offset() + len <= size.size() {
        let pa = page_table.translate_va(start_va).unwrap();
        // Physical RAM must be accessed through the LoongArch cached DMW window.
        let start = pa.get_cached_addr();
        let end = start + len;
        for (idx, addr) in (start..end).enumerate() {
            data[idx] = unsafe { *(addr as *const u8) };
        }
    } else {
        // 跨页路径：按虚拟地址逐字节翻译，避免假设物理地址连续
        for idx in 0..len {
            let va = VirtAddr::from((ptr as usize) + idx);
            let Some(pa) = page_table.translate_va(va) else {
                return None;
            };
            data[idx] = unsafe { *(pa.get_cached_addr() as *const u8) };
        }
    }
    Some(unsafe { core::ptr::read_unaligned(data.as_ptr() as *const T) })
}

/// 将用户空间的T写入给定地址
pub fn try_translated_write<T>(token: usize, ptr: *mut T, value: T) -> bool {
    let len = core::mem::size_of::<T>();
    if !prepare_user_write(token, ptr as usize, len) {
        return false;
    }
    let page_table = PageTable::from_token(token);
    let data = unsafe { core::slice::from_raw_parts((&value as *const T) as *const u8, len) };
    let start_va = VirtAddr::from(ptr as usize);
    let (pte, size) = page_table.find_pte(start_va.std_floor()).unwrap();
    // 页内快路径：保持原有低开销行为
    if start_va.std_page_offset() + len <= size.size() {
        let pa = page_table.translate_va(start_va).unwrap();
        // Physical RAM must be accessed through the LoongArch cached DMW window.
        let start = pa.get_cached_addr();
        let end = start + len;
        for (idx, addr) in (start..end).enumerate() {
            unsafe { *(addr as *mut u8) = data[idx] };
        }
    } else {
        // 跨页路径：按虚拟地址逐字节翻译，避免假设物理地址连续
        for idx in 0..len {
            let va = VirtAddr::from((ptr as usize) + idx);
            let Some(pa) = page_table.translate_va(va) else {
                return false;
            };
            unsafe { *(pa.get_cached_addr() as *mut u8) = data[idx] };
        }
    }

    true
}

pub fn translated_write<T>(token: usize, ptr: *mut T, value: T) {
    if !try_translated_write(token, ptr, value) {
        panic!("translated_write: failed to write to user space");
    };
}
