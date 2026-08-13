//! 页表定义与软件页表翻译
//! 
//! 当前的软件页表翻译参考了 linux 的 GUP，
//! 会在写/引用操作前先将相应物理页的 FrameTracker 引用计数 +1。
//! 
//! 锁序按照 mm 的一致协议

use super::memory_set::{MapArea, MapPermission, MemorySet};
use super::{
    frame_alloc, pte::*, FrameTracker, PTEFlags, PhysAddr, PhysPageNum, UserBuffer,
    UserBufferSegment, VirtAddr, VirtPageNum,
};
use crate::arch::config::{PAGE_SIZE, USER_TRAMPOLINE};
use alloc::string::String;
use alloc::sync::Arc;
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

fn is_static_user_trampoline(vpn: VirtPageNum, pte: PageTableEntry, page_size: PageSize) -> bool {
    extern "C" {
        fn strampoline();
    }

    let trampoline_vpn = VirtAddr::from(USER_TRAMPOLINE).std_floor();
    let trampoline_ppn =
        PhysAddr::from(strampoline as *const () as usize & !crate::CACHED_KERNEL_BASE).std_floor();
    vpn == trampoline_vpn
        && pte.ppn() == trampoline_ppn
        && page_size == PageSize::Page4K
        && !pte.writable()
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
    /* 弃用，可能有生命周期问题
    /// 从现有页表创建一个新的页表，复制内核高半的根项
    pub fn alias_of(other: &PageTable) -> Self {
        Self {
            root_ppn: other.root_ppn,
            frames: Vec::new(),
        }
    }
    */
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

    /* 页表遍历/创建相关方法 */

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
    ///
    /// 找到后读取一份，不返回可变引用
    #[cfg(target_arch = "riscv64")]
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<(PageTableEntry, PageSize)> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = ppn.get_pte_array_ref()[*idx];
            //println!("find_pte: vpn = {:?}, i = {}", vpn, i);
            // 标准页叶子节点需要v，大页叶子节点有rwx任一即可
            if (i == 2 && pte.is_valid())
                || (i < 2 && (pte.readable() || pte.writable() || pte.executable()))
            {
                let page_size = match i {
                    0 => PageSize::Page1G,
                    1 => PageSize::Page2M,
                    2 => PageSize::Page4K,
                    _ => unreachable!(),
                };
                return Some((pte, page_size));
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
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<(PageTableEntry, PageSize)> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = ppn.get_pte_array_ref()[*idx];
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
    /// 从虚拟页号找到页表项的可变引用
    ///
    /// 需要在页表写锁内完成对本函数返回的 PTE 的修改
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn find_pte_mut(
        &mut self,
        vpn: VirtPageNum,
    ) -> Option<(&mut PageTableEntry, PageSize)> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if (i == 2 && pte.is_valid())
                || (i < 2 && (pte.readable() || pte.writable() || pte.executable()))
            {
                let page_size = match i {
                    0 => PageSize::Page1G,
                    1 => PageSize::Page2M,
                    2 => PageSize::Page4K,
                    _ => unreachable!(),
                };
                return Some((pte, page_size));
            }
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn find_pte_mut(
        &mut self,
        vpn: VirtPageNum,
    ) -> Option<(&mut PageTableEntry, PageSize)> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if pte.is_empty() {
                return None;
            }
            if i == 2 || pte.is_huge_page() {
                let page_size = match i {
                    0 => PageSize::Page1G,
                    1 => PageSize::Page2M,
                    2 => PageSize::Page4K,
                    _ => unreachable!(),
                };
                return Some((pte, page_size));
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
        let (pte, _page_size) = self.find_pte_mut(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }

    /// 迁移一个页表项到另一个虚拟页号，要求源页号已映射，目标页号未映射
    pub(crate) fn move_entry(
        &mut self,
        src: VirtPageNum,
        dst: VirtPageNum,
        page_size: PageSize,
    ) -> bool {
        if src == dst {
            return true;
        }

        let Some((entry, src_page_size)) = self.find_pte(src) else {
            return false;
        };
        if !entry.is_valid() || src_page_size != page_size {
            return false;
        }
        if self.translate(dst).is_some_and(|pte| pte.is_valid()) {
            return false;
        }

        let Some(dst_pte) = self.find_pte_create(dst, page_size) else {
            return false;
        };
        if dst_pte.is_valid() {
            return false;
        }
        *dst_pte = entry;

        let Some((src_pte, src_page_size)) = self.find_pte_mut(src) else {
            unreachable!("source PTE disappeared while page table write lock is held");
        };
        debug_assert_eq!(src_page_size, page_size);
        *src_pte = PageTableEntry::empty();
        true
    }

    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|(pte, _)| pte)
    }
    pub fn translate_and_get_size(&self, vpn: VirtPageNum) -> Option<(PageTableEntry, PageSize)> {
        self.find_pte(vpn)
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
        let (pte, _size) = self.find_pte_mut(vpn).unwrap();
        let mut final_flags = flags | PTEFlags::V | PTEFlags::A;
        if flags.contains(PTEFlags::W) && !flags.contains(PTEFlags::U) {
            final_flags |= PTEFlags::D;
        }
        *pte = PageTableEntry::new(ppn, final_flags);
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_entry(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let (pte, _size) = self.find_pte_mut(vpn).unwrap();
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
    /// 取出本页表拥有的全部页表帧，用于延迟释放
    pub fn take_frames(&mut self) -> Vec<FrameTracker> {
        core::mem::take(&mut self.frames)
    }
}
enum PinUserRangeResult {
    Pinned(Vec<UserBufferSegment>),
    NeedsFault,
    Retry, // 用户页表在 pin 过程中被修改，用于 RCU，但目前没有实现
    Invalid,
}

enum PinUserSegmentResult {
    Pinned(UserBufferSegment, usize),
    NeedsFault,
    Retry,
    Invalid,
}

const MAX_TRANSIENT_USER_PIN_RETRIES: usize = 8;

#[inline]
fn area_allows_user_access(permission: MapPermission, write: bool) -> bool {
    permission.contains(MapPermission::U)
        && if write {
            permission.contains(MapPermission::W)
        } else {
            permission.contains(MapPermission::R)
        }
}

#[inline]
fn pte_allows_user_access(pte: PageTableEntry, write: bool) -> bool {
    pte.is_valid()
        && pte.user_accessible()
        && if write {
            pte.writable()
        } else {
            pte.readable()
        }
}

/// 获用户空间虚拟地址的相应缓冲区
/// 
/// 当前为 areas.read() -> area.read() -> page_table.read() 的读锁访问。
fn pin_user_segment(
    mm: &MemorySet,
    va: VirtAddr,
    end: usize,
    write: bool,
) -> PinUserSegmentResult {
    if va.0 >= end {
        return PinUserSegmentResult::Invalid;
    }

    let vpn = va.std_floor();
    let areas = mm.areas.read();
    let area = areas
        .range(..=vpn)
        .next_back()
        .map(|(_, area)| Arc::clone(area));

    let Some(area) = area else {
        // The user trampoline is intentionally not represented by a VMA.  It
        // is the sole untracked mapping accepted by the generic user-buffer
        // API, and only for read access.
        let page_table = mm.page_table.read();
        let Some((pte, page_size)) = page_table.translate_and_get_size(vpn) else {
            return PinUserSegmentResult::Invalid;
        };
        if write {
            return PinUserSegmentResult::Invalid;
        }
        if !pte_allows_user_access(pte, false) || !is_static_user_trampoline(vpn, pte, page_size) {
            return if pte.is_valid() {
                PinUserSegmentResult::Retry
            } else {
                PinUserSegmentResult::Invalid
            };
        }
        let offset = va.actual_page_offset(page_size);
        let count = core::cmp::min(page_size.size() - offset, end - va.0);
        return match unsafe {
            UserBufferSegment::from_untracked_phys(pte.ppn(), page_size, offset, count)
        } {
            Some(segment) => PinUserSegmentResult::Pinned(segment, count),
            None => PinUserSegmentResult::Invalid,
        };
    };

    let area = area.read();
    if !area.contains(vpn) {
        // The predecessor VMA changed while the tree lookup was being made.
        return PinUserSegmentResult::Retry;
    }
    let permission = area.get_map_permission();
    let frame_backed = area.is_frame_backed_mapping();
    // `area` is already a VersionedAreaReadGuard.  Use the guarded MapArea
    // directly: calling VersionedArea::frame_for_vpn here would attempt to
    // lock the same non-reentrant mutex a second time.
    let frame = MapArea::frame_for_vpn(&area, vpn);
    let page_table = mm.page_table.read();
    let Some((pte, page_size)) = page_table.translate_and_get_size(vpn) else {
        return if frame.is_none() && frame_backed && area_allows_user_access(permission, write) {
            PinUserSegmentResult::NeedsFault
        } else if frame_backed {
            PinUserSegmentResult::Retry
        } else {
            PinUserSegmentResult::Invalid
        };
    };

    if !area_allows_user_access(permission, write) || !pte.user_accessible() {
        return PinUserSegmentResult::Invalid;
    }
    let Some(frame) = frame else {
        // A valid PTE without a tracked frame is not a user-page pin.  This
        // can be a transient replacement window, but must never become a bare
        // PPN direct-map pointer.
        return if frame_backed {
            PinUserSegmentResult::Retry
        } else {
            PinUserSegmentResult::Invalid
        };
    };
    if frame.page_size != page_size || frame.ppn != pte.ppn() {
        return if frame_backed {
            PinUserSegmentResult::Retry
        } else {
            PinUserSegmentResult::Invalid
        };
    }

    if !pte_allows_user_access(pte, write) {
        // A writable VMA with a present read-only PTE is the normal COW case.
        // Let the current task resolve it once, then validate from scratch.
        return if write && frame_backed && permission.contains(MapPermission::W) {
            PinUserSegmentResult::NeedsFault
        } else {
            PinUserSegmentResult::Invalid
        };
    }

    let offset = va.actual_page_offset(page_size);
    let count = core::cmp::min(page_size.size() - offset, end - va.0);
    match UserBufferSegment::from_frame(frame, offset, count) {
        Some(segment) => PinUserSegmentResult::Pinned(segment, count),
        None => PinUserSegmentResult::Invalid,
    }
}

/// 获取一段用户缓冲区
fn pin_user_range_once(
    mm: &MemorySet,
    start: usize,
    len: usize,
    write: bool,
) -> PinUserRangeResult {
    let Some(end) = start.checked_add(len) else {
        return PinUserRangeResult::Invalid;
    };
    if len == 0 {
        return PinUserRangeResult::Pinned(Vec::new());
    }
    let mut cursor = start;
    let mut result = Vec::new();
    while cursor < end {
        match pin_user_segment(mm, VirtAddr::from(cursor), end, write) {
            PinUserSegmentResult::Pinned(segment, count) if count != 0 => {
                result.push(segment);
                cursor += count;
            }
            PinUserSegmentResult::Pinned(_, _) => return PinUserRangeResult::Invalid,
            PinUserSegmentResult::NeedsFault => return PinUserRangeResult::NeedsFault,
            PinUserSegmentResult::Retry => return PinUserRangeResult::Retry,
            PinUserSegmentResult::Invalid => return PinUserRangeResult::Invalid,
        }
    }

    PinUserRangeResult::Pinned(result)
}

fn handle_fault_in_range(
    mm: &MemorySet,
    start: usize,
    len: usize,
    write: bool,
) -> bool {
    // A generic kernel copy can target a remote mm and can be called while
    // the current task's inner lock is held.  Fault-in is VMA-based and does
    // not need an architectural stack pointer because stacks are fixed sparse
    // VMAs rather than grow-down mappings.
    if write {
        mm.ensure_writable_user_range(start, len)
    } else {
        mm.ensure_readable_user_range(start, len)
    }
}

/// Pin at most one translated user-memory segment without allocating the
/// `Vec` used by multi-page callers.  The returned segment owns its frame
/// reference, so it remains valid after page-table and VMA locks are dropped.
fn pin_user_segment_with_retry(
    mm: &MemorySet,
    start: usize,
    len: usize,
    write: bool,
) -> Option<(UserBufferSegment, usize)> {
    let end = start.checked_add(len)?;
    if len == 0 {
        return None;
    }

    let mut retries = 0;
    let mut faults = 0;
    loop {
        match pin_user_segment(mm, VirtAddr::from(start), end, write) {
            PinUserSegmentResult::Pinned(segment, count) if count != 0 => {
                return Some((segment, count));
            }
            PinUserSegmentResult::Pinned(_, _) => return None,
            PinUserSegmentResult::NeedsFault => {
                faults += 1;
                if faults > MAX_TRANSIENT_USER_PIN_RETRIES
                    || !handle_fault_in_range(mm, start, len, write)
                {
                    return None;
                }
                retries = 0;
            }
            PinUserSegmentResult::Retry if retries < MAX_TRANSIENT_USER_PIN_RETRIES => {
                retries += 1;
            }
            PinUserSegmentResult::NeedsFault
            | PinUserSegmentResult::Retry
            | PinUserSegmentResult::Invalid => return None,
        }
    }
}

/// 固定待访问地址空间的页帧，返回固定得到的 buffer
fn pin_user_range(
    mm: &MemorySet,
    start: usize,
    len: usize,
    write: bool,
) -> Option<Vec<UserBufferSegment>> {
    // A concurrent VMA/PTE transition can invalidate an otherwise valid pin
    // attempt.  `NeedsFault` is not itself an invalid user pointer: in
    // particular, fork can make a private writable PTE read-only for COW.
    let mut retries = 0;
    let mut faults = 0;
    loop {
        match pin_user_range_once(mm, start, len, write) {
            PinUserRangeResult::Pinned(result) => return Some(result),
            PinUserRangeResult::NeedsFault => {
                faults += 1;
                if faults > MAX_TRANSIENT_USER_PIN_RETRIES {
                    return None;
                }

                if !handle_fault_in_range(mm, start, len, write) {
                    return None;
                }
                retries = 0;
            }
            PinUserRangeResult::Retry if retries < MAX_TRANSIENT_USER_PIN_RETRIES => {
                retries += 1;
            }
            PinUserRangeResult::NeedsFault
            | PinUserRangeResult::Retry
            | PinUserRangeResult::Invalid => return None,
        }
    }
}

pub fn translated_byte_buffer(mm: &MemorySet, ptr: *const u8, len: usize) -> Vec<UserBufferSegment> {
    pin_user_range(mm, ptr as usize, len, false).unwrap_or_default()
}

pub fn try_translated_byte_buffer(
    mm: &MemorySet,
    ptr: *const u8,
    len: usize,
) -> Option<Vec<UserBufferSegment>> {
    pin_user_range(mm, ptr as usize, len, false)
}

pub fn translated_user_buffer(mm: &MemorySet, ptr: *const u8, len: usize) -> Option<UserBuffer> {
    pin_user_range(mm, ptr as usize, len, false).map(UserBuffer::new)
}

pub fn prepare_user_read(mm: &MemorySet, ptr: usize, len: usize) -> bool {
    pin_user_range(mm, ptr, len, false).is_some()
}

pub fn prepare_user_write(mm: &MemorySet, ptr: usize, len: usize) -> bool {
    pin_user_range(mm, ptr, len, true).is_some()
}

pub fn translated_byte_buffer_mut(
    mm: &MemorySet,
    ptr: *const u8,
    len: usize,
) -> Vec<UserBufferSegment> {
    pin_user_range(mm, ptr as usize, len, true).unwrap_or_default()
}

pub fn try_translated_byte_buffer_mut(
    mm: &MemorySet,
    ptr: *mut u8,
    len: usize,
) -> Option<Vec<UserBufferSegment>> {
    pin_user_range(mm, ptr as usize, len, true)
}

/// Translate a writable user range and keep all backing frames pinned for the
/// lifetime of the returned logical buffer.
pub fn translated_user_buffer_mut(mm: &MemorySet, ptr: *mut u8, len: usize) -> Option<UserBuffer> {
    pin_user_range(mm, ptr as usize, len, true).map(UserBuffer::new)
}

/// Translate&Copy a ptr[u8] array end with `\0` to a `String` Vec through page table
/// Maximum length accepted by the generic C-string copy helper.  Pathname
/// callers impose their own, smaller PATH_MAX limits after copying; argv and
/// environment users still need a larger bound.
const MAX_USER_CSTRING_LEN: usize = 128 * 1024;

/// Why a C-string copy stopped before reaching a terminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserCStringError {
    Invalid,
    TooLong,
}

/// Copy a NUL-terminated userspace string a page at a time.
///
/// The former implementation called `try_translated_read::<u8>` once per
/// byte.  Each call pinned a user range and allocated a segment Vec, making a
/// short pathname perform dozens of VMA/page-table lookups.  Limiting every
/// pin to the current base page preserves the important property that bytes
/// after the terminating NUL need not be mapped.
fn copy_user_cstring(
    mm: &MemorySet,
    ptr: *const u8,
    max_len: usize,
) -> Result<String, UserCStringError> {
    if ptr as isize <= 0 {
        return Err(UserCStringError::Invalid);
    }

    let mut string = String::with_capacity(64);
    let mut va = ptr as usize;
    let mut copied = 0usize;
    // Scan one extra byte so callers can accept an exactly `max_len` byte
    // string while recognizing an unterminated or longer input.
    let scan_limit = max_len.checked_add(1).ok_or(UserCStringError::TooLong)?;

    while copied < scan_limit {
        let page_remaining = PAGE_SIZE - (va & (PAGE_SIZE - 1));
        let chunk_len = core::cmp::min(page_remaining, scan_limit - copied);
        let (segment, segment_len) = pin_user_segment_with_retry(mm, va, chunk_len, false)
            .ok_or(UserCStringError::Invalid)?;
        let bytes = &segment[..];
        let take = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        if copied > max_len || take > max_len - copied {
            return Err(UserCStringError::TooLong);
        }
        let prefix = &bytes[..take];
        if prefix.is_ascii() {
            // ASCII is valid UTF-8, so the common pathname case can append a
            // complete page fragment with one allocation-aware copy.
            string.push_str(unsafe { core::str::from_utf8_unchecked(prefix) });
        } else {
            string.reserve(take);
            for &byte in prefix {
                // Keep the legacy byte-to-char conversion semantics.  VFS
                // pathname validation remains responsible for rejecting names
                // it cannot represent safely.
                string.push(byte as char);
            }
        }

        if take != bytes.len() {
            return Ok(string);
        }

        va = va
            .checked_add(segment_len)
            .ok_or(UserCStringError::Invalid)?;
        copied = copied
            .checked_add(segment_len)
            .ok_or(UserCStringError::TooLong)?;
    }

    Err(UserCStringError::TooLong)
}

pub fn translated_str(mm: &MemorySet, ptr: *const u8) -> String {
    copy_user_cstring(mm, ptr, MAX_USER_CSTRING_LEN).unwrap_or_else(|_| {
        panic!("translated_str: user ptr is not readable or string is too long")
    })
}

/// Translate a NUL-terminated string from userspace, returning None for an
/// invalid pointer or an unterminated string over the generic safety limit.
pub fn try_translated_str(mm: &MemorySet, ptr: *const u8) -> Option<String> {
    copy_user_cstring(mm, ptr, MAX_USER_CSTRING_LEN).ok()
}

/// Translate a C string with a caller-specific byte limit.
///
/// Pathname syscalls use this to return `ENAMETOOLONG`, while exec can map an
/// oversized argv/environment string to `E2BIG`, instead of conflating either
/// case with an invalid userspace address.
pub fn try_translated_str_with_limit(
    mm: &MemorySet,
    ptr: *const u8,
    max_len: usize,
) -> Result<String, UserCStringError> {
    copy_user_cstring(mm, ptr, max_len)
}

/// 从给定地址读取数据并返回T
pub fn translated_read<T>(mm: &MemorySet, ptr: *const T) -> T {
    try_translated_read(mm, ptr)
        .unwrap_or_else(|| panic!("translated_read: failed to read from user space"))
}

pub fn try_translated_read<T>(mm: &MemorySet, ptr: *const T) -> Option<T> {
    let len = core::mem::size_of::<T>();
    let buffers = pin_user_range(mm, ptr as usize, len, false)?;
    let mut data = vec![0u8; len];
    let mut copied = 0;
    for buffer in &buffers {
        let count = core::cmp::min(buffer.len(), len - copied);
        data[copied..copied + count].copy_from_slice(&buffer[..count]);
        copied += count;
    }
    Some(unsafe { core::ptr::read_unaligned(data.as_ptr() as *const T) })
}

/// 将用户空间的T写入给定地址
pub fn try_translated_write<T>(mm: &MemorySet, ptr: *mut T, value: T) -> bool {
    let len = core::mem::size_of::<T>();
    let Some(mut buffers) = pin_user_range(mm, ptr as usize, len, true) else {
        return false;
    };
    let data = unsafe { core::slice::from_raw_parts((&value as *const T) as *const u8, len) };
    let mut copied = 0;
    for buffer in &mut buffers {
        let count = core::cmp::min(buffer.len(), len - copied);
        buffer[..count].copy_from_slice(&data[copied..copied + count]);
        copied += count;
    }
    true
}

pub fn translated_write<T>(mm: &MemorySet, ptr: *mut T, value: T) {
    if !try_translated_write(mm, ptr, value) {
        panic!("translated_write: failed to write to user space");
    };
}
