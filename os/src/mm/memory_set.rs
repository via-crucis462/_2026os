use super::{frame_alloc, FrameTracker};
use super::{PageTable, pte::*, PTEFlags};
#[allow(unused)]
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum, PageSize};
use super::{StepByOne, VPNRange};
use super::id::*;
#[allow(unused)]
use crate::arch::config::*;
use crate::arch::trap::current_trap_cx_user_va;
use crate::mm::{get_free_frames, mmap};
use crate::sync::MPSafeCell;
use crate::syscall::errno::Errno;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;
use crate::fs::File;
use crate::mm::PageSize::Page4K;

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
    pub static ref KERNEL_SPACE: Arc<MPSafeCell<MemorySet>> =
        Arc::new(MPSafeCell::new(MemorySet::new_kernel()));
}

/// the kernel token
pub fn kernel_token() -> usize {
    KERNEL_SPACE.exclusive_access().token()
}

/// address space
/// 注意维护brk_index
pub struct MemorySet {
    page_table: PageTable,
    asid: ASIDHandle,
    pub areas: Vec<MapArea>,
    brk_index: usize, //新增，用于记录brk所在area（堆区）的索引，请注意维护，后续可能会删除
}

impl MemorySet {
    #[cfg(target_arch = "loongarch64")]
    fn flush_tlb_after_mapping_change() {
        unsafe {
            // 映射关系发生变化后，失效陈旧 TLB 项。
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
            brk_index: 0,// 注意维护！！
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
        let areas: Vec<MapArea> = parent.areas.iter().map(|a| MapArea::from_another(a)).collect();
        /*for area in &areas {
            println!("shared area: [{:#x}, {:#x}), {:?}", area.vpn_range.get_start().0 * PAGE_SIZE, area.vpn_range.get_end().0 * PAGE_SIZE, area.map_perm);
        }*/
        Self {
            page_table: PageTable::alias_of(&parent.page_table), // 共享根页表，但不拥有中间页帧
            asid: asid_alloc(),            // New ASID for child
            areas,
            brk_index: parent.brk_index,
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
    pub fn areas(&self) -> &Vec<MapArea> {
        &self.areas
    }
    pub fn brk_index(&self) -> usize {
        self.brk_index
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
            MapArea::new(
                start_va, end_va,
                MapType::Framed,
                permission,
                page_size
            ),
            None,
            start_va.into(),
        );
    }
    /// 预留，用于文件映射, 目前只标记，啥都没做
    pub fn insert_file_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        page_size: PageSize,
    ) {
        self.push( // 页的映射发生在push时
            MapArea::new(
                start_va,
                end_va,
                MapType::File,
                permission,
                page_size
            ),
            None,
            start_va.into(),
        );
    }
    /// remove a area
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) {
        if let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .find(|(_, area)| area.vpn_range.get_start() == start_vpn)
        {
            area.unmap(&mut self.page_table);
            self.areas.remove(idx);
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
        trace!("map area: [{:#x}, {:#x}), {:?}", map_area.vpn_range.get_start().0 * PAGE_SIZE, map_area.vpn_range.get_end().0 * PAGE_SIZE, map_area.map_perm);
        self.areas.push(map_area);
    }
    fn push_guard_area(&mut self, guard_start: usize, guard_pages: usize) {
        self.areas.push(MapArea::new(
            guard_start.into(),
            (guard_start + guard_pages * PAGE_SIZE).into(),
            MapType::Guard,
            MapPermission::empty(),
            PageSize::Page4K
        ));
    }
    /// Mention that trampoline is not collected by areas.
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]
    fn map_trampoline(&mut self) {
        info!("mapping trampoline");
        self.page_table.map(
            VirtAddr::from(TRAMPOLINE).into(),
            PhysAddr::from(strampoline as *const () as usize).into(),// 高位0x9...被截断
            PTEFlags::R | PTEFlags::X,
            PageSize::Page4K // 默认标准页大小
        );
    }

