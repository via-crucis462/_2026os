use super::id::*;
use super::{frame_alloc, FrameTracker};
use super::{pte::*, PTEFlags, PageTable};
#[allow(unused)]
use super::{PageSize, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
#[allow(unused)]
use crate::arch::config::*;
use crate::fs::File;
use crate::mm::PageSize::Page4K;
use crate::mm::{get_free_frames, mmap, UserBuffer};
use crate::process::signal::frame;
use crate::sync::MPSafeCell;
use crate::syscall::errno::Errno;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use core::hint::spin_loop;
use core::ptr::{read_volatile, write_volatile};

use spin::{Mutex, RwLock};

use lazy_static::*;

#[cfg(target_arch = "riscv64")]
use riscv::register::satp;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
}

lazy_static! {
    /// The kernel's initial memory mapping(kernel address space)
    ///
    /// 不含内核栈的映射，内核栈由栈分配器在每个线程创建时单独映射
    pub static ref KERNEL_SPACE: Arc<MPSafeCell<MemorySet>> =
        Arc::new(MPSafeCell::new(MemorySet::new_kernel()));
}

/// the kernel token
pub fn kernel_token() -> usize {
    KERNEL_SPACE.exclusive_access().token()
}

/// ASID used by the kernel page table that owns all kernel-stack mappings.
///
/// ASIDs identify address spaces rather than individual virtual ranges, so
/// every kernel stack shares this dedicated kernel address-space ASID.
pub fn kernel_asid() -> usize {
    KERNEL_SPACE.exclusive_access().asid()
}

/// Invalidate shared kernel mappings under every active user ASID.
#[cfg(target_arch = "riscv64")]
pub fn flush_kernel_tlb_targets() {
    use core::sync::atomic::{fence, Ordering};

    fence(Ordering::SeqCst);
    let local_bit = 1usize << crate::get_hart_id();
    for (token, harts) in crate::mm::active_tokens() {
        let asid = (token >> 44) & 0xffff;
        if harts & local_bit != 0 {
            crate::arch::mm::flush_tlb_for_asid(asid);
        }
        let remote_harts = harts & !local_bit;
        if remote_harts != 0 {
            crate::arch::sbi::remote_sfence_vma_asid(remote_harts, asid);
        }
    }
}

/// address space
/// 
/// 在锁粒度细化后约定锁序：拿外层锁访问 Self -> MapArea -> PageTable
/// 
/// 如果有解除映射，在 PageTable 修改后调用 flush_tlb_targets() 刷新 tlb，再释放旧帧
pub struct MemorySet {
    page_table: PageTable,
    asid: ASIDHandle,
    pub areas: Vec<MapArea>,
    /// 当前程序断点
    brk: VirtAddr,
    /// 堆区起点（创建地址空间时确定），brk 不允许低于该值
    start_brk: VirtAddr,
}

