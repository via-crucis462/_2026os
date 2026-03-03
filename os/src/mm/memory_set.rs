use super::{frame_alloc, FrameTracker};
use super::{PageTable, pte::*, PTEFlags};
#[allow(unused)]
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
#[allow(unused)]
use crate::arch::config::*;
use crate::mm::mmap;
use crate::sync::UPSafeCell;
use alloc::collections::BTreeMap;
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
    pub static ref KERNEL_SPACE: Arc<UPSafeCell<MemorySet>> =
        Arc::new(unsafe { UPSafeCell::new(MemorySet::new_kernel()) });
}

/// the kernel token
pub fn kernel_token() -> usize {
    KERNEL_SPACE.exclusive_access().token()
}

/// address space
/// 注意维护brk_index
pub struct MemorySet {
    page_table: PageTable,
    areas: Vec<MapArea>,
    brk_index: usize, //新增，用于记录brk所在area（堆区）的索引，请注意维护，后续可能会删除
}

impl MemorySet {
    /// Create a new empty `MemorySet`.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
            brk_index: 0,// 注意维护！！
        }
    }
    /// Get the page table token
    pub fn token(&self) -> usize {
        self.page_table.token()
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
        info!("map area: [{:#x}, {:#x}), {:?}", map_area.vpn_range.get_start().0 * PAGE_SIZE, map_area.vpn_range.get_end().0 * PAGE_SIZE, map_area.map_perm);
        self.areas.push(map_area);
    }
    /// Mention that trampoline is not collected by areas.
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]
    fn map_trampoline(&mut self) {
        info!("mapping trampoline");
        self.page_table.map(
            VirtAddr::from(TRAMPOLINE).into(),// 高位0xf...被截断
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
    /// also returns user_sp_base and entry point.

    pub fn from_elf(elf_data: &[u8]) -> (Self, usize, usize, usize, usize, usize) {
        let mut memory_set = Self::new_bare();
        // map trampoline
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();
        // map program headers of elf, with U flag
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut max_end_vpn = VirtPageNum(0);
        
        let mut phdr_addr = 0;
        let phnum = ph_count as usize;
        let phent = elf_header.pt2.ph_entry_size() as usize;

        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize 
                    + OFFSET_FOR_USER_APP).into();
                let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize
                    + OFFSET_FOR_USER_APP).into();
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
                let map_area = MapArea::new(start_va, end_va, MapType::Framed, map_perm);
                max_end_vpn = map_area.vpn_range.get_end();
                memory_set.push(
                    map_area,
                    Some(&elf.input[ph.offset() as *const () as usize..(ph.offset() + ph.file_size()) as *const () as usize]),
                    ph.virtual_addr() as usize + OFFSET_FOR_USER_APP,
                );

                // 如果该 LOAD 段包含了程序头表，则记录其虚拟地址
                if ph.offset() <= elf_header.pt2.ph_offset() && 
                   elf_header.pt2.ph_offset() < ph.offset() + ph.file_size() {
                    phdr_addr = (ph.virtual_addr() + (elf_header.pt2.ph_offset() - ph.offset())) as usize;
                }
            }
        }
        //println!("[kernel] MemorySet::from_elf: mapped common areas");
        // map user stack with U flags
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_stack_bottom: usize = max_end_va.into();
        // guard page
        user_stack_bottom += PAGE_SIZE;
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
        // used in sbrk
        // 堆区空间设置在栈区之后
        memory_set.push(
            MapArea::new(
                user_stack_top.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
            user_stack_top,
        );
        memory_set.brk_index = memory_set.areas.len() - 1;// 此时brk在最后一个区域
        //println!("[kernel] MemorySet::from_elf: mapped all areas");
        // map TrapContext
        // la64下不需要映射
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
        );
        (
            memory_set,
            user_stack_top,
            elf.header.pt2.entry_point() as *const () as usize + OFFSET_FOR_USER_APP,
            phdr_addr,
            phnum,
            phent,
        )
    }
    /// Create a new address space by copy code&data from a exited process's address space.
    pub fn from_existed_user(user_space: &Self) -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        #[cfg(target_arch = "riscv64")]
        memory_set.map_trampoline();
        // copy data sections/trap_context/user_stack
        for area in user_space.areas.iter() {
            let new_area = MapArea::from_another(area);
            let start_va: usize = new_area.vpn_range.get_start().into();
            memory_set.push(new_area, None, start_va << 12);
            // copy data from another space
            for vpn in area.vpn_range {
                let src_ppn = user_space.translate(vpn).unwrap().ppn();
                let dst_ppn = memory_set.translate(vpn).unwrap().ppn();
                dst_ppn
                    .get_bytes_array()
                    .copy_from_slice(src_ppn.get_bytes_array());
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
            true
        } else {
            false
        }
    }

    /// append the area to new_end
    #[allow(unused)]
    pub fn append_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.append_to(&mut self.page_table, new_end.ceil());
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
        prot: mmap::MMapProt
    ) -> Result<usize, i32> {
        let mut start_va = addr;
        if start_va == 0 {
            if let Some(new_addr) = self.find_free_area(length) {
                start_va = new_addr;
            } else {
                return Err(-1);
            }
        }

        // 检查冲突
        if self.has_conflict(start_va, length) {
            return Err(-1);
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

        // 映射区域
        self.insert_file_area(
            VirtAddr::from(start_va),
            VirtAddr::from(start_va + length),
            permission,
        );

        Ok(start_va)
    }

    pub fn find_free_area(&self, length: usize) -> Option<usize> {
        // 从 0x4000_0000 开始往上找，避开程序段和堆，并保留足够的安全距离
        let mut current_addr = 0x4000_0000;
        let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1); // 对齐到页
        
        let mut sorted_areas: Vec<_> = self.areas.iter().collect();
        sorted_areas.sort_by_key(|a| a.vpn_range.get_start());
        
        for area in sorted_areas {
            let area_start: usize = area.vpn_range.get_start().into();
            if current_addr + length <= area_start {
                return Some(current_addr);
            }
            let area_end: usize = area.vpn_range.get_end().into();
            if area_end > current_addr {
                current_addr = area_end;
            }
        }
        
        if current_addr + length < 0x8000_0000 { // 确保不超过用户空间上限
            Some(current_addr)
        } else {
            None
        }
    }
    /// munmap的实现
    /// 注意：不允许取消映射brk之前的区域
    pub fn munmap(&mut self, start: usize, length: usize) -> Result<(),i32> {
        let brk_idx = self.brk_index;
        let brk_area = &self.areas[brk_idx];// brk area
        let _brk_start = brk_area.vpn_range.get_start().0 * PAGE_SIZE;
        let brk_end = brk_area.vpn_range.get_end().0 * PAGE_SIZE;

        // 检查是否越界
        if start < brk_area.vpn_range.get_end().0 * PAGE_SIZE {
            return Err(-1);
        }

        let end = start + length;
        let start_vpn = VirtAddr::from(start).floor();// 目标起始页号
        let end_vpn = VirtAddr::from(end).ceil();// 目标结束页号

        // 可能存在的新area，写在外面以避免for循环中self引用问题引发的报错
        let mut new_area: Option<MapArea> = None;

        // 注意只遍历brk之后的area
        for area in self.areas[brk_idx+1..].iter_mut() {
            // 暂时未检查是否：取消映射trap_context、trampoline

            // 找到有重合部分的区域
            if area.vpn_range.get_start() < end_vpn && area.vpn_range.get_end() > start_vpn {// 有交集;
                let inc_left = area.vpn_range.get_start() >= start_vpn;// 删左边部分
                let inc_right = area.vpn_range.get_end() <= end_vpn;// 删右边部分
                let split = (!inc_left) & (!inc_right);// 从中间分开成两个部分
                let all = inc_left & inc_right;// 删掉整个区域


                if all {
                    // 使其长度为0, 稍后再删除
                    area.shrink_to(&mut self.page_table, area.vpn_range.get_start());
                } else if split {
                    // 缩短自身，成为新段的左边部分
                    let old_end = area.vpn_range.get_end();
                    let mut mid_ft = area.data_frames.split_off(&start_vpn);
                    area.resize(area.vpn_range.get_start(), start_vpn);
                    // 取出右边保留部分的ft
                    let right_ft = mid_ft.split_off(&end_vpn);
                    // 中间部分解除映射
                    drop(mid_ft);
                    // 新建右边部分
                    new_area = Some(MapArea::new(
                        VirtAddr::from(end),
                        VirtAddr::from(old_end),
                        area.map_type,
                        area.map_perm,
                    ));
                    new_area.as_mut().unwrap().data_frames = right_ft;
                } else if inc_left {
                    // 解除映射左边部分
                    for vpn in VPNRange::new(area.vpn_range.get_start(), end_vpn) {
                        area.unmap_one(&mut self.page_table, vpn);
                    }
                    // 调整范围
                    area.resize(end_vpn, area.vpn_range.get_end());
                } else if inc_right {
                    // 删右边部分
                    area.shrink_to(&mut self.page_table, end_vpn);
                }

            }
        }

        // 如果有，插入新area
        if let Some(area) = new_area {
            self.areas.push(area);
        }

        // 删除长度为0的area
        // 但不删除brk之前
        self.areas.retain(
            |area| area.vpn_range.get_start() < area.vpn_range.get_end()||
            area.vpn_range.get_start() <= brk_end.into()//brk之前的全部保留
            );
        Ok(())
    }
    // brk的实现（通过调整brk区域大小实现）
    // 注意到rcore已实现，不过其实现过简且用到了遍历，复杂度较高，这里重新实现一个更简单的版本
    // 目前的实现有问题（必须页对齐）故暂时弃置
    pub fn _brk(&mut self, addr: usize) -> Result<usize, i32> {        
        let brk_area = &mut self.areas[self.brk_index];
        let old_brk = brk_area.vpn_range.get_end().0 * PAGE_SIZE;
        if addr == 0{
            return Ok(old_brk);
        } else {
            if addr > old_brk {
                // 扩大
                brk_area.append_to(&mut self.page_table, VirtAddr::from(addr).ceil());
            } else if addr < old_brk {
                if addr < brk_area.vpn_range.get_start().0 * PAGE_SIZE {// 不允许缩小到起始地址之前
                    return Err(-1);
                }
                // 缩小
                brk_area.shrink_to(&mut self.page_table, VirtAddr::from(addr).ceil());
            }
        }
        Ok(addr)
    }

}
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
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
        }
    }
    pub fn from_another(another: &Self) -> Self {
        Self {
            vpn_range: VPNRange::new(another.vpn_range.get_start(), another.vpn_range.get_end()),
            data_frames: BTreeMap::new(),
            map_type: another.map_type,
            map_perm: another.map_perm,
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
        if self.map_type == MapType::Framed {
            self.data_frames.remove(&vpn);
        }
        page_table.unmap(vpn);
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