    // 用户态使用的跳板页（主要用于信号处理后恢复）
    fn map_user_trampoline(&mut self) {
        info!("mapping user trampoline");
        self.page_table.map(
            VirtAddr::from(USER_TRAMPOLINE).into(),
            PhysAddr::from(strampoline as *const () as usize).into(),// 高位0x9...被截断
            PTEFlags::R | PTEFlags::X | PTEFlags::U,
            PageSize::Page4K // 默认标准页大小
        );
    }
    /// Without kernel stacks.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        // map kernel sections
        info!(".text [{:#x}, {:#x})", stext as *const () as usize, etext as *const () as usize);
        info!(".rodata [{:#x}, {:#x})", srodata as *const () as usize, erodata as *const () as usize);
        info!(".data [{:#x}, {:#x})", sdata as *const () as usize, edata as *const () as usize);
        info!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );
        
        // 映射跳板页
        // la64下不映射到内核空间
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
                MapType::Identical,
                MapPermission::R | MapPermission::X,
                PageSize::Page2M
            ),
            None,
            stext as *const () as usize,
        );
        info!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as *const () as usize).into(),
                (erodata as *const () as usize).into(),
                MapType::Identical,
                MapPermission::R,
                PageSize::Page2M
            ),
            None,
            srodata as *const () as usize,
        );
        info!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as *const () as usize).into(),
                (edata as *const () as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
                PageSize::Page2M
            ),
            None,
            sdata as *const () as usize,
        );
        info!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as *const () as usize).into(),
                (ebss as *const () as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
                PageSize::Page2M
            ),
            None,
            sbss_with_stack as *const () as usize,
        );
        #[cfg( target_arch = "loongarch64")]{
            // 实际上riscv也最好改成这样，暂时只改la
            info!("mapping memory for devices");
            let ekernel_addr = ekernel as *const () as usize;
            memory_set.push(
                MapArea::new(
                    (ekernel_addr).into(),
                    (ekernel_addr + DMA_SIZE).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K
                ),
                None,
                ekernel_addr,
            );
            info!("mapping physical memory");
            // 临时：留下最后0x100_0000给内核栈
            memory_set.push(
                MapArea::new(
                    (ekernel_addr + DMA_SIZE).into(),
                    (MEMORY_END - 0x100_0000).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K
                ),
                None,
                ekernel_addr + DMA_SIZE,
            );
        }
        // --- 等到大页映射完再映射标准页，避免产生碎片（虽然会被放进回收栈，影响很小） ---

        #[cfg(target_arch = "riscv64")]{
            info!("mapping physical memory");
            memory_set.push(
                MapArea::new(
                    (ekernel as *const () as usize).into(),
                    (MEMORY_END - 4096).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page2M
                ),
                None,
                ekernel as *const () as usize,
            );
        }

        // 两者均有MMIO空间，地址可能不同
        info!("mapping memory-mapped registers");
        for pair in MMIO {
            memory_set.push(
                MapArea::new(
                    (*pair).0.into(),
                    ((*pair).0 + (*pair).1).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
                    PageSize::Page4K
                ),
                None,
                (*pair).0,
            );
        }

        memory_set
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// and return heap_bottom/user_sp/final_entry/main_entry metadata.
    /// Memoryset//堆底//用户栈顶//最终入口点（可能是解释器）//主程序入口点//程序头表地址//程序头表数量//程序头表项大小//解释器加载基址（如果有）
    pub fn from_elf(elf_data: &[u8]) -> Option<(Self, usize, usize, usize, usize, usize, usize, usize, Option<usize>)> {
        Self::from_elf_with_interp_loader(elf_data, |_| None)
    }

    /// Build address space from a main ELF and an optional interpreter loader.
    pub fn from_elf_with_interp_loader<F>(
        elf_data: &[u8],
        mut load_interp: F,
    ) -> Option<(Self, usize, usize, usize, usize, usize, usize, usize, Option<usize>)>
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        let mut memory_set = Self::new_bare();

        // riscv映射跳板
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();

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
        
        let mut phdr_addr = 0;
        let phnum = ph_count as usize;
        let phent = elf_header.pt2.ph_entry_size() as usize;
        let mut final_entry = elf.header.pt2.entry_point() as *const () as usize + OFFSET_FOR_USER_APP;
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

                    //获取加载段的虚拟地址范围与权限
                    let start_va: VirtAddr = (ph.virtual_addr() as usize + OFFSET_FOR_USER_APP).into();
                    let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize + OFFSET_FOR_USER_APP).into();
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
                        PageSize::Page4K
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
                        Some(&elf.input[ph.offset() as *const () as usize..(ph.offset() + ph.file_size()) as *const () as usize]),
                        ph.virtual_addr() as usize + OFFSET_FOR_USER_APP,
                    );
                    if ph.offset() <= elf_header.pt2.ph_offset()
                        && elf_header.pt2.ph_offset() < ph.offset() + ph.file_size()
                    {
                        phdr_addr =
                            (ph.virtual_addr() + (elf_header.pt2.ph_offset() - ph.offset())) as usize;
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
                        phdr_addr = ph.virtual_addr() as usize + OFFSET_FOR_USER_APP;
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
                let interp_image_base = interp_elf
                    .program_iter()
                    .filter_map(|segment| {
                        if let Ok(xmas_elf::program::Type::Load) = segment.get_type() {
                            Some(VirtAddr::from(segment.virtual_addr() as usize).std_floor().0 * PAGE_SIZE)
                        } else {
                            None
                        }
                    })
                    .min()
                    .unwrap_or(0);
                let interp_load_bias = interp_runtime_base.saturating_sub(interp_image_base);
                interp_base = Some(interp_load_bias);
                final_entry = interp_load_bias + interp_elf.header.pt2.entry_point() as usize;
                loaded_interp = true;
                info!(
                    "MemorySet::from_elf: PT_INTERP loaded '{}', entry switched {:#x} -> {:#x}",
                    interp_path,
                    main_entry,
                    final_entry
                );
                for interp_ph in interp_elf.program_iter() {
                    if let Ok(xmas_elf::program::Type::Load) = interp_ph.get_type() {
                        let start_va = interp_load_bias + interp_ph.virtual_addr() as usize;
                        let end_va = start_va + interp_ph.mem_size() as usize;
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
                        let offset = interp_ph.offset() as usize;
                        let file_size = interp_ph.file_size() as usize;
                        let data = &interp_data[offset..offset + file_size];
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
                                PageSize::Page4K
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
                "MemorySet::from_elf: PT_INTERP present but unresolved, using main entry {:#x}",
                final_entry
            );
        }
        if !saw_interp {
            info!(
                "MemorySet::from_elf: no PT_INTERP, using main entry {:#x}",
                final_entry
            );
        }
        debug!("MemorySet::from_elf: mapped common areas");
        // 按顺序布局：主程序/解释器段 -> guard -> 用户栈 -> guard -> 堆。
        const GUARD_PAGES: usize = 10;
        let user_stack_bottom = max_end_vpn.0 * PAGE_SIZE + GUARD_PAGES * PAGE_SIZE;
        memory_set.push_guard_area(max_end_vpn.0 * PAGE_SIZE, GUARD_PAGES);
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;
        memory_set.push(
            MapArea::new(
                user_stack_bottom.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
                PageSize::Page4K
            ),
            None,
            user_stack_bottom,
        );
        let heap_bottom = user_stack_top + GUARD_PAGES * PAGE_SIZE;
        memory_set.push_guard_area(user_stack_top, GUARD_PAGES);
        let heap_bottom_vpn = VirtAddr::from(heap_bottom).std_ceil();
        memory_set.push(
            MapArea::new(
                heap_bottom_vpn.into(),
                heap_bottom_vpn.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
                PageSize::Page4K
            ),
            None,
            heap_bottom,
        );
    
        memory_set.brk_index = memory_set.areas.len() - 1;//初始化堆索引
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
        // map trampoline
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();
        // 用户态信号恢复跳板
        memory_set.map_user_trampoline();
        
        // copy data sections/trap_context/user_stack
        for idx in 0..user_space.areas.len() {
            let map_type = user_space.areas[idx].map_type;
            let map_perm = user_space.areas[idx].map_perm;
            let is_shared = user_space.areas[idx].is_shared;
            let page_size = user_space.areas[idx].page_size;
            let vpn_range = VPNRange::new(
                user_space.areas[idx].vpn_range.get_start(),
                user_space.areas[idx].vpn_range.get_end(),
            );
            let share_user_pages = matches!(map_type, MapType::Framed | MapType::File)
                && map_perm.contains(MapPermission::U);
            if share_user_pages {
                let mut new_area = MapArea::from_another(&user_space.areas[idx]);
                let step = page_size.num_pages();
                let mut vpn = vpn_range.get_start();
                while vpn < vpn_range.get_end() {
                    let Some(src_pte) = user_space.page_table.translate(vpn) else {
                        vpn.step_by(step);
                        continue;
                    };
                    if !src_pte.is_valid() {
                        vpn.step_by(step);
                        continue;
                    }
                    let writable_cow = map_perm.contains(MapPermission::W) && !is_shared;
                    let child_perm = if writable_cow {
                        map_perm & !MapPermission::W
                    } else {
                        map_perm
                    };
                    let child_flags = PTEFlags::from_bits(child_perm.bits).unwrap();
                    memory_set.page_table.map(vpn, src_pte.ppn(), child_flags, page_size);
                    new_area.data_frames.insert(vpn, FrameTracker::from_ppn(src_pte.ppn(), page_size));
                    if writable_cow {
                        let parent_perm = map_perm & !MapPermission::W;
                        let parent_flags = PTEFlags::from_bits(parent_perm.bits).unwrap();
                        user_space.page_table.set_flags(vpn, parent_flags, page_size);
                    }
                    vpn.step_by(step);
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
                            if memory_set.translate(vpn).is_none() || !memory_set.translate(vpn).unwrap().is_valid() {
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
        // 复制brk_index
        memory_set.brk_index = user_space.brk_index;
        memory_set
    }
    pub fn handle_cow_fault(&mut self, bad_addr: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();
        let page_table = &mut self.page_table;
        if let Some(area) = self.areas.iter_mut().find(|a| {
            vpn >= a.vpn_range.get_start() && vpn < a.vpn_range.get_end()
        }) {
            if area.map_type == MapType::Guard || area.is_shared || !area.map_perm.contains(MapPermission::W) {
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
                    area.data_frames.insert(vpn, new_frame);
                    #[cfg(target_arch = "loongarch64")]
                    Self::flush_tlb_after_mapping_change();
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
            if VirtAddr::from(start) >= area.vpn_range.get_start().into() && VirtAddr::from(start) < area.vpn_range.get_end().into() {
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
            if VirtAddr::from(start) >= area.vpn_range.get_start().into() && VirtAddr::from(start) < area.vpn_range.get_end().into() {
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
        //println!("Activating new page table with ASID {}", self.asid());
        let satp = self.token();
        let asid = self.asid();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma x0, {asid}", asid = in(reg) asid);
        }
    }
    /// 对于龙芯，修改PGDL/H寄器
    /// 用户处于低半地址空间
    #[cfg(target_arch = "loongarch64")]
    pub fn activate(&self) {
        let pgdl = self.page_table.token();
        unsafe {
            asm!("csrwr {pgdl}, 0x19", pgdl = in(reg) pgdl);
            asm!("dbar 0");
        }
    }
    
    /// Translate a virtual page number to a page table entry
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
    pub fn translate_create(&mut self, vpn: VirtPageNum, page_size: PageSize) -> Option<PageTableEntry> {
        self.page_table.translate_create(vpn, page_size)
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_pte_dirty(&mut self, vpn: VirtPageNum) -> bool {
        if let Some((pte, _)) = self.page_table.find_pte(vpn) {
            pte.set_dirty();
            true
        } else {
            false
        }
    }
    /// 写回共享映射页面内容
    pub fn sync_shared_pages(&mut self) {
        for area in self.areas.iter_mut() {
            area.sync_back_to_file();
        }
    }
    /// Remove all `MapArea`
    pub fn recycle_data_pages(&mut self) {
        for area in self.areas.iter_mut() {
            area.unmap(&mut self.page_table);
        }
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
            area.shrink_to(&mut self.page_table, new_end.std_ceil());
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
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
    /// mmap的实现（只分配内存，不加载文件，并且不检查参数合法性）
    /// 目前的实现全用4k页
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

        // 剩余物理页数
        let free_std_pages = get_free_frames();
        // 计算需要的物理页数
        let needing_std_pages = 
            VirtAddr(addr+length).std_ceil().0 -
            VirtAddr(addr).std_floor().0;
        info!("mapping memory: addr={:#x}, length={:#x}, prot={:?}, flags={:?}, free_std_pages={}, needing_std_pages={}", 
            addr, length, prot, mmap_flags, free_std_pages, needing_std_pages);
        // 检查内存是否充足
        if free_std_pages < needing_std_pages {
            warn!(
                "mmap failed: not enough free frames (need {}, have {})",
                needing_std_pages,
                free_std_pages
            );
            return Err(Errno::ENOMEM.as_isize());
        }

        // 找合适起始地址
        let mut start_va = addr;
        if start_va == 0 {
            if let Some(new_addr) = self.find_free_area(length) {
                start_va = new_addr;
            } else {
                error!("mmap failed: no suitable free area found for length {:#x}", length);
                return Err(Errno::EEXIST.as_isize());
            }
        } else {
            // 检查冲突
            if self.has_conflict(start_va, length) {
                if mmap_flags.contains(mmap::MMapFlags::MAP_FIXED) {
                    if let Ok(_ret) = self.munmap(start_va, length) {
                        // Handle the result if needed
                        //println!("[kernel] mmap: MAP_FIXED flag set, unmapped conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                    }else {
                        //println!("[kernel] mmap: MAP_FIXED flag set, but failed to unmap conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                        return Err(Errno::EEXIST.as_isize());
                    }
                } else {
                    //println!("[kernel] mmap failed: address range [{:#x}, {:#x}) conflicts with existing mapping", start_va, start_va + length);
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


        let is_shared = mmap_flags.contains(mmap::MMapFlags::MAP_SHARED);
        let is_anonymous = mmap_flags.contains(mmap::MMapFlags::MAP_ANONYMOUS);
        if is_shared && !is_anonymous {
            // 共享文件映射 
            let file = file_inner.as_ref().unwrap();
            let mut area = MapArea {
                vpn_range: VPNRange::new(
                    VirtAddr::from(start_va).std_floor(), 
                    VirtAddr::from(start_va + length).std_ceil()
                ),
                data_frames: BTreeMap::new(), 
                map_type: MapType::File, 
                map_perm: permission,
                is_shared: true,
                backing_file: file_inner.clone()
                    .map(|f| (f.clone(), page_offset)),
                page_size:Page4K // 默认用标准页
            };

            // 文件页偏移末，开边界，也就是文件最后一页的下一个页的偏移，超过的部分不映射
            let file_end_page_offset = PhysAddr(file.get_stat().size as usize).std_ceil().0;

            let start_vpn = VirtAddr::from(start_va).std_floor().0;
            for i in 0..needing_std_pages {
                let vpn = start_vpn + i;
                let file_page_offset = page_offset + i; 
                if file_page_offset >= file_end_page_offset {
                    break;
                }
                if let Some(cache) = file.get_shared_page(file_page_offset) {
                    let page = cache.lock();
                    let ppn = page.frame.ppn;
                    // clone FrameTracker: 引用计数 +1
                    let frame_clone = page.frame.clone();
                    let pte_flags = PTEFlags::from_bits(permission.bits).unwrap();
                    self.page_table.map(VirtPageNum::from(vpn), ppn, pte_flags,Page4K);
                    area.data_frames.insert(VirtPageNum::from(vpn), frame_clone);
                } else {
                    return Err(Errno::ENOMEM.as_isize());
                }
            }
            // 注册到全局共享页面管理器
            let man = &crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER;
            man.register_file(file.ino(), file);
            // 这里是简单插入，映射在前面已经完成了
            self.areas.push(area);
        } else {
            // 普通映射
            self.insert_file_area(
                VirtAddr::from(start_va),
                VirtAddr::from(start_va + length),
                permission,
                PageSize::Page4K // mmap目前直接用标准页
            );
            
            if is_shared {
                if let Some(last_area) = self.areas.last_mut() {
                    last_area.is_shared = true;
                }
            }
        }

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        
        Ok(start_va)
        
    }

    /// 在当前地址空间中寻找一个长度为 length 的空闲连续区域
    /// 找的是逻辑区域，与实际物理页无关
    pub fn find_free_area(&self, length: usize) -> Option<usize> {
        //println!("[kernel] find_free_area: finding free area for length {:#x}", length);
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
        
        for _area in sorted_areas.iter() {
            /*println!(
                "[kernel] find_free_area: existing area [{:#x}, {:#x})",
                area.vpn_range.get_start().0 * PAGE_SIZE,
                area.vpn_range.get_end().0 * PAGE_SIZE
            );*/
        }
        
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
    /// munmap的实现
    /// 注意：不允许取消映射brk之前的区域
    pub fn munmap(&mut self, start: usize, length: usize) -> Result<(), isize> {
        let brk_idx = self.brk_index;
        let brk_area = &self.areas[brk_idx];
        let brk_end = brk_area.vpn_range.get_end().0 * PAGE_SIZE;

        let end = start + length;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        // 收集因从中间截断而产生的新右半部分区域
        let mut new_areas: Vec<MapArea> = Vec::new();

        for area in self.areas.iter_mut() {
            let a_start = area.vpn_range.get_start();
            let a_end = area.vpn_range.get_end();

            // 检查是否有交集
            if a_start < end_vpn && a_end > start_vpn {
                let delete_left = a_start >= start_vpn; // 目标区域覆盖了当前块的左侧
                let delete_right = a_end <= end_vpn;    // 目标区域覆盖了当前块的右侧

                let step = area.page_size.num_pages();
                if delete_left && delete_right {
                    // 情况1：All（当前块被目标区域完全包裹，全部删掉）
                    let mut vpn = a_start;
                    while vpn < a_end {
                        area.unmap_one(&mut self.page_table, vpn);
                        vpn.step_by(step);
                    }
                    area.resize(a_start, a_start); // 长度设为0，稍后统一 retain 清理
                    
                } else if !delete_left && !delete_right {
                    panic!("munmap: test : split area in the middle");
                    // 情况2：Split（目标区域在当前块中间，一分为二）
                    // 2.1 清理中间被 unmap 的页表和物理页
                    for vpn in VPNRange::new(start_vpn, end_vpn) {
                        area.unmap_one(&mut self.page_table, vpn);
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
                        area.unmap_one(&mut self.page_table, vpn);
                        vpn.step_by(step);
                    }
                    
                    area.resize(end_vpn, a_end);
                    
                } else if delete_right {
                    // 情况4：Inc_Right（删掉右边部分）
                    let mut vpn = start_vpn;
                    while vpn < a_end {
                        area.unmap_one(&mut self.page_table, vpn);
                        vpn.step_by(step);
                    }
                    area.resize(a_start, start_vpn);
                }
            }
        }

        // 插入劈开产生的新区域
        self.areas.extend(new_areas);

        // 删除长度为0的区域，但不删除brk及之前的区域
        self.areas.retain(|area| {
            area.vpn_range.get_start() < area.vpn_range.get_end() ||
            area.vpn_range.get_start() <= VirtAddr::from(brk_end).std_floor()
        });

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();

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
        if let Some(area) = self.areas.iter_mut().find(|a| {
            a.contains(vpn)
        }) {
            if area.map_type == MapType::Guard {
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
            
            // 共享文件映射直接返回false，交给后续处理
            if area.is_shared && area.backing_file.is_some() {
                return false;
            }

            // 非共享映射：惰性分配新物理帧
            area.map_one(page_table, vpn, page_size_opt.unwrap());
            
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            
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
            // 执行扩张：将该 Area 的起点向下延伸到 vpn (注意保留原来的终点)
            self.areas[idx].vpn_range = crate::mm::address::VPNRange::new(
                vpn,
                self.areas[idx].vpn_range.get_end()
            );
            
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
        println!(
            "[kernel] memory_set: asid={}, brk_index={}, area_count={}",
            self.asid.0,
            self.brk_index,
            self.areas.len()
        );
        for (idx, area) in self.areas.iter().enumerate() {
            let start = area.vpn_range.get_start().0 * PAGE_SIZE;
            let end = area.vpn_range.get_end().0 * PAGE_SIZE;
            let badv_hit = badv.map(|addr| addr >= start && addr < end).unwrap_or(false);
            let era_hit = era.map(|addr| addr >= start && addr < end).unwrap_or(false);
            println!(
                "[kernel] area[{}] [{:#x}, {:#x}) {:?}{}{}{}",
                idx,
                start,
                end,
                area.map_perm,
                if idx == self.brk_index { " [brk]" } else { "" },
                if badv_hit { " [BADV]" } else { "" },
                if era_hit { " [ERA]" } else { "" },
            );
        }
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
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    pub vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
    pub is_shared: bool,
    // 记录文件信息和页偏移，其中页偏移的语义为映射起始页在文件中的页偏移量
    pub backing_file: Option<(Arc<dyn File + Send + Sync>, usize)>,
    pub page_size: PageSize,
}

impl MapArea {
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
            page_size: another.page_size,
        }
    }
    /// 解除单页映射并释放物理帧
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        match self.map_type {
            MapType::Framed | MapType::File => {
                // 共享文件映射需要先写回内容
                if self.is_shared {
                    if let Some((file, base_offset)) = &self.backing_file {
                        let file_page_offset = *base_offset + (vpn.0 - self.vpn_range.get_start().0);
                        let man = &crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER;
                        man.write_back_page_cache(file.ino(), file_page_offset, file);
                    }
                }

                // 先 drop FrameTracker，主要针对 clone_vm 子进程继承父进程页表的情况
                // 调了好久才发现这种情况，如果不判断是否有frame就改页表，会炸掉:(
                if self.data_frames.remove(&vpn).is_some() {
                    // 解除映射页表
                    page_table.unmap(vpn);
                }
            }
            MapType::Identical => {
                page_table.unmap(vpn);
            }
            MapType::Guard => {}
        }
    }

    /// 用于 Split 时复制出相同属性的新区域
    pub fn clone_meta_with_new_range(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> Self {
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(), // 初始为空，由外面填充
            map_type: self.map_type,
            map_perm: self.map_perm,
            is_shared: self.is_shared,               
            backing_file: self.backing_file.clone(),
            page_size: self.page_size,
        }
    }
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum, page_size: PageSize) {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Framed => {
                let frame = frame_alloc(page_size).unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
            // 文件映射：目前简单处理为 Framed，以便 sys_mmap 可以直接读写
            MapType::File => {
                let frame = frame_alloc(page_size).unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
            MapType::Guard => {
                return;
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        #[cfg(target_arch = "loongarch64")]
        // la64在内核态不需要用页表
        if self.map_type !=  MapType::Identical{
            page_table.map(vpn, ppn, pte_flags, page_size);
        }
        #[cfg(target_arch = "riscv64")]
        page_table.map(vpn, ppn, pte_flags, page_size);
    }
    /* pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        match self.map_type {
            MapType::Framed | MapType::File => {
                if self.data_frames.remove(&vpn).is_some() {
                    // 共享映射的 PTE 可能已被 munmap 解除，先检查
                    if page_table.translate(vpn).is_some() {
                        page_table.unmap(vpn);
                    }
                } else if self.is_shared {
                    if page_table.translate(vpn).is_some() && page_table.translate(vpn).unwrap().is_valid() {
                        page_table.unmap(vpn);
                    }
                }
            }
            MapType::Identical => {
                page_table.unmap(vpn);
            }
            MapType::Guard => {}
        }
    }*/
    pub fn map(&mut self, page_table: &mut PageTable) {
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_start();
        while vpn < self.vpn_range.get_end() {
            self.map_one(page_table, vpn, self.page_size);
            vpn.step_by(step);
        }
    }
    pub fn unmap(&mut self, page_table: &mut PageTable) {
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_start();
        while vpn < self.vpn_range.get_end() {
            self.unmap_one(page_table, vpn);
            vpn.step_by(step);
        }
    }
    #[allow(unused)]
    pub fn shrink_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        let step = self.page_size.num_pages();
        let mut vpn = new_end;
        while vpn < self.vpn_range.get_end() {
            self.unmap_one(page_table, vpn);
            vpn.step_by(step);
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    #[allow(unused)]
    pub fn append_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        let step = self.page_size.num_pages();
        let mut vpn = self.vpn_range.get_end();
        while vpn < new_end {
            trace!("MapArea::append_to: old vpn end={:#x} , mapping new page vpn={:#x}", self.vpn_range.get_end().0, vpn.0);
            self.map_one(page_table, vpn, self.page_size);
            vpn.step_by(step);
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// 只修改边界，不解除映射或新增映射
    #[allow(unused)]
    pub fn resize(&mut self, new_start: VirtPageNum, new_end: VirtPageNum) {
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
        }
        else {
            let mut data = Vec::new();
            let step = self.page_size.num_pages();
            let mut vpn = self.vpn_range.get_start();
            while vpn < self.vpn_range.get_end() {
                let src = &page_table.translate(vpn).unwrap().ppn().get_bytes_array_with_size(self.page_size);
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
                    man.write_back_page_cache(ino, file_page_offset, file);
                }
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// map type for memory set: identical or framed
pub enum MapType {
    Identical,
    Framed,
    File,
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
    let mid_text: VirtAddr = ((stext as *const () as usize + etext as *const () as usize) / 2).into();
    let mid_rodata: VirtAddr = ((srodata as *const () as usize + erodata as *const () as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as *const () as usize + edata as *const () as usize) / 2).into();
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
