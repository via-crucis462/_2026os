use super::{frame_alloc, FrameTracker};
use super::{PageTable, pte::*, PTEFlags};
#[allow(unused)]
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
use super::id::*;
#[allow(unused)]
use crate::arch::config::*;
use crate::arch::trap::current_trap_cx_user_va;
use crate::mm::mmap;
use crate::sync::MPSafeCell;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
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
    #[cfg(target_arch = "riscv64")]
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
    areas: Vec<MapArea>,
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
    /// Get the page table token
    pub fn token(&self) -> usize {
        self.page_table.token()
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
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission),
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
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::File, permission),
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
        );
    }
    /// Without kernel stacks.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        // la64下不映射到内核空间
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();
        // map kernel sections
        info!(".text [{:#x}, {:#x})", stext as *const () as usize, etext as *const () as usize);
        info!(".rodata [{:#x}, {:#x})", srodata as *const () as usize, erodata as *const () as usize);
        info!(".data [{:#x}, {:#x})", sdata as *const () as usize, edata as *const () as usize);
        info!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );
        info!("mapping .text section");
        memory_set.push(
            MapArea::new(
                (stext as *const () as usize).into(),
                (etext as *const () as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::X,
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
                ),
                None,
                ekernel_addr + DMA_SIZE,
            );
        }
        #[cfg(target_arch = "riscv64")]{
            info!("mapping physical memory");
            memory_set.push(
                MapArea::new(
                    (ekernel as *const () as usize).into(),
                    (MEMORY_END - 4096).into(),
                    MapType::Identical,
                    MapPermission::R | MapPermission::W,
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
                    let map_area = MapArea::new(start_va, end_va, MapType::Framed, map_perm);
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
                            Some(VirtAddr::from(segment.virtual_addr() as usize).floor().0 * PAGE_SIZE)
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
                        let interp_end_vpn: VirtPageNum = VirtAddr::from(end_va).ceil();
                        if interp_end_vpn > max_end_vpn {
                            max_end_vpn = interp_end_vpn;
                        }
                        memory_set.push(
                            MapArea::new(
                                start_va.into(),
                                end_va.into(),
                                MapType::Framed,
                                map_perm,
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
            ),
            None,
            user_stack_bottom,
        );
        let heap_bottom = user_stack_top + GUARD_PAGES * PAGE_SIZE;
        memory_set.push_guard_area(user_stack_top, GUARD_PAGES);
        let heap_bottom_vpn = VirtAddr::from(heap_bottom).ceil();
        memory_set.push(
            MapArea::new(
                heap_bottom_vpn.into(),
                heap_bottom_vpn.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
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
    pub fn from_existed_user(user_space: &Self) -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();
        
        // copy data sections/trap_context/user_stack
        for area in user_space.areas.iter() {
            if area.is_shared {

                let mut new_area = MapArea::new(
                    VirtAddr::from(area.vpn_range.get_start().0 * PAGE_SIZE),
                    VirtAddr::from(area.vpn_range.get_end().0 * PAGE_SIZE),
                    area.map_type,
                    area.map_perm,
                );
                new_area.is_shared = true;

                // 遍历父进程的虚拟页号
                for vpn in area.vpn_range {
                    if let Some(src_pte) = user_space.translate(vpn) {
                        if src_pte.is_valid() {
                            // 强行把子进程的虚拟页，映射到父进程的同一块物理页（PPN）上！
                            let flags = PTEFlags::from_bits(area.map_perm.bits).unwrap();
                            memory_set.page_table.map(vpn, src_pte.ppn(), flags);
                        }
                    }
                }
                // 注意：没有 copy_data，也没有生成 FrameTracker
                memory_set.areas.push(new_area);

            } else {
                // ==========================================
                // 🐢 传统流程：私有内存，走原来的深拷贝逻辑
                // ==========================================
                let mut new_area: MapArea = MapArea::from_another(area);
                let start_va: VirtAddr = new_area.vpn_range.get_start().into();
                memory_set.push(new_area, None, start_va.0);
                
                // copy data from another space
                for vpn in area.vpn_range {
                    if let Some(src_pte) = user_space.translate(vpn) {
                        if src_pte.is_valid() {
                            let src_ppn = src_pte.ppn();
                            // 由于惰性分配，我们要确保目标页分配了再拷贝
                            if memory_set.translate(vpn).is_none() || !memory_set.translate(vpn).unwrap().is_valid() {
                                memory_set.page_table.translate_create(vpn);
                            }
                            let dst_ppn = memory_set.translate(vpn).unwrap().ppn();
                            dst_ppn.get_bytes_array().copy_from_slice(src_ppn.get_bytes_array());
                        }
                    }
                }
            }
        }

        // 复制brk_index
        memory_set.brk_index = user_space.brk_index;
        memory_set
    }
    /// Change page table by writing satp CSR Register.
    #[cfg(target_arch = "riscv64")]
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma");
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
        self.page_table.translate(vpn)
    }
    pub fn translate_create(&mut self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.page_table.translate_create(vpn)
    }
    /// Remove all `MapArea`
    pub fn recycle_data_pages(&mut self) {
        self.areas.clear();
    }

    /// shrink the area to new_end
    #[allow(unused)]
    pub fn shrink_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.shrink_to(&mut self.page_table, new_end.ceil());
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
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.append_to(&mut self.page_table, new_end.ceil());
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
            let target_start = VirtAddr::from(start).floor();
            let target_end = VirtAddr::from(start + len).ceil();
            if target_end > area_start && target_start < area_end {
                return true;
            }
        }
        false
    }
    /// mmap的实现（只分配内存，不加载文件）
    pub fn mmap(
        &mut self,
        addr: usize,
        length: usize,
        prot: mmap::MMapProt,
        mmap_flags: mmap::MMapFlags
    ) -> Result<usize, i32> {
        let mut start_va = addr;
        if start_va == 0 {
            if let Some(new_addr) = self.find_free_area(length) {
                start_va = new_addr;
            } else {
                //println!("[kernel] mmap failed: no suitable free area found for length {:#x}", length);
                return Err(-1);
            }
        }
        else {
                // 最少分配一页
                let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
                // 检查冲突
                if self.has_conflict(start_va, length) {
                    if mmap_flags.contains(mmap::MMapFlags::MAP_FIXED) {
                        if let Ok(_ret) = self.munmap(start_va, length) {
                            // Handle the result if needed
                            //println!("[kernel] mmap: MAP_FIXED flag set, unmapped conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                        }else {
                            //println!("[kernel] mmap: MAP_FIXED flag set, but failed to unmap conflicting area at [{:#x}, {:#x})", start_va, start_va + length);
                            return Err(-1);
                        }
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
        //println!("[kernel] mmap: mapping area [{:#x}, {:#x}) with permissions {:?}", start_va, start_va + length, permission);
        // 映射区域
        self.insert_file_area(
        VirtAddr::from(start_va),
        VirtAddr::from(start_va + length),
            permission,
        );
        if mmap_flags.contains(mmap::MMapFlags::MAP_SHARED) {
            self.areas.last_mut().unwrap().is_shared = true;
        }
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        Ok(start_va)
        
    }

    /// 在当前地址空间中寻找一个长度为 length 的空闲连续区域
    pub fn find_free_area(&self, length: usize) -> Option<usize> {
        // 从用户空间的 0x4000_0000 开始往上找
         //println!("[kernel] find_free_area: finding free area for length {:#x}", length);
        // 将长度向上对齐到页
        let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

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
    pub fn munmap(&mut self, start: usize, length: usize) -> Result<(), i32> {
        let brk_idx = self.brk_index;
        let brk_area = &self.areas[brk_idx];
        let brk_end = brk_area.vpn_range.get_end().0 * PAGE_SIZE;

        let end = start + length;
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();

        // 收集因从中间截断而产生的新右半部分区域
        let mut new_areas: Vec<MapArea> = Vec::new();

        for area in self.areas.iter_mut() {
            let a_start = area.vpn_range.get_start();
            let a_end = area.vpn_range.get_end();

            // 检查是否有交集
            if a_start < end_vpn && a_end > start_vpn {
                let delete_left = a_start >= start_vpn; // 目标区域覆盖了当前块的左侧
                let delete_right = a_end <= end_vpn;    // 目标区域覆盖了当前块的右侧

                if delete_left && delete_right {
                    // 情况1：All（当前块被目标区域完全包裹，全部删掉）
                    for vpn in VPNRange::new(a_start, a_end) {
                        area.unmap_one(&mut self.page_table, vpn);
                    }
                    area.resize(a_start, a_start); // 长度设为0，稍后统一 retain 清理
                    
                } else if !delete_left && !delete_right {
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
                    let mut right_area = MapArea::new(
                        VirtAddr::from(end_vpn.0 * PAGE_SIZE),
                        VirtAddr::from(a_end.0 * PAGE_SIZE),
                        area.map_type,
                        area.map_perm,
                    );
                    right_area.data_frames = right_frames;
                    new_areas.push(right_area);
                    
                } else if delete_left {
                    // 情况3：Inc_Left（删掉左边部分）
                    for vpn in VPNRange::new(a_start, end_vpn) {
                        area.unmap_one(&mut self.page_table, vpn);
                    }
                    area.resize(end_vpn, a_end);
                    
                } else if delete_right {
                    // 情况4：Inc_Right（删掉右边部分）
                    for vpn in VPNRange::new(start_vpn, a_end) {
                        area.unmap_one(&mut self.page_table, vpn);
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
            area.vpn_range.get_start() <= VirtAddr::from(brk_end).floor()
        });

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();

        Ok(())
    }
    /// 处理缺页异常。如果触发异常的地址在合法区域内，则为其分配物理页；否则返回 false。
    #[no_mangle]
    #[inline(never)]
    pub fn handle_page_fault(&mut self, bad_addr: usize, sp: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).floor();
        let page_table = &mut self.page_table;
        
        // 1. 遍历寻找包含该虚拟页号的合法内存段 (MapArea)
        if let Some(area) = self.areas.iter_mut().find(|a| {
            vpn >= a.vpn_range.get_start() && vpn < a.vpn_range.get_end()
        }) {
            if area.map_type == MapType::Guard {
                return false;
            }
            // 2. 检查该页是否已经在页表中映射
            if let Some(pte) = page_table.translate(vpn) {
                if pte.is_valid() {
                    // 已经映射却还报 Fault，通常是非法写只读段
                    return false; 
                }
            }
            
            // 3. 确认为合法的未映射页（惰性分配触发），执行分配和映射
            area.map_one(page_table, vpn);
            
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            
            return true; // 惰性分配修复成功！
        }
        
        // 4. 【新增】：动态扩张用户栈 (Dynamic Stack Growth)
        let sp_vpn = VirtAddr::from(sp).floor();
        
        // 设定一个栈最大允许单次/总共扩张的大小，比如 32 页 (128KB)，防止恶意程序耗尽内存
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
                self.areas[idx].map_one(page_table, VirtPageNum::from(v));
            }
            
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            
            // trace!("[kernel] User stack dynamically expanded down to {:#x}", bad_addr);
            return true; // 栈扩张修复成功！
        }

        // 5. 如果既不在合法区域，也不符合栈扩张规则，则是真正的野指针/无可救药的溢出
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
}
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
    pub is_shared: bool,
}

impl MapArea {
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
            is_shared: false,
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
        }
    }
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Framed => {
                let frame = frame_alloc().unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
            // 文件映射：目前简单处理为 Framed，以便 sys_mmap 可以直接读写
            MapType::File => {
                let frame = frame_alloc().unwrap();
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
            page_table.map(vpn, ppn, pte_flags);
        }
        #[cfg(target_arch = "riscv64")]
        page_table.map(vpn, ppn, pte_flags);
    }
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        match self.map_type {
            MapType::Framed | MapType::File => {
                if self.data_frames.remove(&vpn).is_some() {
                    page_table.unmap(vpn);
                } else if self.is_shared {
                    // 核心：子进程的共享页没有 FrameTracker（因为物理页在父进程手里），
                    // 但子进程退出时依然需要解除自己页表里的映射，防止死锁或崩溃。
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
    }
    pub fn map(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.map_one(page_table, vpn);
        }
    }
    pub fn unmap(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.unmap_one(page_table, vpn);
        }
    }
    #[allow(unused)]
    pub fn shrink_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(new_end, self.vpn_range.get_end()) {
            self.unmap_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    #[allow(unused)]
    pub fn append_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(self.vpn_range.get_end(), new_end) {
            trace!("MapArea::append_to: old vpn end={:#x} , mapping new page vpn={:#x}", self.vpn_range.get_end().0, vpn.0);
            self.map_one(page_table, vpn)
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
                .get_bytes_array()[page_offset..page_offset + src_len];
            dst.copy_from_slice(&data[data_offset..data_offset + src_len]);
            
            data_offset += src_len;
            page_offset = 0;
            current_vpn.step();
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
            for vpn in self.vpn_range {
                let src = &page_table.translate(vpn).unwrap().ppn().get_bytes_array();
                data.extend_from_slice(src);
            }
            Some(data)
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
        .translate(mid_text.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.floor())
        .unwrap()
        .executable(),);
    println!("remap_test passed!");
}