impl MemorySet {
    /// Share the kernel's Sv39 upper half with this user page table.
    ///
    /// The copied root entries point at kernel-owned lower-level page tables;
    /// this MemorySet owns only its lower-half page table frames and mappings.
    #[cfg(target_arch = "riscv64")]
    pub fn install_kernel_space(&mut self) {
        let kernel_space = KERNEL_SPACE.exclusive_access();
        self.page_table.share_kernel_half(&kernel_space.page_table);
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn flush_tlb_after_mapping_change() {
        unsafe {
            // 先保证页表写入对重填路径可见，再失效陈旧 TLB 项。
            asm!("dbar 0");
            asm!("invtlb 0, $r0, $r0");
            asm!("dbar 0");
        }
    }

    /// Create a new empty `MemorySet`.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            asid: asid_alloc().into(),
            areas: Vec::new(),
            brk: VirtAddr::from(0),
            start_brk: VirtAddr::from(0),
        }
    }

    /// Create a MemorySet that shares the same page table with the parent.
    /// Used by fork() with CLONE_VM flag for true address space sharing.
    ///
    /// - Shares the parent's page_table (same root_ppn → same satp token)
    /// - Allocates a new ASID (different TLB tag, but same page table content)
    /// - Copies area metadata but with empty data_frames (child doesn't own parent's frames)
    /// - Child's own trap_cx page (pushed later) gets its own FrameTracker
    pub fn share_from_parent(parent: &Self) -> Self {
        let areas: Vec<MapArea> = parent
            .areas
            .iter()
            .map(|a| MapArea::from_another(a))
            .collect();
        /*for area in &areas {
            println!("shared area: [0x{:x}, 0x{:x}), {:?}", area.vpn_range.get_start().0 * PAGE_SIZE, area.vpn_range.get_end().0 * PAGE_SIZE, area.map_perm);
        }*/
        Self {
            page_table: PageTable::alias_of(&parent.page_table), // 共享根页表，但不拥有中间页帧
            asid: asid_alloc(),                                  // New ASID for child
            areas,
            brk: parent.brk,
            start_brk: parent.start_brk,
        }
    }
    /// Get the page table token
    pub fn token(&self) -> usize {
        #[cfg(target_arch = "riscv64")]
        {
            return self.page_table.token(self.asid());
        }
        #[cfg(target_arch = "loongarch64")]
        {
            self.page_table.token()
        }
    }
    pub fn asid(&self) -> usize {
        self.asid.0
    }
    /// Invalidate this address space on every hart currently running it.
    ///
    /// Call this after changing a PTE and before releasing a frame that used
    /// to be reachable through that PTE. OpenSBI completes remote fences before
    /// returning, so callers may safely drop such frames afterwards.
    #[cfg(target_arch = "riscv64")]
    pub fn flush_tlb_targets(&self) {
        use core::sync::atomic::{fence, Ordering};

        // Publish the PTE update before observing the active-hart set. A hart
        // published afterwards enters only after the new PTE is visible.
        fence(Ordering::SeqCst);
        let targets = crate::mm::running_harts(self.token());
        if targets == 0 {
            return;
        }

        let local_bit = 1usize << crate::get_hart_id();
        if targets & local_bit != 0 {
            crate::arch::mm::flush_tlb_for_asid(self.asid());
        }
        let remote_targets = targets & !local_bit;
        if remote_targets != 0 {
            crate::arch::sbi::remote_sfence_vma_asid(remote_targets, self.asid());
        }
    }
    pub fn areas(&self) -> &Vec<MapArea> {
        &self.areas
    }
    /// 堆区起点，program break 不允许低于该值。
    pub fn start_brk(&self) -> usize {
        self.start_brk.0
    }

    /// 返回当前 program break（字节粒度）。
    pub fn current_brk(&self) -> usize {
        self.brk.0
    }

    /// 调整当前任务的 program break，并把堆区补充填满或收缩。
    pub fn change_program_brk(&mut self, addr: usize) -> Result<usize, i32> {
        let old_brk = self.brk.0;
        if addr == 0 {
            return Ok(old_brk);
        }
        if addr < self.start_brk.0 {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }

        let old_top = VirtAddr::from(old_brk).std_ceil();
        let new_top = VirtAddr::from(addr).std_ceil();
        if old_top == new_top {
            // 同一页内移动，无需改动映射
            self.brk = VirtAddr::from(addr);
            return Ok(addr);
        }

        if addr > old_brk {
            let pages = (new_top.0 - old_top.0) / PAGE_SIZE;
            if get_free_frames() < pages {
                return Err(Errno::ENOMEM.as_isize() as i32);
            }
            self.grow_heap(old_top.into(), new_top.into())?;
        } else {
            // shrink_heap 的参数是 (旧堆顶, 新堆顶)：删除 [new_top, old_top) 区间。
            self.shrink_heap(old_top.into(), new_top.into())?;
        }
        self.brk = VirtAddr::from(addr);
        Ok(addr)
    }

    /// 扩展堆区到 [from, to)（均已页对齐）：优先扩展现有堆顶区域，否则新建。
    fn grow_heap(&mut self, from: VirtAddr, to: VirtAddr) -> Result<(), i32> {
        let from_vpn = from.std_ceil();
        let to_vpn = to.std_ceil();
        if to_vpn <= from_vpn {
            return Ok(());
        }
        if self.has_conflict(from.0, to.0 - from.0) {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }
        let heap_start_vpn = self.start_brk.std_ceil();
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|a| {
                a.vpn_range.get_end() == from_vpn && a.vpn_range.get_start() >= heap_start_vpn
            })
        {
            area.append_to(&mut self.page_table, to_vpn);
        } else {
            let area = MapArea::new(
                from,
                to,
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
                PageSize::Page4K,
            );
            self.push(area, None, from.0);
        }
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        Ok(())
    }

    /// 收缩堆区，跨越空洞/分裂区域时整段移除
    fn shrink_heap(&mut self, from: VirtAddr, to: VirtAddr) -> Result<(), i32> {
        let from_vpn = from.std_ceil();
        let to_vpn = to.std_ceil();
        if to_vpn >= from_vpn {
            return Ok(());
        }
        let heap_start_vpn = self.start_brk.std_ceil();
        let mut frames = Vec::new();
        let mut found = false;
        for area in self.areas.iter_mut() {
            let a_start = area.vpn_range.get_start();
            let a_end = area.vpn_range.get_end();
            if a_start >= from_vpn || a_end <= to_vpn || a_start < heap_start_vpn {
                continue;
            }
            found = true;
            if a_start < to_vpn {
                // 区域跨越新的 brk 所在页：只删除 [to_vpn, a_end) 部分
                frames.extend(area.shrink_to(&mut self.page_table, to_vpn));
            } else {
                // 整块位于待收缩范围内
                frames.extend(area.unmap(&mut self.page_table));
                area.resize(a_start, a_start);
            }
        }
        if !found {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }
        // 移除被清空的区域
        self.areas
            .retain(|a| a.vpn_range.get_start() < a.vpn_range.get_end());
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(frames);
        Ok(())
    }
    /// Assume that no conflicts.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        page_size: PageSize,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission, page_size),
            None,
            start_va.into(),
        );
    }
    /// 插入惰性文件映射，首次访问时由 handle_page_fault 填充物理页。
    pub fn insert_file_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        page_size: PageSize,
        file: Arc<dyn File + Send + Sync>,
        page_offset: usize,
        is_shared: bool,
    ) {
        let mut area = MapArea::new(start_va, end_va, MapType::File, permission, page_size);
        area.backing_file = Some((file, page_offset));
        area.is_shared = is_shared;
        self.areas.push(area);
    }
    /// remove a area
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) -> Vec<FrameTracker> {
        if let Some(idx) = self
            .areas
            .iter()
            .enumerate()
            .find(|(_, area)| area.vpn_range.get_start() == start_vpn)
            .map(|(idx, _)| idx)
        {
            let frames = self.areas[idx].unmap(&mut self.page_table);
            self.areas.remove(idx);
            frames
        } else {
            Vec::new()
        }
    }
    /// Add a new MapArea into this MemorySet.
    /// Assuming that there are no conflicts in the virtual address
    /// space.
    pub fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>, start_va: usize) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, start_va);
        }
        trace!(
            "map area: [{:#x}, {:#x}), {:?}",
            map_area.vpn_range.get_start().0 * PAGE_SIZE,
            map_area.vpn_range.get_end().0 * PAGE_SIZE,
            map_area.map_perm
        );
        self.areas.push(map_area);
    }
    fn push_guard_area(&mut self, guard_start: usize, guard_pages: usize) {
        self.areas.push(MapArea::new(
            guard_start.into(),
            (guard_start + guard_pages * PAGE_SIZE).into(),
            MapType::Guard,
            MapPermission::empty(),
            PageSize::Page4K,
        ));
    }
    /// Mention that trampoline is not collected by areas.
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]
    fn map_trampoline(&mut self) {
        info!("mapping trampoline");
        self.page_table.map(
            VirtAddr::from(TRAMPOLINE).into(),
            PhysAddr::from(strampoline as *const () as usize & !crate::CACHED_KERNEL_BASE).into(),
            PTEFlags::R | PTEFlags::X,
            PageSize::Page4K, // 默认标准页大小
        );
    }

    // 用户态使用的跳板页（主要用于信号处理后恢复）
    fn map_user_trampoline(&mut self) {
        info!("mapping user trampoline");
        self.page_table.map(
            VirtAddr::from(USER_TRAMPOLINE).into(),
            PhysAddr::from(strampoline as *const () as usize & !crate::CACHED_KERNEL_BASE).into(),
            PTEFlags::R | PTEFlags::X | PTEFlags::U,
            PageSize::Page4K, // 默认标准页大小
        );
    }
    /// 创建并映射内核空间
    /// 
    /// 不含页帧，仅映射到页表(rv)，初始化时映射，后续不再修改。
    /// 主要针对 riscv，la 下内核使用 MMU 的映射窗口，
    /// 所以对于 la 这部分无意义，只 push 段。
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();

        info!(
            ".text [{:#x}, {:#x})",
            stext as *const () as usize, etext as *const () as usize
        );
        info!(
            ".rodata [{:#x}, {:#x})",
            srodata as *const () as usize, erodata as *const () as usize
        );
        info!(
            ".data [{:#x}, {:#x})",
            sdata as *const () as usize, edata as *const () as usize
        );
        info!(
            ".bss [0x{:x}, 0x{:x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );

        #[cfg(target_arch = "riscv64")]
        {
            memory_set.map_trampoline();
            memory_set.map_user_trampoline();
        }

        info!("mapping .text section");
        memory_set.push(
            MapArea::new(
                (stext as *const () as usize).into(),
                (etext as *const () as usize).into(),
                MapType::Windowed,
                MapPermission::R | MapPermission::X,
                PageSize::Page2M,
            ),
            None,
            stext as *const () as usize,
        );

        info!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as *const () as usize).into(),
                (erodata as *const () as usize).into(),
                MapType::Windowed,
                MapPermission::R,
                PageSize::Page2M,
            ),
            None,
            srodata as *const () as usize,
        );

        info!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as *const () as usize).into(),
                (edata as *const () as usize).into(),
                MapType::Windowed,
                MapPermission::R | MapPermission::W,
                PageSize::Page2M,
            ),
            None,
            sdata as *const () as usize,
        );

        info!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as *const () as usize).into(),
                (ebss as *const () as usize).into(),
                MapType::Windowed,
                MapPermission::R | MapPermission::W,
                PageSize::Page2M,
            ),
            None,
            sbss_with_stack as *const () as usize,
        );

        #[cfg(target_arch = "loongarch64")]
        {
            info!("mapping memory for devices");

            let ekernel_addr = ekernel as *const () as usize;

            memory_set.push(
                MapArea::new(
                    ekernel_addr.into(),
                    (ekernel_addr + DMA_SIZE).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K,
                ),
                None,
                ekernel_addr,
            );

            info!("mapping physical memory");

            memory_set.push(
                MapArea::new(
                    LOWRAM_BASE.into(),
                    LOWRAM_END.into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K,
                ),
                None,
                LOWRAM_BASE,
            );
        }

        #[cfg(target_arch = "riscv64")]
        {
            info!("mapping physical memory");

            // Windowed 的 VA = CACHED_KERNEL_BASE + PA：起点直接是 ekernel（已在窗口内），
            // 终点需要把物理 MEMORY_END 抬进窗口，否则 VPNRange 起点大于终点会 panic。
            let start = ekernel as *const () as usize;

            memory_set.push(
                MapArea::new(
                    start.into(),
                    (MEMORY_END | CACHED_KERNEL_BASE).into(),
                    MapType::Windowed,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page2M,
                ),
                None,
                start,
            );
        }

        info!("mapping memory-mapped registers");
        for &(base, size) in MMIO {
            // MMIO 物理地址抬进窗口（设备访问按 UNCACHED 窗口约定，
            // rv 下与 CACHED_KERNEL_BASE 同值）；保持 4K 页以免 2M 页对齐问题。
            memory_set.push(
                MapArea::new(
                    (base | UNCACHED_KERNEL_BASE).into(),
                    ((base + size) | UNCACHED_KERNEL_BASE).into(),
                    MapType::Windowed,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K,
                ),
                None,
                base,
            );
        }

        memory_set
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// and return heap_bottom/user_sp/final_entry/main_entry metadata.
    /// Memoryset//堆底//用户栈顶//最终入口点（可能是解释器）//主程序入口点//程序头表地址//程序头表数量//程序头表项大小//解释器加载基址（如果有）
    pub fn from_elf(
        elf_data: &[u8],
    ) -> Option<(
        Self,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        Option<usize>,
    )> {
        Self::from_elf_with_interp_loader(elf_data, |_| None)
    }

    /// Build address space from a main ELF and an optional interpreter loader.
    pub fn from_elf_with_interp_loader<F>(
        elf_data: &[u8],
        mut load_interp: F,
    ) -> Option<(
        Self,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        Option<usize>,
    )>
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        /// 获取对齐要求，返回至少为 PAGE_SIZE 的对齐值，或者 None 表示无效对齐
        fn load_align(raw_align: u64) -> Option<usize> {
            let align = usize::try_from(raw_align).ok()?;
            match align {
                0 | 1 => Some(PAGE_SIZE),
                _ if align.is_power_of_two() => Some(align.max(PAGE_SIZE)),
                _ => None,
            }
        }
        /// 把 value 向上对齐到 align 的倍数，返回 Some(对齐后的值) 或 None（溢出）
        fn align_up(value: usize, align: usize) -> Option<usize> {
            value
                .checked_add(align - 1)
                .map(|value| value & !(align - 1))
        }

        let mut memory_set = Self::new_bare();

        // 用户态信号处理后恢复跳板
        memory_set.map_user_trampoline();

        //读取elf头部，获取程序头表等信息
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;

        //校验魔数是否相等
        let magic = elf_header.pt1.magic;
        if magic != [0x7f, 0x45, 0x4c, 0x46] {
            println!("invalid ELF magic: {:x?}", magic);
            return None;
        }

        //获取段数
        let ph_count = elf_header.pt2.ph_count();

        let mut max_end_vpn = VirtPageNum(0);
        let mut main_max_end_vpn = VirtPageNum(0);

        // The runtime load bias of ET_DYN images must preserve each PT_LOAD's
        // p_vaddr/p_offset alignment. Use the strongest requirement of the
        // image and reject malformed alignment values instead of mapping an
        // image with an invalid bias.
        let Some(main_load_align) = elf
            .program_iter()
            .try_fold(PAGE_SIZE, |max_align, segment| {
                if matches!(segment.get_type(), Ok(xmas_elf::program::Type::Load)) {
                    Some(max_align.max(load_align(segment.align())?))
                } else {
                    Some(max_align)
                }
            })
        else {
            warn!("MemorySet::from_elf: invalid main PT_LOAD alignment");
            return None;
        };
        let main_load_bias = match elf_header.pt2.type_().as_type() {
            xmas_elf::header::Type::SharedObject => align_up(USER_APP_BASE, main_load_align)?,
            _ => OFFSET_FOR_USER_APP,
        };

        let mut phdr_addr = 0;
        let phnum = ph_count as usize;
        let phent = elf_header.pt2.ph_entry_size() as usize;
        let mut final_entry =
            (elf.header.pt2.entry_point() as usize).checked_add(main_load_bias)?;
        let main_entry = final_entry;
        let mut saw_interp = false;
        let mut loaded_interp = false;
        let mut interp_path = None;

        let mut interp_base = None;

        //遍历程序头表，根据类型进行处理
        for i in 0..ph_count {
            //获取实例化的段
            let ph = elf.program_header(i).unwrap();
            match ph.get_type() {
                //如果是Load段
                Ok(xmas_elf::program::Type::Load) => {
                    let file_size = ph.file_size() as usize;
                    let mem_size = ph.mem_size() as usize;
                    let file_offset = ph.offset() as usize;
                    let virtual_addr = ph.virtual_addr() as usize;
                    if file_size > mem_size
                        || file_offset
                            .checked_add(file_size)
                            .map_or(true, |end| end > elf.input.len())
                        || load_align(ph.align()).is_none()
                    {
                        warn!("MemorySet::from_elf: invalid PT_LOAD segment");
                        return None;
                    }
                    //获取加载段的虚拟地址范围与权限
                    let start = virtual_addr.checked_add(main_load_bias)?;
                    let end = start.checked_add(mem_size)?;
                    let start_va: VirtAddr = start.into();
                    let end_va: VirtAddr = end.into();
                    let mut map_perm = MapPermission::U;
                    let ph_flags = ph.flags();
                    if ph_flags.is_read() {
                        map_perm |= MapPermission::R;
                    }
                    if ph_flags.is_write() {
                        map_perm |= MapPermission::W;
                    }
                    if ph_flags.is_execute() {
                        map_perm |= MapPermission::X;
                    }

                    //新建逻辑段实例并更新结尾的最大虚拟页号
                    let map_area = MapArea::new(
                        start_va,
                        end_va,
                        MapType::Framed,
                        map_perm,
                        PageSize::Page4K,
                    );
                    let area_end_vpn = map_area.vpn_range.get_end();
                    if area_end_vpn > max_end_vpn {
                        max_end_vpn = area_end_vpn;
                    }
                    if area_end_vpn > main_max_end_vpn {
                        main_max_end_vpn = area_end_vpn;
                    }
                    //将文件数据复制到逻辑段中
                    memory_set.push(
                        map_area,
                        Some(&elf.input[file_offset..file_offset + file_size]),
                        start,
                    );
                    if ph.offset() <= elf_header.pt2.ph_offset()
                        && elf_header.pt2.ph_offset() < ph.offset() + ph.file_size()
                    {
                        phdr_addr = (ph.virtual_addr() + (elf_header.pt2.ph_offset() - ph.offset()))
                            as usize
                            + main_load_bias;
                    }
                }
                Ok(xmas_elf::program::Type::Interp) => {
                    saw_interp = true;
                    let offset = ph.offset() as usize;
                    let size = ph.file_size() as usize;
                    interp_path = Some(String::from(
                        core::str::from_utf8(&elf.input[offset..offset + size])
                            .unwrap_or("")
                            .trim_end_matches('\0'),
                    ));
                }
                // PHDR 可用于更稳定地提供 AT_PHDR。
                Ok(xmas_elf::program::Type::Phdr) => {
                    if phdr_addr == 0 {
                        phdr_addr = ph.virtual_addr() as usize + main_load_bias;
                    }
                }
                // 这些段通常由 LOAD 段覆盖或仅提供元信息，不需要单独映射。
                Ok(_other) => {
                    trace!("ignoring ELF program header of type {:?}", _other);
                    // ignored intentionally
                }
                // 架构私有段（例如 RISCV_ATTRIBUTES）会走这里，安全忽略即可。
                Err(_raw_type) => {
                    trace!("ignoring ELF program header of unknown type");
                    // ignored intentionally
                }
            }
        }
        if let Some(interp_path) = interp_path.as_deref() {
            if let Some(interp_data) = load_interp(interp_path) {
                let interp_elf = xmas_elf::ElfFile::new(interp_data.as_slice()).unwrap();
                const GUARD_PAGES: usize = 10;
                let interp_runtime_base = main_max_end_vpn.0 * PAGE_SIZE + GUARD_PAGES * PAGE_SIZE;
                memory_set.push_guard_area(main_max_end_vpn.0 * PAGE_SIZE, GUARD_PAGES);
                let Some(interp_load_align) =
                    interp_elf
                    .program_iter()
                        .try_fold(PAGE_SIZE, |max_align, segment| {
                            if matches!(segment.get_type(), Ok(xmas_elf::program::Type::Load)) {
                                Some(max_align.max(load_align(segment.align())?))
                        } else {
                                Some(max_align)
                        }
                    })
                else {
                    warn!("MemorySet::from_elf: invalid interpreter PT_LOAD alignment");
                    return None;
                };
                let interp_load_bias = align_up(interp_runtime_base, interp_load_align)?;
                interp_base = Some(interp_load_bias);
                final_entry =
                    interp_load_bias.checked_add(interp_elf.header.pt2.entry_point() as usize)?;
                loaded_interp = true;
                info!(
                    "MemorySet::from_elf: PT_INTERP loaded '{}', entry switched {:#x} -> {:#x}",
                    interp_path, main_entry, final_entry
                );
                for interp_ph in interp_elf.program_iter() {
                    if let Ok(xmas_elf::program::Type::Load) = interp_ph.get_type() {
                        let file_size = interp_ph.file_size() as usize;
                        let mem_size = interp_ph.mem_size() as usize;
                        let file_offset = interp_ph.offset() as usize;
                        let virtual_addr = interp_ph.virtual_addr() as usize;
                        if file_size > mem_size
                            || file_offset
                                .checked_add(file_size)
                                .map_or(true, |end| end > interp_data.len())
                            || load_align(interp_ph.align()).is_none()
                        {
                            warn!("MemorySet::from_elf: invalid interpreter PT_LOAD segment");
                            return None;
                        }
                        let start_va = interp_load_bias.checked_add(virtual_addr)?;
                        let end_va = start_va.checked_add(mem_size)?;
                        let mut map_perm = MapPermission::U;
                        let interp_flags = interp_ph.flags();
                        if interp_flags.is_read() {
                            map_perm |= MapPermission::R;
                        }
                        if interp_flags.is_write() {
                            map_perm |= MapPermission::W;
                        }
                        if interp_flags.is_execute() {
                            map_perm |= MapPermission::X;
                        }
                        let data = &interp_data[file_offset..file_offset + file_size];
                        let interp_end_vpn: VirtPageNum = VirtAddr::from(end_va).std_ceil();
                        if interp_end_vpn > max_end_vpn {
                            max_end_vpn = interp_end_vpn;
                        }
                        memory_set.push(
                            MapArea::new(
                                start_va.into(),
                                end_va.into(),
                                MapType::Framed,
                                map_perm,
                                PageSize::Page4K,
                            ),
                            Some(data),
                            start_va,
                        );
                    }
                }
            } else {
                warn!(
                    "MemorySet::from_elf: PT_INTERP present but loader '{}' not found; fallback to main entry",
                    interp_path
                );
            }
        }
        if saw_interp && !loaded_interp {
            info!(
                "MemorySet::from_elf: PT_INTERP present but unresolved, using main entry 0x{:x}",
                final_entry
            );
        }
        if !saw_interp {
            info!(
                "MemorySet::from_elf: no PT_INTERP, using main entry 0x{:x}",
                final_entry
            );
        }
        debug!("MemorySet::from_elf: mapped common areas");
        // 按顺序布局：主程序/解释器段 -> guard -> 用户栈 -> guard -> 堆。
        const GUARD_PAGES: usize = 10;
        let user_stack_bottom = max_end_vpn.0 * PAGE_SIZE + GUARD_PAGES * PAGE_SIZE;
        memory_set.push_guard_area(max_end_vpn.0 * PAGE_SIZE, GUARD_PAGES);
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;
        memory_set.areas.push(MapArea::new(
            user_stack_bottom.into(),
            user_stack_top.into(),
            MapType::Framed,
            MapPermission::R | MapPermission::W | MapPermission::U,
            PageSize::Page4K,
        ));
        let heap_bottom = user_stack_top + GUARD_PAGES * PAGE_SIZE;
        memory_set.push_guard_area(user_stack_top, GUARD_PAGES);
        let heap_bottom_vpn = VirtAddr::from(heap_bottom).std_ceil();
        memory_set.push(
            MapArea::new(
                heap_bottom_vpn.into(),
                heap_bottom_vpn.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
                PageSize::Page4K,
            ),
            None,
            heap_bottom,
        );
        // 初始化堆起点与 program break（brk 从堆底开始，可随后用 brk/sbrk 增长）
        memory_set.start_brk = VirtAddr::from(heap_bottom);
        memory_set.brk = VirtAddr::from(heap_bottom);
        // map TrapContext
        // la64下不需要映射
        // 对于riscv，需要在创建进程时再映射
        /*
        #[cfg(target_arch = "riscv64")]
        debug!("MemorySet::from_elf: mapping TrapContext");
        #[cfg(target_arch = "riscv64")]
        memory_set.push(
            MapArea::new(
                TRAP_CONTEXT_BASE.into(),
                TRAMPOLINE.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W,
            ),
            None,
            TRAP_CONTEXT_BASE,
        ); */
        #[cfg(target_arch = "riscv64")]
        memory_set.install_kernel_space();

        debug!("MemorySet::from_elf: mapped all areas");
        Some((
            memory_set,
            heap_bottom,
            user_stack_top,
            final_entry,
            main_entry,
            phdr_addr,
            phnum,
            phent,
            interp_base,
        ))
    }
    /// Create a new address space by copy code&data from a exited process's address space.
    pub fn from_existed_user(user_space: &mut Self) -> Self {
        let mut memory_set = Self::new_bare();
        // 用户态信号恢复跳板
        memory_set.map_user_trampoline();

        // 复制用户拥有的段
        for idx in 0..user_space.areas.len() {
            let map_type = user_space.areas[idx].map_type;
            // 采用共享内核页表之后实际上弃用，暂时保留接口
            if map_type == MapType::BorrowedKernel {
                continue;
            }
            // Guard areas contain metadata only and deliberately have no PTE.
            // Copying one through push() would call map_one(), while a guard
            // mapping is supposed to be a no-op rather than an allocation.
            if map_type == MapType::Guard {
                memory_set
                    .areas
                    .push(MapArea::from_another(&user_space.areas[idx]));
                continue;
            }
            let map_perm = user_space.areas[idx].map_perm;
            let is_shared = user_space.areas[idx].is_shared;
            let page_size = user_space.areas[idx].page_size;
            let vpn_range = VPNRange::new(
                user_space.areas[idx].vpn_range.get_start(),
                user_space.areas[idx].vpn_range.get_end(),
            );
            // 优化：空的 PROT_NONE 区域不需要复制页表，跳过页表访问
            if (!map_perm.contains(MapPermission::U)
                || !map_perm.intersects(
                    MapPermission::R | MapPermission::W | MapPermission::X,
                ))
                && user_space.areas[idx].data_frames.is_empty()
            {
                memory_set
                    .areas
                    .push(MapArea::from_another(&user_space.areas[idx]));
                continue;
            }
            let share_user_pages = matches!(map_type, MapType::Framed | MapType::File);
            if share_user_pages {
                let mut new_area = MapArea::from_another(&user_space.areas[idx]);
                // 只遍历父进程实际已分配的页
                let mapped_vpns: Vec<VirtPageNum> = user_space.areas[idx]
                    .data_frames
                    .keys()
                    .copied()
                    .filter(|vpn| *vpn >= vpn_range.get_start() && *vpn < vpn_range.get_end())
                    .collect();
                for vpn in mapped_vpns {
                    let Some(src_pte) = user_space.page_table.translate(vpn) else {
                        continue;
                    };
                    if !src_pte.is_valid() {
                        continue;
                    }
                    let writable_cow = map_perm.contains(MapPermission::W) && !is_shared;
                    let child_perm = if writable_cow {
                        map_perm & !MapPermission::W
                    } else {
                        map_perm
                    };
                    let child_flags = PTEFlags::from_bits(child_perm.bits).unwrap();
                    memory_set
                        .page_table
                        .map(vpn, src_pte.ppn(), child_flags, page_size);
                    let frame_tracker = user_space.areas[idx]
                        .data_frames
                        .get(&vpn)
                        .cloned()
                        .unwrap();
                    new_area.data_frames.insert(vpn, frame_tracker);
                    if writable_cow {
                        let parent_perm = map_perm & !MapPermission::W;
                        let parent_flags = PTEFlags::from_bits(parent_perm.bits).unwrap();
                        user_space
                            .page_table
                            .set_flags(vpn, parent_flags, page_size);
                    }
                }
                memory_set.areas.push(new_area);
            } else {
                let new_area: MapArea = MapArea::from_another(&user_space.areas[idx]);
                let start_va: VirtAddr = new_area.vpn_range.get_start().into();
                memory_set.push(new_area, None, start_va.0);
                let step = page_size.num_pages();
                let mut vpn = vpn_range.get_start();
                while vpn < vpn_range.get_end() {
                    if let Some(src_pte) = user_space.translate(vpn) {
                        if src_pte.is_valid() {
                            let src_ppn = src_pte.ppn();
                            if memory_set.translate(vpn).is_none()
                                || !memory_set.translate(vpn).unwrap().is_valid()
                            {
                                memory_set.page_table.translate_create(vpn, page_size);
                            }
                            let dst_ppn = memory_set.translate(vpn).unwrap().ppn();
                            // 按实际页大小拷贝全部数据（大页需拷贝多个基本页）
                            let num_pages = page_size.num_pages();
                            for i in 0..num_pages {
                                PhysPageNum(dst_ppn.0 + i)
                                    .get_bytes_array()
                                    .copy_from_slice(PhysPageNum(src_ppn.0 + i).get_bytes_array());
                            }
                        }
                    }
                    vpn.step_by(step);
                }
            }
        }
        // 复制 brk 与堆起点
        memory_set.brk = user_space.brk;
        memory_set.start_brk = user_space.start_brk;
        #[cfg(target_arch = "riscv64")]
        memory_set.install_kernel_space();
        memory_set
    }
    pub fn handle_cow_fault(&mut self, bad_addr: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();
        let page_table = &mut self.page_table;
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|a| vpn >= a.vpn_range.get_start() && vpn < a.vpn_range.get_end())
        {
            if area.map_type == MapType::Guard
                || area.is_shared
                || !area.map_perm.contains(MapPermission::W)
            {
                return false;
            }
            if let Some(pte) = page_table.translate(vpn) {
                if pte.is_valid() && !pte.writable() {
                    let old_ppn = pte.ppn();
                    let new_frame = frame_alloc(area.page_size).unwrap();
                    let new_ppn = new_frame.ppn;
                    // 按实际页大小拷贝全部数据（大页需拷贝多个基本页）
                    let num_pages = area.page_size.num_pages();
                    for i in 0..num_pages {
                        PhysPageNum(new_ppn.0 + i)
                            .get_bytes_array()
                            .copy_from_slice(PhysPageNum(old_ppn.0 + i).get_bytes_array());
                    }
                    let pte_flags = PTEFlags::from_bits(area.map_perm.bits).unwrap();
                    page_table.set_entry(vpn, new_ppn, pte_flags);
                    let old_frame = area.data_frames.insert(vpn, new_frame);
                    #[cfg(target_arch = "loongarch64")]
                    Self::flush_tlb_after_mapping_change();
                    #[cfg(target_arch = "riscv64")]
                    self.flush_tlb_targets();
                    drop(old_frame);
                    return true;
                }
            }
        }
        false
    }
    //用于内核态给用户空间写入数据，判断是否是copy页时使用
    pub fn ensure_writable_user_range(&mut self, start: usize, len: usize, sp: usize) -> bool {
        if len == 0 {
            return true;
        }
        let page_size = if let Some(page_size) = self.areas.iter().find_map(|area| {
            if VirtAddr::from(start) >= area.vpn_range.get_start().into()
                && VirtAddr::from(start) < area.vpn_range.get_end().into()
            {
                Some(area.page_size)
            } else {
                None
            }
        }) {
            page_size
        } else {
            return false;
        };
        let mut vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(start + len - 1).std_floor();
        loop {
            let page_start = vpn.0 * PAGE_SIZE;
            match self.page_table.translate(vpn) {
                Some(pte) if pte.is_valid() && pte.writable() => {}
                Some(pte) if pte.is_valid() => {
                    if !self.handle_cow_fault(page_start) {
                        return false;
                    }
                }
                _ => {
                    if !self.handle_page_fault(page_start, sp) {
                        return false;
                    }
                    match self.page_table.translate(vpn) {
                        Some(pte) if pte.is_valid() && pte.writable() => {}
                        Some(pte) if pte.is_valid() => {
                            if !self.handle_cow_fault(page_start) {
                                return false;
                            }
                        }
                        _ => return false,
                    }
                }
            }
            if vpn == end_vpn {
                break;
            }
            vpn.step_by(page_size.num_pages());
        }
        true
    }
    pub fn ensure_readable_user_range(&mut self, start: usize, len: usize, sp: usize) -> bool {
        if len == 0 {
            return true;
        }
        let page_size = if let Some(page_size) = self.areas.iter().find_map(|area| {
            if VirtAddr::from(start) >= area.vpn_range.get_start().into()
                && VirtAddr::from(start) < area.vpn_range.get_end().into()
            {
                Some(area.page_size)
            } else {
                None
            }
        }) {
            page_size
        } else {
            return false;
        };

        let mut vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(start + len - 1).std_floor();
        loop {
            let page_start = vpn.0 * PAGE_SIZE;
            match self.page_table.translate(vpn) {
                Some(pte) if pte.is_valid() && pte.readable() => {}
                Some(pte) if pte.is_valid() => return false,
                _ => {
                    if !self.handle_page_fault(page_start, sp) {
                        return false;
                    }
                    match self.page_table.translate(vpn) {
                        Some(pte) if pte.is_valid() && pte.readable() => {}
                        _ => return false,
                    }
                }
            }
            if vpn == end_vpn {
                break;
            }
            vpn.step_by(page_size.num_pages());
        }
        true
    }
    /// Change page table by writing satp CSR Register.
    #[cfg(target_arch = "riscv64")]

    pub fn activate(&self) {
        let token = self.token();

        unsafe {
            core::arch::asm!(
                "csrw satp, {token}",
                "sfence.vma x0, x0",
                token = in(reg) token,
                options(nostack)
            );
        }
    }

    /// 对于龙芯，修改PGDL/H寄器
    /// 用户处于低半地址空间
    #[cfg(target_arch = "loongarch64")]
    pub fn activate(&self) {
        let pgdl = self.page_table.token();
        unsafe {
            asm!("csrwr {pgdl}, 0x19", pgdl = inout(reg) pgdl => _);
            asm!("dbar 0");
        }
    }

    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        let page_size = self.areas.iter().find_map(|area| {
            if vpn >= area.vpn_range.get_start() && vpn < area.vpn_range.get_end() {
                Some(area.page_size)
            } else {
                None
            }
        })?;
        self.page_table.translate(vpn)
    }
    pub fn translate_create(
        &mut self,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> Option<PageTableEntry> {
        self.page_table.translate_create(vpn, page_size)
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_pte_dirty(&mut self, vpn: VirtPageNum) -> bool {
        if let Some((pte, _)) = self.page_table.find_pte(vpn) {
            pte.set_dirty();
            Self::flush_tlb_after_mapping_change();
            true
        } else {
            false
        }
    }
    #[cfg(target_arch = "riscv64")]
    pub fn set_pte_dirty(&mut self, vpn: VirtPageNum) -> bool {
        if let Some((pte, _)) = self.page_table.find_pte(vpn) {
            let flags = pte.flags();
            if pte.is_valid() && pte.writable() && !flags.contains(PTEFlags::D) {
                pte.set_dirty();
                self.flush_tlb_targets();
                return true;
            }
        }
        false
    }
    /// 写回共享映射页面内容
    pub fn sync_shared_pages(&mut self) {
        for area in self.areas.iter_mut() {
            area.sync_back_to_file();
        }
    }
    /// Remove all `MapArea`
    pub fn recycle_data_pages(&mut self) {
        let mut frames = Vec::new();
        for area in self.areas.iter_mut() {
            frames.extend(area.unmap(&mut self.page_table));
        }
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(frames);
        self.areas.clear();
    }
    /// shrink the area to new_end
    #[allow(unused)]
    pub fn shrink_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.std_floor())
        {
            let frames = area.shrink_to(&mut self.page_table, new_end.std_ceil());
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            #[cfg(target_arch = "riscv64")]
            self.flush_tlb_targets();
            drop(frames);
            true
        } else {
            false
        }
    }

    /// append the area to new_end
    #[allow(unused)]
    pub fn append_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        //找到对应逻辑段
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.std_floor())
        {
            area.append_to(&mut self.page_table, new_end.std_ceil());
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            true
        } else {
            false
        }
    }

    /// 检查目标地址段是否与已有的映射冲突(存在交集)
    fn has_conflict(&self, start: usize, len: usize) -> bool {
        for area in self.areas.iter() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            let target_start = VirtAddr::from(start).std_floor();
            let target_end = VirtAddr::from(start + len).std_ceil();
            if target_end > area_start && target_start < area_end {
                return true;
            }
        }
        false
    }
    /// mmap 实现
    /// 
    /// 当前实现下所有映射都为懒分配，
    /// 首次访问时由 handle_page_fault 分配物理页并映射。
    /// 目前暂时固定用 4KB 页。
    pub fn mmap(
        &mut self,
        addr: usize,
        length: usize,
        prot: mmap::MMapProt,
        mmap_flags: mmap::MMapFlags,
        file_inner: Option<Arc<dyn File + Send + Sync>>,
        offset: usize,
    ) -> Result<usize, isize> {
        // 最少分配一页，似乎没必要，暂时注释
        // let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        // 字节偏移转页偏移
        let page_offset = offset / PAGE_SIZE;

        // 计算需要的物理页数
        let needing_std_pages = VirtAddr::from(addr + length).std_ceil().0 - VirtAddr::from(addr).std_floor().0;
        let is_anonymous = mmap_flags.contains(mmap::MMapFlags::MAP_ANONYMOUS);
        let is_shared = mmap_flags.contains(mmap::MMapFlags::MAP_SHARED);
        let free_std_pages = get_free_frames();
        info!("mapping memory: addr={:#x}, length={:#x}, prot={:?}, flags={:?}, free_std_pages={}, needing_std_pages={}", 
            addr, length, prot, mmap_flags, free_std_pages, needing_std_pages);

        // 找合适起始地址
        let mut start_va = addr;
        if start_va == 0 {
            if let Some(new_addr) = self.find_free_area(length) {
                //println!("[kernel] mmap: found free area at 0x{:x} for length 0x{:x}", new_addr, length);
                start_va = new_addr;
            } else {
                error!(
                    "mmap failed: no suitable free area found for length {:#x}",
                    length
                );
                return Err(Errno::EEXIST.as_isize());
            }
        } else {
            // 检查冲突
            if self.has_conflict(start_va, length) {
                if mmap_flags.contains(mmap::MMapFlags::MAP_FIXED) {
                    if let Ok(_ret) = self.munmap(start_va, length) {
                        // Handle the result if needed
                        //println!("[kernel] mmap: MAP_FIXED flag set, unmapped conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                    } else {
                        //println!("[kernel] mmap: MAP_FIXED flag set, but failed to unmap conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                        return Err(Errno::EEXIST.as_isize());
                    }
                } else {
                    //println!("[kernel] mmap failed: address range [0x{:x}, 0x{:x}) conflicts with existing mapping", start_va, start_va + length);
                    return Err(Errno::EEXIST.as_isize());
                }
            }
        }

        // 设置权限
        let mut permission = MapPermission::empty();
        if prot.contains(mmap::MMapProt::PROT_READ) {
            permission |= MapPermission::R;
        }
        if prot.contains(mmap::MMapProt::PROT_WRITE) {
            permission |= MapPermission::W;
        }
        if prot.contains(mmap::MMapProt::PROT_EXEC) {
            permission |= MapPermission::X;
        }
        if prot != mmap::MMapProt::PROT_NONE {
            permission |= MapPermission::U;
        }

        if is_anonymous {
            // 匿名 mmap 只创建区域元数据。首次访问由 handle_page_fault
            // 分配并清零一个物理页，不再在这里遍历整个线程栈。
            let mut area = MapArea::new(
                VirtAddr::from(start_va),
                VirtAddr::from(start_va + length),
                MapType::Framed,
                permission,
                PageSize::Page4K,
            );
            area.is_shared = is_shared;
            if is_shared {
                area.anonymous_shared_frames = Some(Arc::new(MPSafeCell::new(BTreeMap::new())));
            }
            self.areas.push(area);
        } else {
            let file = file_inner.ok_or_else(|| Errno::EBADF.as_isize())?;
            self.insert_file_area(
                VirtAddr::from(start_va),
                VirtAddr::from(start_va + length),
                permission,
                PageSize::Page4K,
                file,
                page_offset,
                is_shared,
            );
        }

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();

        Ok(start_va)
    }

    /// 在当前地址空间中寻找一个长度为 length 的空闲连续区域
    /// 找的是逻辑区域，与实际物理页无关
    pub fn find_free_area(&self, length: usize) -> Option<usize> {
        //println!("[kernel] find_free_area: finding free area for length 0x{:x}", length);
        // 将长度向上对齐到页，似乎没必要
        // let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let mut current_addr: usize = USER_APP_BASE + (USER_APP_MAX_SIZE - USER_APP_BASE) / 2;
        // 为高地址用户栈预留顶部空间，避免 mmap 和初始栈冲突。
        // loongarch的栈位置固定，所以上限可以直接计算
        #[cfg(target_arch = "loongarch64")]
        let limit_addr: usize = USER_STACK_TOP - USER_STACK_SIZE;
        #[cfg(target_arch = "riscv64")]
        let limit_addr: usize = USER_APP_MAX_SIZE;

        // 获取按起始虚拟页号排序后的区域列表
        let mut sorted_areas: Vec<_> = self.areas.iter().collect();
        sorted_areas.sort_by_key(|a| a.vpn_range.get_start());

        /*for _area in sorted_areas.iter() {
            println!(
                "[kernel] find_free_area: existing area [0x{:x}, 0x{:x})",
                area.vpn_range.get_start().0 * PAGE_SIZE,
                area.vpn_range.get_end().0 * PAGE_SIZE
            );
        }*/

        for area in sorted_areas {
            let area_start: usize = area.vpn_range.get_start().0 * PAGE_SIZE;
            if current_addr + length <= area_start {
                return Some(current_addr);
            }

            // 否则，将探测点更新为当前区域的结束地址
            let area_end: usize = area.vpn_range.get_end().0 * PAGE_SIZE;
            if area_end > current_addr {
                current_addr = area_end;
            }
        }

        if current_addr + length < limit_addr {
            Some(current_addr)
        } else {
            None
        }
    }
    /// munmap 实现
    /// 现在的实现允许取消映射包括堆区等，堆区现由单独字段维护
    pub fn munmap(&mut self, start: usize, length: usize) -> Result<(), isize> {
        let end = start + length;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        // 收集因从中间截断而产生的新右半部分区域
        let mut new_areas: Vec<MapArea> = Vec::new();
        let mut released_frames = Vec::new();

        for area in self.areas.iter_mut() {
            let a_start = area.vpn_range.get_start();
            let a_end = area.vpn_range.get_end();

            // 检查是否有交集
            if a_start < end_vpn && a_end > start_vpn {
                let delete_left = a_start >= start_vpn; // 目标区域覆盖了当前块的左侧
                let delete_right = a_end <= end_vpn; // 目标区域覆盖了当前块的右侧

                let step = area.page_size.num_pages();
                if delete_left && delete_right {
                    // 情况1：All（当前块被目标区域完全包裹，全部删掉）
                    let mut vpn = a_start;
                    while vpn < a_end {
                        if let Some(frame) = area.unmap_one(&mut self.page_table, vpn) {
                            released_frames.push(frame);
                        }
                        vpn.step_by(step);
                    }
                    area.resize(a_start, a_start); // 长度设为0，稍后统一 retain 清理
                } else if !delete_left && !delete_right {
                    // panic!("munmap: test : split area in the middle");
                    // 情况2：Split（目标区域在当前块中间，一分为二）
                    // 2.1 清理中间被 unmap 的页表和物理页
                    for vpn in VPNRange::new(start_vpn, end_vpn) {
                        if let Some(frame) = area.unmap_one(&mut self.page_table, vpn) {
                            released_frames.push(frame);
                        }
                    }
                    // 2.2 切出右半部分保留的物理帧
                    let right_frames = area.data_frames.split_off(&end_vpn);
                    // 2.3 缩短当前块，作为左半部分
                    area.resize(a_start, start_vpn);
                    // 2.4 新建右半部分
                    let mut right_area = area.clone_meta_with_new_range(end_vpn, a_end);
                    right_area.data_frames = right_frames;
                    new_areas.push(right_area);
                } else if delete_left {
                    // 情况3：Inc_Left（删掉左边部分）
                    let mut vpn = a_start;
                    while vpn < end_vpn {
                        if let Some(frame) = area.unmap_one(&mut self.page_table, vpn) {
                            released_frames.push(frame);
                        }
                        vpn.step_by(step);
                    }
                    area.resize(end_vpn, a_end);
                } else if delete_right {
                    // 情况4：Inc_Right（删掉右边部分）
                    let mut vpn = start_vpn;
                    while vpn < a_end {
                        if let Some(frame) = area.unmap_one(&mut self.page_table, vpn) {
                            released_frames.push(frame);
                        }
                        vpn.step_by(step);
                    }
                    area.resize(a_start, start_vpn);
                }
            }
        }

        // 插入劈开产生的新区域
        self.areas.extend(new_areas);

        // 删除长度为0的区域
        self.areas
            .retain(|area| area.vpn_range.get_start() < area.vpn_range.get_end());

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(released_frames);

        Ok(())
    }

    /// 丢弃驻留页但保留虚拟映射。匿名页重新分配为零页，私有文件页重新
    /// 从文件读取，共享文件页则在下次访问时重新映射对应的 page cache。
    pub fn madvise_dontneed(&mut self, start: usize, length: usize) -> Result<(), isize> {
        let end = start
            .checked_add(length)
            .ok_or_else(|| Errno::EINVAL.as_isize())?;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        let mut released_frames = Vec::new();
        for area in self.areas.iter_mut() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            let discard_start = core::cmp::max(start_vpn, area_start);
            let discard_end = core::cmp::min(end_vpn, area_end);
            if discard_start >= discard_end || area.map_type == MapType::Guard {
                continue;
            }

            let step = area.page_size.num_pages();
            let mut vpn = discard_start;
            while vpn < discard_end {
                if let Some(frame) = area.unmap_one(&mut self.page_table, vpn) {
                    released_frames.push(frame);
                }
                vpn.step_by(step);
            }
        }

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(released_frames);

        Ok(())
    }

    /// mremap 的就地扩容（仅匿名映射）
    ///
    /// 调用方保证 old_addr/new_size/old_size 均已按页对齐、new_size > old_size。
    /// 仅当 old 区域是匿名映射且扩展区间无冲突时才能原地扩大；
    /// 其余情况返回 ENOMEM，由 sys_mremap 回退到“新映射 + 拷贝 + 解除旧映射”。
    pub fn mremap_inplace(
        &mut self,
        old_addr: usize,
        old_size: usize,
        new_size: usize,
    ) -> Result<usize, isize> {
        let old_start_vpn = VirtAddr::from(old_addr).std_floor();
        let old_end_vpn = VirtAddr::from(old_addr + old_size).std_ceil();
        let new_end_vpn = VirtAddr::from(old_addr + new_size).std_ceil();

        let idx = self
            .areas
            .iter()
            .position(|area| {
                area.vpn_range.get_start().0 * PAGE_SIZE <= old_addr
                    && old_addr + old_size <= area.vpn_range.get_end().0 * PAGE_SIZE
                    && area.map_type == MapType::Framed
                    && area.map_type != MapType::Guard
            })
            .ok_or(Errno::ENOMEM.as_isize())?;

        // 扩展区间必须空闲，否则无法原地扩大
        if self.has_conflict(old_addr + old_size, new_size - old_size) {
            return Err(Errno::ENOMEM.as_isize());
        }

        self.areas[idx].resize(old_start_vpn, new_end_vpn);
        Ok(old_addr)
    }

    fn split_area_at(
        &mut self,
        idx: usize,
        split_vpn: VirtPageNum,
    ) -> Result<Option<usize>, isize> {
        let area_start = self.areas[idx].vpn_range.get_start();
        let area_end = self.areas[idx].vpn_range.get_end();
        if split_vpn <= area_start || split_vpn >= area_end {
            return Ok(None);
        }

        let step = self.areas[idx].page_size.num_pages();
        if split_vpn.0 % step != 0 {
            return Err(Errno::EINVAL.as_isize());
        }

        let right_frames = self.areas[idx].data_frames.split_off(&split_vpn);
        let mut right_area = self.areas[idx].clone_meta_with_new_range(split_vpn, area_end);
        right_area.data_frames = right_frames;
        self.areas[idx].resize(area_start, split_vpn);
        self.areas.insert(idx + 1, right_area);
        Ok(Some(idx + 1))
    }

    pub fn mprotect(
        &mut self,
        start: usize,
        length: usize,
        prot: mmap::MMapProt,
    ) -> Result<(), isize> {
        if length == 0 {
            return Ok(());
        }

        let end = start
            .checked_add(length)
            .ok_or_else(|| Errno::EINVAL.as_isize())?;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        let mut permission = MapPermission::empty();
        if prot.contains(mmap::MMapProt::PROT_READ) {
            permission |= MapPermission::R;
        }
        if prot.contains(mmap::MMapProt::PROT_WRITE) {
            permission |= MapPermission::W;
        }
        if prot.contains(mmap::MMapProt::PROT_EXEC) {
            permission |= MapPermission::X;
        }
        if prot != mmap::MMapProt::PROT_NONE {
            permission |= MapPermission::U;
        }

        let mut covered_until = start_vpn;
        let mut affected: Vec<usize> = self
            .areas
            .iter()
            .enumerate()
            .filter_map(|(idx, area)| {
                if area.vpn_range.get_start() < end_vpn && area.vpn_range.get_end() > start_vpn {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        affected.sort_by_key(|&idx| self.areas[idx].vpn_range.get_start().0);

        if affected.is_empty() {
            return Err(Errno::ENOMEM.as_isize());
        }

        for &idx in affected.iter() {
            let area = &self.areas[idx];
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area.map_type == MapType::Guard || area_start > covered_until {
                return Err(Errno::ENOMEM.as_isize());
            }
            if permission.contains(MapPermission::W) && area.is_shared {
                if let Some((file, _)) = &area.backing_file {
                    if !file.writable() {
                        return Err(Errno::EACCES.as_isize());
                    }
                }
            }
            if area.page_size != Page4K {
                let step = area.page_size.num_pages();
                if start_vpn.0 % step != 0 || end_vpn.0 % step != 0 {
                    return Err(Errno::EINVAL.as_isize());
                }
            }
            if area_end > covered_until {
                covered_until = area_end;
            }
        }

        if covered_until < end_vpn {
            return Err(Errno::ENOMEM.as_isize());
        }

        let permissions_tightened = affected.iter().any(|idx| {
            !(self.areas[*idx].map_perm & !permission).is_empty()
        });

        for &idx in affected.iter().rev() {
            self.split_area_at(idx, end_vpn)?;
            self.split_area_at(idx, start_vpn)?;
        }

        let pte_flags = PTEFlags::from_bits(permission.bits).unwrap();
        for area in self.areas.iter_mut() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area_start >= start_vpn && area_end <= end_vpn {
                area.map_perm = permission;
                let step = area.page_size.num_pages();
                let mut vpn = area_start;
                while vpn < area_end {
                    if let Some(pte) = self.page_table.translate(vpn) {
                        if pte.is_valid() {
                            self.page_table.set_flags(vpn, pte_flags, area.page_size);
                        }
                    }
                    vpn.step_by(step);
                }
            }
        }

        #[cfg(target_arch = "riscv64")]
        if permissions_tightened {
            self.flush_tlb_targets();
        }
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();

        Ok(())
    }

    pub fn disable_share_in_range(&mut self, start: usize, length: usize) -> Result<(), isize> {
        if length == 0 {
            return Ok(());
        }

        let end = start
            .checked_add(length)
            .ok_or_else(|| Errno::EINVAL.as_isize())?;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        let mut covered_until = start_vpn;
        let mut affected: Vec<usize> = self
            .areas
            .iter()
            .enumerate()
            .filter_map(|(idx, area)| {
                if area.vpn_range.get_start() < end_vpn && area.vpn_range.get_end() > start_vpn {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        affected.sort_by_key(|&idx| self.areas[idx].vpn_range.get_start().0);

        if affected.is_empty() {
            return Err(Errno::ENOMEM.as_isize());
        }

        for &idx in affected.iter() {
            let area = &self.areas[idx];
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area.map_type == MapType::Guard || area_start > covered_until {
                return Err(Errno::ENOMEM.as_isize());
            }
            if area.page_size != Page4K {
                let step = area.page_size.num_pages();
                if start_vpn.0 % step != 0 || end_vpn.0 % step != 0 {
                    return Err(Errno::EINVAL.as_isize());
                }
            }
            if area_end > covered_until {
                covered_until = area_end;
            }
        }

        if covered_until < end_vpn {
            return Err(Errno::ENOMEM.as_isize());
        }

        for &idx in affected.iter().rev() {
            self.split_area_at(idx, end_vpn)?;
            self.split_area_at(idx, start_vpn)?;
        }

        for area in self.areas.iter_mut() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area_start >= start_vpn && area_end <= end_vpn {
                area.is_shared = false;
            }
        }

        Ok(())
    }

    /// 处理缺页异常。如果触发异常的地址在合法区域内，则为其分配物理页；否则返回 false。
    /// 待进一步完善&测试
    #[no_mangle]
    #[inline(never)]
    pub fn handle_page_fault(&mut self, bad_addr: usize, sp: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();
        let page_table = &mut self.page_table;

        let mut page_size_opt = None;

        // 遍历寻找包含该虚拟页号的段
        if let Some(area) = self.areas.iter_mut().find(|a| a.contains(vpn)) {
            if area.map_type == MapType::Guard {
                return false;
            }

            // PROT_NONE 区域只保留地址空间，任何访问都必须失败，不能
            // 因缺页而无意义地消耗一个物理页。
            if !area.map_perm.contains(MapPermission::U)
                || !area
                    .map_perm
                    .intersects(MapPermission::R | MapPermission::W | MapPermission::X)
            {
                return false;
            }

            page_size_opt = Some(area.page_size);

            // 检查该页是否已经在页表中映射
            if let Some(pte) = page_table.translate(vpn) {
                if pte.is_valid() {
                    // 已经映射却还报 Fault，通常是非法写只读段
                    return false;
                }
            }

            // 按映射类型惰性分配物理页或映射文件页缓存
            if !area.try_map_one(page_table, vpn, page_size_opt.unwrap()) {
                return false;
            }
            
            #[cfg(target_arch = "loongarch64")]
            {
                Self::flush_tlb_after_mapping_change();
                unsafe { asm!("ibar 0") };
            }
            return true; // 惰性分配修复成功
        }

        // 动态扩张用户栈
        let sp_vpn = VirtAddr::from(sp).std_floor();

        // 设定一个栈最大允许单次/总共扩张的大小，防止恶意程序耗尽内存
        const MAX_EXPAND_PAGES: usize = 32;

        let mut expand_idx = None;
        for (idx, area) in self.areas.iter().enumerate() {
            if area.map_type == MapType::Guard {
                continue;
            }
            let start_vpn = area.vpn_range.get_start();

            // 检查缺页地址(vpn)是否紧贴着该段的下方，且距离不超过限制
            if vpn < start_vpn && (start_vpn.0 - vpn.0) <= MAX_EXPAND_PAGES {
                // 进一步确认：缺页确实发生在栈指针(sp)附近，这证明是正常的压栈行为
                if vpn.0 <= sp_vpn.0 && (sp_vpn.0 - vpn.0) <= MAX_EXPAND_PAGES {
                    expand_idx = Some((idx, start_vpn));
                    break;
                }
            }
        }

        if let Some((idx, old_start_vpn)) = expand_idx {
            // 从被扩张的 area 获取 page_size
            page_size_opt = Some(self.areas[idx].page_size);
            // 执行扩张：将该 Area 的起点向下延伸到 vpn (注意保留原来的终点)
            self.areas[idx].vpn_range =
                crate::mm::address::VPNRange::new(vpn, self.areas[idx].vpn_range.get_end());

            // 为刚刚扩张出来的这些虚拟页（从 vpn 到 old_start_vpn）全部分配物理帧并映射
            for v in vpn.0..old_start_vpn.0 {
                // 假设你引入了 VirtPageNum
                self.areas[idx].map_one(page_table, VirtPageNum::from(v), page_size_opt.unwrap());
            }

            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            // trace!("[kernel] User stack dynamically expanded down to {:#x}", bad_addr);
            return true; // 栈扩张修复成功！
        }

        // 既不在合法区域，也不符合栈扩张规则，野指针/溢出
        false
    }

    pub fn debug_dump_areas(&self, badv: Option<usize>, era: Option<usize>) {
        let heap_start_vpn = self.start_brk.std_ceil();
        let brk_end_vpn = VirtAddr::from(self.brk.0).std_ceil();
        println!(
            "[kernel] memory_set: asid={}, start_brk=0x{:x}, brk=0x{:x}, area_count={}",
            self.asid.0,
            self.start_brk.0,
            self.brk.0,
            self.areas.len()
        );
        for (idx, area) in self.areas.iter().enumerate() {
            let start = area.vpn_range.get_start().0 * PAGE_SIZE;
            let end = area.vpn_range.get_end().0 * PAGE_SIZE;
            let badv_hit = badv
                .map(|addr| addr >= start && addr < end)
                .unwrap_or(false);
            let era_hit = era.map(|addr| addr >= start && addr < end).unwrap_or(false);
            let is_brk = area.vpn_range.get_start() >= heap_start_vpn
                && area.vpn_range.get_start() < brk_end_vpn;
            println!(
                "[kernel] area[{}] [0x{:x}, 0x{:x}) {:?}{}{}{}",
                idx,
                start,
                end,
                area.map_perm,
                if is_brk { " [brk]" } else { "" },
                if badv_hit { " [BADV]" } else { "" },
                if era_hit { " [ERA]" } else { "" },
            );
        }
    }

    /// 判断已有 PTE 是否已满足本次用户访问所需权限。
    /// 用于并发缺页竞态：另一个 hart 刚完成映射/COW 时，直接重试即可。
    pub fn pte_satisfies(
        &self,
        vpn: VirtPageNum,
        need_read: bool,
        need_write: bool,
        need_exec: bool,
    ) -> bool {
        self.page_table.translate(vpn).map_or(false, |pte| {
            pte.is_valid()
                && pte.user_accessible()
                && (!need_read || pte.readable())
                && (!need_write || pte.writable())
                && (!need_exec || pte.executable())
        })
    }

    /// 检查是否是超出文件大小导致的pagefault，如果是，返回后触发SIGBUS信号
    pub fn check_mmap_page_fault(&self, bad_addr: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();
        // 如果 PTE 已存在且有效，说明页已建立映射，缺页是权限冲突（如写只读页）
        if let Some(pte) = self.page_table.translate(vpn) {
            if pte.is_valid() {
                return false;
            }
        }
        for area in self.areas.iter() {
            // 访问超出文件大小
            if area.map_type == MapType::File
                && area.is_shared
                && area.backing_file.is_some()
                && area.contains(vpn)
            {
                return true;
            }
        }
        false
    }
}

impl Drop for MemorySet {
    fn drop(&mut self) {
        #[cfg(target_arch = "riscv64")]
        {
            let token = self.token();
            self.flush_tlb_targets();
            // 页表帧所有权移交 tlb 层：若仍有核的 satp 指向该页表
            // （空闲核有意保留 warm satp 不切换），帧会延迟到最后一个
            // 核切换离开后才释放，不会因立即回收而破坏其它核的 satp。
            let frames = self.page_table.take_frames();
            crate::mm::tlb::remove_token(token, frames);
        }
    }
}
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    pub vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
    pub is_shared: bool,
    // 记录文件信息和页偏移，其中页偏移的语义为映射起始页在文件中的页偏移量
    pub backing_file: Option<(Arc<dyn File + Send + Sync>, usize)>,
    /// 匿名共享映射登记，懒分配，用于子进程继承父进程的共享匿名映射
    /// 
    /// 每次共享匿名映射缺页先查该表，如果存在则直接克隆 FrameTracker，
    /// 否则分配新的物理页并登记。
    anonymous_shared_frames: Option<Arc<MPSafeCell<BTreeMap<VirtPageNum, FrameTracker>>>>,
    pub page_size: PageSize,
}

impl MapArea {
    /// 默认为匿名非文件映射
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
        page_size: PageSize,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.std_floor();
        let end_vpn: VirtPageNum = end_va.std_ceil();
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
            is_shared: false,
            page_size,
            backing_file: None,
            anonymous_shared_frames: None,
        }
    }
    pub fn get_vpn_range(&self) -> &VPNRange {
        &self.vpn_range
    }
    pub fn from_another(another: &Self) -> Self {
        Self {
            vpn_range: VPNRange::new(another.vpn_range.get_start(), another.vpn_range.get_end()),
            data_frames: BTreeMap::new(),
            map_type: another.map_type,
            map_perm: another.map_perm,
            is_shared: another.is_shared,
            backing_file: another.backing_file.clone(),
            anonymous_shared_frames: another.anonymous_shared_frames.clone(),
            page_size: another.page_size,
        }
    }
    /// Remove one PTE and return its frame without dropping it.
    ///
    /// The caller must invalidate every active TLB before dropping the frame.
    pub fn unmap_one(
        &mut self,
        page_table: &mut PageTable,
        vpn: VirtPageNum,
    ) -> Option<FrameTracker> {
        match self.map_type {
            MapType::Framed | MapType::File => {
                // 共享文件映射需要先写回内容
                if self.is_shared {
                    if let Some((file, base_offset)) = &self.backing_file {
                        let file_page_offset =
                            *base_offset + (vpn.0 - self.vpn_range.get_start().0);
                        let man = &crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER;
                        man.write_back_page_cache(file.ino(), file_page_offset);
                    }
                }

                let frame = self.data_frames.remove(&vpn);
                if frame.is_some() {
                    page_table.unmap(vpn);
                }
                frame
            }
            MapType::Identical | MapType::Windowed => {
                page_table.unmap(vpn);
                None
            }
            // BorrowedKernel contains no FrameTracker; only remove its PTE.
            MapType::BorrowedKernel => {
                if page_table.translate(vpn).is_some() {
                    page_table.unmap(vpn);
                }
                None
            }
            MapType::Guard => None,
        }
    }

    /// 用于 Split 时复制出相同属性的新区域
    pub fn clone_meta_with_new_range(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> Self {
        let backing_file = self.backing_file.as_ref().map(|(file, page_offset)| {
            (
                file.clone(),
                *page_offset + start_vpn.0 - self.vpn_range.get_start().0,
            )
        });
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(), // 初始为空，由外面填充
            map_type: self.map_type,
            map_perm: self.map_perm,
            is_shared: self.is_shared,
            backing_file,
            anonymous_shared_frames: self.anonymous_shared_frames.clone(),
            page_size: self.page_size,
        }
    }
    /// 尝试分配物理页并建立映射，失败返回 false
    /// 为共享文件映射分配页缓存，为匿名映射分配物理页，私有文件映射分配物理页并从文件读取内容
    pub fn try_map_one(
        &mut self,
        page_table: &mut PageTable,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> bool {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Windowed => {
                // `VirtPageNum` is always expressed in 4 KiB pages, including
                // when this PTE is a 2 MiB or 1 GiB leaf.  Remove the window
                // from the byte address before converting it back to a PPN;
                // shifting the window by `page_size` would clear the wrong VPN
                // bit for huge pages and encode a high-half address as a PA.
                let va = usize::from(VirtAddr::from(vpn));
                ppn = PhysAddr::from(va & !CACHED_KERNEL_BASE).std_floor();
            }
            MapType::Framed => {
                if let Some(shared_frames) = &self.anonymous_shared_frames {
                    let mut shared_frames = shared_frames.exclusive_access();
                    if let Some(shared_frame) = shared_frames.get(&vpn) {
                        let frame = shared_frame.clone();
                        ppn = frame.ppn;
                        self.data_frames.insert(vpn, frame);
                    } else {
                        let Some(frame) = frame_alloc(page_size) else {
                            return false;
                        };
                        ppn = frame.ppn;
                        shared_frames.insert(vpn, frame.clone());
                        self.data_frames.insert(vpn, frame);
                    }
                } else {
                    let Some(frame) = frame_alloc(page_size) else {
                        return false;
                    };
                    ppn = frame.ppn;
                    self.data_frames.insert(vpn, frame);
                }
            }
            MapType::File => {
                return if self.is_shared {
                    self.try_map_shared_file_page(page_table, vpn, page_size)
                } else {
                    self.try_map_private_file_pages(page_table, vpn, page_size)
                };
            }
            MapType::BorrowedKernel => {
                return false;
            }
            MapType::Guard => {
                return false;
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        #[cfg(target_arch = "loongarch64")]
        // la64在内核态不需要用页表
        if self.map_type != MapType::Identical && self.map_type != MapType::Windowed {
            page_table.map(vpn, ppn, pte_flags, page_size);
        }
        #[cfg(target_arch = "riscv64")]
        page_table.map(vpn, ppn, pte_flags, page_size);
        true
    }

    fn try_map_shared_file_page(
        &mut self,
        page_table: &mut PageTable,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> bool {
        if page_size != PageSize::Page4K {
            warn!("Shared file mapping only supports 4K pages, but got {:?}", page_size);
            return false;
        }

        // 计算并验证文件页偏移
        let Some((file, base_page_offset)) = &self.backing_file else {
            return false;
        };
        let Some(relative_page) = vpn.0.checked_sub(self.vpn_range.get_start().0) else {
            return false;
        };
        let Some(file_page_offset) = base_page_offset.checked_add(relative_page) else {
            return false;
        };
        let file_end_page_offset = VirtAddr::from(file.get_stat().size as usize).std_ceil().0;
        if file_page_offset >= file_end_page_offset {
            return false;
        }

        let Some(cache) = file.get_shared_page(file_page_offset) else {
            return false;
        };
        let page = cache.lock();
        let frame = page.frame.clone();
        let ppn = frame.ppn;
        drop(page);

        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        page_table.map(vpn, ppn, pte_flags, page_size);
        self.data_frames.insert(vpn, frame);
        true
    }

    fn try_map_private_file_pages(
        &mut self,
        page_table: &mut PageTable,
        fault_vpn: VirtPageNum,
        page_size: PageSize,
    ) -> bool {
        const READ_AHEAD_PAGES: usize = 64;

        let Some((file, base_page_offset)) = &self.backing_file else {
            return false;
        };
        let relative_page = fault_vpn.0 - self.vpn_range.get_start().0;
        let Some(file_offset) = base_page_offset
            .checked_add(relative_page)
            .and_then(|page| page.checked_mul(PAGE_SIZE))
        else {
            return false;
        };

        let available_pages = self.vpn_range.get_end().0 - fault_vpn.0;
        let mut frames: Vec<(VirtPageNum, FrameTracker)> = Vec::new();
        let mut buffers = Vec::new();
        for index in 0..core::cmp::min(READ_AHEAD_PAGES, available_pages) {
            let vpn = VirtPageNum::from(fault_vpn.0 + index);
            if page_table.translate(vpn).is_some_and(|pte| pte.is_valid()) {
                break;
            }
            let Some(frame) = frame_alloc(page_size) else {
                break;
            };
            buffers.push(frame.get_bytes_array());
            frames.push((vpn, frame));
        }
        if frames.is_empty() {
            return false;
        }

        file.read_at(file_offset, UserBuffer::new(buffers));
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        for (vpn, frame) in frames {
            let ppn = frame.ppn;
            #[cfg(target_arch = "loongarch64")]
            page_table.map(vpn, ppn, pte_flags, page_size);
            #[cfg(target_arch = "riscv64")]
            page_table.map(vpn, ppn, pte_flags, page_size);
            self.data_frames.insert(vpn, frame);
        }
        true
    }

    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum, page_size: PageSize) {
        // These area kinds intentionally own no physical frame. Preserve the
        // historical no-op contract for eager metadata walks.
        if matches!(self.map_type, MapType::BorrowedKernel | MapType::Guard) {
            return;
        }
        assert!(
            self.try_map_one(page_table, vpn, page_size),
            "failed to allocate frame while eagerly mapping {:?}",
            vpn,
        );
    }
    pub fn map(&mut self, page_table: &mut PageTable) {
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_start();
        while vpn < self.vpn_range.get_end() {
            self.map_one(page_table, vpn, self.page_size);
            vpn.step_by(step);
        }
    }
    /// 解除映射时返回被解除映射的帧，用于调用者维护 tlb 刷新和页帧释放顺序
    pub fn unmap(&mut self, page_table: &mut PageTable) -> Vec<FrameTracker> {
        let mut frames = Vec::new();
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_start();
        while vpn < self.vpn_range.get_end() {
            if let Some(frame) = self.unmap_one(page_table, vpn) {
                frames.push(frame);
            }
            vpn.step_by(step);
        }
        frames
    }
    #[allow(unused)]
    pub fn shrink_to(
        &mut self,
        page_table: &mut PageTable,
        new_end: VirtPageNum,
    ) -> Vec<FrameTracker> {
        let mut frames = Vec::new();
        let step = self.page_size.num_pages();
        let mut vpn = new_end;
        while vpn < self.vpn_range.get_end() {
            if let Some(frame) = self.unmap_one(page_table, vpn) {
                frames.push(frame);
            }
            vpn.step_by(step);
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
        frames
    }
    #[allow(unused)]
    pub fn append_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_end();
        while vpn < new_end {
            trace!(
                "MapArea::append_to: old vpn end={:#x} , mapping new page vpn={:#x}",
                self.vpn_range.get_end().0,
                vpn.0
            );
            self.map_one(page_table, vpn, self.page_size);
            vpn.step_by(step);
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// 只修改边界，不解除映射或新增映射
    #[allow(unused)]
    pub fn resize(&mut self, new_start: VirtPageNum, new_end: VirtPageNum) {
        let old_start = self.vpn_range.get_start();
        if new_start > old_start {
            if let Some((_, page_offset)) = self.backing_file.as_mut() {
                *page_offset += new_start.0 - old_start.0;
            }
        }
        self.vpn_range = VPNRange::new(new_start, new_end);
    }

    /// data: start-aligned but maybe with shorter length
    /// assume that all frames were cleared before
    pub fn copy_data(&mut self, page_table: &mut PageTable, data: &[u8], start_va: usize) {
        assert_eq!(self.map_type, MapType::Framed);
        let mut data_offset: usize = 0;
        let mut current_vpn = self.vpn_range.get_start();
        let mut page_offset = start_va % PAGE_SIZE;
        let len = data.len();

        while data_offset < len {
            let src_len = (len - data_offset).min(PAGE_SIZE - page_offset);
            let dst = &mut page_table
                .translate(current_vpn)
                .unwrap()
                .ppn()
                .get_bytes_array_with_size(self.page_size)[page_offset..page_offset + src_len];
            dst.copy_from_slice(&data[data_offset..data_offset + src_len]);

            data_offset += src_len;
            page_offset = 0;
            current_vpn.step_by(self.page_size.num_pages());
        }
    }
    /// 返回这段中的数据（如果framed）
    /// u8的vec
    pub fn _get_data(&self, page_table: &mut PageTable) -> Option<Vec<u8>> {
        if self.map_type != MapType::Framed {
            None
        } else {
            let mut data = Vec::new();
            let step = self.page_size.num_pages();
            let mut vpn = self.vpn_range.get_start();
            while vpn < self.vpn_range.get_end() {
                let src = &page_table
                    .translate(vpn)
                    .unwrap()
                    .ppn()
                    .get_bytes_array_with_size(self.page_size);
                data.extend_from_slice(src);
                vpn.step_by(step);
            }
            Some(data)
        }
    }
    pub fn get_map_permission(&self) -> MapPermission {
        self.map_perm
    }
    pub fn contains(&self, vpn: VirtPageNum) -> bool {
        self.vpn_range.contains(vpn)
    }
    /// 将段内数据全部写回文件（如果是共享文件映射）
    pub fn sync_back_to_file(&mut self) {
        if self.is_shared {
            if let Some((file, offset)) = &self.backing_file {
                let ino = file.ino();
                for vpn in self.vpn_range.clone() {
                    let file_page_offset = *offset + (vpn.0 - self.vpn_range.get_start().0);
                    let man = &super::mmap::SHARED_PAGE_CACHE_MANAGER;
                    man.write_back_page_cache(ino, file_page_offset);
                }
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// map type for memory set: identical or framed
pub enum MapType {
    Identical,
    // 匿名映射和普通独占物理页
    Framed,
    // 文件映射，通过 is_shared 区分共享和私有
    File,
    // 共享页表
    BorrowedKernel,
    // 用于内核与用户共享页表，将内核放到高半地址空间
    Windowed,
    Guard,
}

bitflags! {
    /// map permission corresponding to that in pte: `R W X U`
    pub struct MapPermission: u8 {
        ///Readable
        const R = 1 << 1;
        ///Writable
        const W = 1 << 2;
        ///Excutable
        const X = 1 << 3;
        ///Accessible in U mode
        const U = 1 << 4;
    }
}

/// test map function in page table
#[allow(unused)]
pub fn remap_test() {
    let mut kernel_space = KERNEL_SPACE.exclusive_access();
    // 高半地址相加会溢出 usize，用 start + (end - start) / 2 求中点
    let mid_text: VirtAddr = (stext as *const () as usize
        + (etext as *const () as usize - stext as *const () as usize) / 2)
        .into();
    let mid_rodata: VirtAddr = (srodata as *const () as usize
        + (erodata as *const () as usize - srodata as *const () as usize) / 2)
        .into();
    let mid_data: VirtAddr = (sdata as *const () as usize
        + (edata as *const () as usize - sdata as *const () as usize) / 2)
        .into();
    assert!(!kernel_space
        .page_table
        .translate(mid_text.std_floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.std_floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.std_floor())
        .unwrap()
        .executable(),);
    println!("remap_test passed!");
}
