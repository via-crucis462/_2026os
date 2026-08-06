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
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use crate::sync::RwLock;

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
    pub static ref KERNEL_SPACE: MemorySet = MemorySet::new_kernel();
}

/// the kernel token
pub fn kernel_token() -> usize {
    KERNEL_SPACE.token()
}

/// ASID used by the kernel page table that owns all kernel-stack mappings.
///
/// ASIDs identify address spaces rather than individual virtual ranges, so
/// every kernel stack shares this dedicated kernel address-space ASID.
pub fn kernel_asid() -> usize {
    KERNEL_SPACE.asid()
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
/// 在锁粒度细化后约定锁序：areas -> MapArea -> PageTable
///
/// 如果有解除映射，在 PageTable 修改后调用 flush_tlb_targets() 刷新 tlb，再释放旧帧
pub struct MemorySet {
    /// 不可修改
    asid: ASIDHandle,
    /// 起始页号（含）-> 映射区域
    pub areas: RwLock<BTreeMap<VirtPageNum, Arc<Mutex<MapArea>>>>,
    /// 页表，单独持有自身页帧
    page_table: Arc<RwLock<PageTable>>,
    /// 约定下面两个值只能持有 areas 的读锁时访问，避免在修改映射时被其他线程修改
    /// 当前程序断点
    brk: AtomicU64,
    /// 堆区起点（创建地址空间时确定），brk 不允许低于该值
    start_brk: AtomicU64,
}

impl MemorySet {
    /// 将内核根页表的高半地址空间引用进页表
    /// 
    /// 从第一次调用此函数开始，内核的根页表按约定不再变化
    /// 直接复制根页表的高半部分
    #[cfg(target_arch = "riscv64")]
    pub fn install_kernel_space(&self) {
        let kernel_space = &KERNEL_SPACE;
        let kernel_pt = kernel_space.page_table.read();
        self.page_table.write().share_kernel_half(&kernel_pt);
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
            page_table: Arc::new(RwLock::new(PageTable::new())),
            asid: asid_alloc().into(),
            areas: RwLock::new(BTreeMap::new()),
            brk: AtomicU64::new(0),
            start_brk: AtomicU64::new(0),
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
        let parent_areas = parent.areas.read();
        let parent_page_table = parent.page_table.read();
        let areas = {
            let mut areas = BTreeMap::new();
            for area in parent_areas.values() {
                let area = area.lock();
                let new_area = MapArea::from_another(&area);
                areas.insert(new_area.vpn_range.get_start(), Arc::new(Mutex::new(new_area)));
            }
            areas
        };
        Self {
            page_table: Arc::new(RwLock::new(PageTable::alias_of(&parent_page_table))),
            asid: asid_alloc(),
            areas: RwLock::new(areas),
            brk: AtomicU64::new(parent.brk.load(Ordering::Relaxed)),
            start_brk: AtomicU64::new(parent.start_brk.load(Ordering::Relaxed)),
        }
    }
    /// Get the page table token
    pub fn token(&self) -> usize {
        let pt = self.page_table.read();
        #[cfg(target_arch = "riscv64")]
        {
            return pt.token(self.asid());
        }
        #[cfg(target_arch = "loongarch64")]
        {
            pt.token()
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
    /// 快照所有区域（调试/只读遍历用），调用方自行 lock 每个区域。
    pub fn area_snapshot(&self) -> Vec<Arc<Mutex<MapArea>>> {
        self.areas.read().values().cloned().collect()
    }
    /// 堆区起点，program break 不允许低于该值。
    pub fn start_brk(&self) -> usize {
        let _guard = self.areas.read();
        self.start_brk.load(Ordering::Relaxed) as usize
    }

    /// 返回当前 program break（字节粒度）。
    pub fn current_brk(&self) -> usize {
        let _guard = self.areas.read();
        self.brk.load(Ordering::Relaxed) as usize
    }

    /// 调整当前任务的 program break，并把堆区补充填满或收缩。
    pub fn change_program_brk(&self, addr: usize) -> Result<usize, i32> {
        let old_brk = self.current_brk();
        if addr == 0 {
            return Ok(old_brk);
        }
        if addr < self.start_brk() {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }

        let old_top = VirtAddr::from(old_brk).std_ceil();
        let new_top = VirtAddr::from(addr).std_ceil();
        if old_top == new_top {
            // 同一页内移动，无需改动映射
            self.brk.store(addr as u64, Ordering::Relaxed);
            return Ok(addr);
        }

        if addr > old_brk {
            let pages = (new_top.0 - old_top.0) / PAGE_SIZE;
            if get_free_frames() < pages {
                return Err(Errno::ENOMEM.as_isize() as i32);
            }
            self.grow_heap(old_top.into(), new_top.into(), &self.brk, addr)?;
        } else {
            // shrink_heap 的参数是 (旧堆顶, 新堆顶)：删除 [new_top, old_top) 区间。
            self.shrink_heap(old_top.into(), new_top.into(), &self.brk, addr)?;
        }
        Ok(addr)
    }

    /// 扩展堆区到 [from, to)（均已页对齐）：优先扩展现有堆顶区域，否则新建。
    /// 在持有 areas 写锁时更新 brk，保证 brk 与堆区映射的一致性。
    fn grow_heap(
        &self,
        from: VirtAddr,
        to: VirtAddr,
        brk: &AtomicU64,
        new_brk: usize,
    ) -> Result<(), i32> {
        let from_vpn = from.std_ceil();
        let to_vpn = to.std_ceil();
        if to_vpn <= from_vpn {
            return Ok(());
        }
        if self.has_conflict(from.0, to.0 - from.0) {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }
        let heap_start_vpn = VirtAddr::from(self.start_brk()).std_ceil();
        let mut areas = self.areas.write();
        let target = {
            let mut found = None;
            for area_arc in areas.values() {
                let area = area_arc.lock();
                if area.vpn_range.get_end() == from_vpn
                    && area.vpn_range.get_start() >= heap_start_vpn
                {
                    found = Some(area_arc.clone());
                    break;
                }
            }
            found
        };
        if let Some(target) = target {
            let mut area = target.lock();
            let mut pt = self.page_table.write();
            area.append_to(&mut pt, to_vpn);
        } else {
            let mut area = MapArea::new(
                from,
                to,
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
                PageSize::Page4K,
            );
            let key = area.vpn_range.get_start();
            let mut pt = self.page_table.write();
            area.map(&mut pt);
            drop(pt);
            areas.insert(key, Arc::new(Mutex::new(area)));
        }
        brk.store(new_brk as u64, Ordering::Relaxed);
        drop(areas);
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        Ok(())
    }

    /// 收缩堆区，跨越空洞/分裂区域时整段移除。
    /// 在持有 areas 写锁时更新 brk，保证 brk 与堆区映射的一致性。
    fn shrink_heap(
        &self,
        from: VirtAddr,
        to: VirtAddr,
        brk: &AtomicU64,
        new_brk: usize,
    ) -> Result<(), i32> {
        let from_vpn = from.std_ceil();
        let to_vpn = to.std_ceil();
        if to_vpn >= from_vpn {
            return Ok(());
        }
        let heap_start_vpn = VirtAddr::from(self.start_brk()).std_ceil();
        let mut frames = Vec::new();
        let mut found = false;
        let mut empty_keys: Vec<VirtPageNum> = Vec::new();
        {
            let mut areas = self.areas.write();
            for (key, area_arc) in areas.iter() {
                let mut area = area_arc.lock();
                let a_start = area.vpn_range.get_start();
                let a_end = area.vpn_range.get_end();
                if a_start >= from_vpn || a_end <= to_vpn || a_start < heap_start_vpn {
                    continue;
                }
                found = true;
                if a_start < to_vpn {
                    // 区域跨越新的 brk 所在页：只删除 [to_vpn, a_end) 部分
                    let mut pt = self.page_table.write();
                    frames.extend(area.shrink_to(&mut pt, to_vpn));
                } else {
                    // 整块位于待收缩范围内
                    let mut pt = self.page_table.write();
                    frames.extend(area.unmap(&mut pt));
                    area.resize(a_start, a_start);
                }
                if area.vpn_range.get_start() >= area.vpn_range.get_end() {
                    empty_keys.push(*key);
                }
            }
            for key in empty_keys.iter() {
                areas.remove(key);
            }
            if found {
                brk.store(new_brk as u64, Ordering::Relaxed);
            }
        }
        if !found {
            return Err(Errno::ENOMEM.as_isize() as i32);
        }
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(frames);
        Ok(())
    }
    /// Assume that no conflicts.
    pub fn insert_framed_area(
        &self,
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
        &self,
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
        let key = area.vpn_range.get_start();
        self.areas.write().insert(key, Arc::new(Mutex::new(area)));
    }
    /// remove a area
    pub fn remove_area_with_start_vpn(&self, start_vpn: VirtPageNum) -> Vec<FrameTracker> {
        let mut areas = self.areas.write();
        if let Some(area_arc) = areas.remove(&start_vpn) {
            let mut area = area_arc.lock();
            let mut pt = self.page_table.write();
            let frames = area.unmap(&mut pt);
            drop(pt);
            drop(area);
            frames
        } else {
            Vec::new()
        }
    }
    /// Add a new MapArea into this MemorySet.
    /// Assuming that there are no conflicts in the virtual address
    /// space.
    pub fn push(&self, mut map_area: MapArea, data: Option<&[u8]>, start_va: usize) {
        let key = map_area.vpn_range.get_start();
        {
            let mut pt = self.page_table.write();
            map_area.map(&mut pt);
            if let Some(data) = data {
                map_area.copy_data(&mut pt, data, start_va);
            }
        }
        trace!(
            "map area: [{:#x}, {:#x}), {:?}",
            map_area.vpn_range.get_start().0 * PAGE_SIZE,
            map_area.vpn_range.get_end().0 * PAGE_SIZE,
            map_area.map_perm
        );
        self.areas.write().insert(key, Arc::new(Mutex::new(map_area)));
    }
    fn push_guard_area(&self, guard_start: usize, guard_pages: usize) {
        let area = MapArea::new(
            guard_start.into(),
            (guard_start + guard_pages * PAGE_SIZE).into(),
            MapType::Guard,
            MapPermission::empty(),
            PageSize::Page4K,
        );
        let key = area.vpn_range.get_start();
        self.areas.write().insert(key, Arc::new(Mutex::new(area)));
    }
    /// Mention that trampoline is not collected by areas.
    #[allow(unused)]
    #[cfg(target_arch = "riscv64")]
    fn map_trampoline(&self) {
        info!("mapping trampoline");
        self.page_table.write().map(
            VirtAddr::from(TRAMPOLINE).into(),
            PhysAddr::from(strampoline as *const () as usize & !crate::CACHED_KERNEL_BASE).into(),
            PTEFlags::R | PTEFlags::X,
            PageSize::Page4K, // 默认标准页大小
        );
    }

    // 用户态使用的跳板页（主要用于信号处理后恢复）
    fn map_user_trampoline(&self) {
        info!("mapping user trampoline");
        self.page_table.write().map(
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
        let memory_set = Self::new_bare();

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

        let memory_set = Self::new_bare();

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
        let stack_area = MapArea::new(
            user_stack_bottom.into(),
            user_stack_top.into(),
            MapType::Framed,
            MapPermission::R | MapPermission::W | MapPermission::U,
            PageSize::Page4K,
        );
        memory_set.areas.write().insert(
            stack_area.vpn_range.get_start(),
            Arc::new(Mutex::new(stack_area)),
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
                PageSize::Page4K,
            ),
            None,
            heap_bottom,
        );
        // 初始化堆起点与 program break（brk 从堆底开始，可随后用 brk/sbrk 增长）
        memory_set.start_brk.store(heap_bottom as u64, Ordering::Relaxed);
        memory_set.brk.store(heap_bottom as u64, Ordering::Relaxed);
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
    pub fn from_existed_user(user_space: &Self) -> Self {
        let memory_set = Self::new_bare();
        // 用户态信号恢复跳板
        memory_set.map_user_trampoline();

        // 快照父进程：只持有 areas 读锁；页表按“area -> PageTable”顺序逐段获取。
        let parent_areas = user_space.areas.read();

        // 复制用户拥有的段
        for area_arc in parent_areas.values() {
            let area = area_arc.lock();
            let map_type = area.map_type;
            // 采用共享内核页表之后实际上弃用，暂时保留接口
            if map_type == MapType::BorrowedKernel {
                continue;
            }
            // Guard areas contain metadata only and deliberately have no PTE.
            // Copying one through push() would call map_one(), while a guard
            // mapping is supposed to be a no-op rather than an allocation.
            if map_type == MapType::Guard {
                let new_area = MapArea::from_another(&area);
                let key = new_area.vpn_range.get_start();
                memory_set
                    .areas
                    .write()
                    .insert(key, Arc::new(Mutex::new(new_area)));
                continue;
            }
            let map_perm = area.map_perm;
            let is_shared = area.is_shared;
            let page_size = area.page_size;
            let vpn_range = area.vpn_range;
            // 优化：空的 PROT_NONE 区域不需要复制页表，跳过页表访问
            if (!map_perm.contains(MapPermission::U)
                || !map_perm.intersects(
                    MapPermission::R | MapPermission::W | MapPermission::X,
                ))
                && area.data_frames.is_empty()
            {
                let new_area = MapArea::from_another(&area);
                let key = new_area.vpn_range.get_start();
                memory_set
                    .areas
                    .write()
                    .insert(key, Arc::new(Mutex::new(new_area)));
                continue;
            }
            let share_user_pages = matches!(map_type, MapType::Framed | MapType::File);
            if share_user_pages {
                let mut new_area = MapArea::from_another(&area);
                // 只遍历父进程实际已分配的页
                let mapped_vpns: Vec<VirtPageNum> = area
                    .data_frames
                    .keys()
                    .copied()
                    .filter(|vpn| *vpn >= vpn_range.get_start() && *vpn < vpn_range.get_end())
                    .collect();
                for vpn in mapped_vpns {
                    let Some(src_pte) = user_space.page_table.read().translate(vpn) else {
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
                    memory_set.page_table.write().map(vpn, src_pte.ppn(), child_flags, page_size);
                    let frame_tracker = area
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
                            .write()
                            .set_flags(vpn, parent_flags, page_size);
                    }
                }
                let key = new_area.vpn_range.get_start();
                memory_set
                    .areas
                    .write()
                    .insert(key, Arc::new(Mutex::new(new_area)));
            } else {
                let new_area: MapArea = MapArea::from_another(&area);
                let start_va: VirtAddr = new_area.vpn_range.get_start().into();
                memory_set.push(new_area, None, start_va.0);
                let step = page_size.num_pages();
                let mut vpn = vpn_range.get_start();
                while vpn < vpn_range.get_end() {
                    if let Some(src_pte) = user_space.page_table.read().translate(vpn) {
                        if src_pte.is_valid() {
                            let src_ppn = src_pte.ppn();
                            let dst_ppn = {
                                let mut child_pt = memory_set.page_table.write();
                                if child_pt.translate(vpn).is_none()
                                    || !child_pt.translate(vpn).unwrap().is_valid()
                                {
                                    child_pt.translate_create(vpn, page_size);
                                }
                                child_pt.translate(vpn).unwrap().ppn()
                            };
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
        memory_set
            .brk
            .store(user_space.brk.load(Ordering::Relaxed), Ordering::Relaxed);
        memory_set.start_brk.store(
            user_space.start_brk.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        drop(parent_areas);
        #[cfg(target_arch = "riscv64")]
        memory_set.install_kernel_space();
        memory_set
    }
    /// 快照查找包含 vpn 的区域（areas 读锁内短暂锁 area，返回 Arc 后释放）
    fn find_area_arc(&self, vpn: VirtPageNum) -> Option<Arc<Mutex<MapArea>>> {
        let areas = self.areas.read();
        let (_, area_arc) = areas.range(..=vpn).next_back()?;
        let area_arc = area_arc.clone();
        if area_arc.lock().contains(vpn) {
            Some(area_arc)
        } else {
            None
        }
    }

    pub fn handle_cow_fault(&self, bad_addr: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();
        let Some(area_arc) = self.find_area_arc(vpn) else {
            return false;
        };
        let mut area = area_arc.lock();
        if area.map_type == MapType::Guard
            || area.is_shared
            || !area.map_perm.contains(MapPermission::W)
        {
            return false;
        }
        let page_size = area.page_size;
        let map_perm = area.map_perm;
        let mut pt = self.page_table.write();
        if let Some(pte) = pt.translate(vpn) {
            if pte.is_valid() && !pte.writable() {
                let old_ppn = pte.ppn();
                let new_frame = frame_alloc(page_size).unwrap();
                let new_ppn = new_frame.ppn;
                // 按实际页大小拷贝全部数据（大页需拷贝多个基本页）
                let num_pages = page_size.num_pages();
                for i in 0..num_pages {
                    PhysPageNum(new_ppn.0 + i)
                        .get_bytes_array()
                        .copy_from_slice(PhysPageNum(old_ppn.0 + i).get_bytes_array());
                }
                let pte_flags = PTEFlags::from_bits(map_perm.bits).unwrap();
                pt.set_entry(vpn, new_ppn, pte_flags);
                drop(pt);
                let old_frame = area.data_frames.insert(vpn, new_frame);
                #[cfg(target_arch = "loongarch64")]
                Self::flush_tlb_after_mapping_change();
                #[cfg(target_arch = "riscv64")]
                self.flush_tlb_targets();
                drop(old_frame);
                return true;
            }
        }
        false
    }
    //用于内核态给用户空间写入数据，判断是否是copy页时使用
    pub fn ensure_writable_user_range(&self, start: usize, len: usize, sp: usize) -> bool {
        if len == 0 {
            return true;
        }
        let page_size = {
            let areas = self.areas.read();
            let mut found = None;
            for area_arc in areas.values() {
                let area = area_arc.lock();
                if VirtAddr::from(start) >= area.vpn_range.get_start().into()
                    && VirtAddr::from(start) < area.vpn_range.get_end().into()
                {
                    found = Some(area.page_size);
                    break;
                }
            }
            match found {
                Some(ps) => ps,
                None => return false,
            }
        };
        let mut vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(start + len - 1).std_floor();
        loop {
            let page_start = vpn.0 * PAGE_SIZE;
            let ready = {
                let pt = self.page_table.read();
                match pt.translate(vpn) {
                    Some(pte) if pte.is_valid() && pte.writable() => true,
                    _ => false,
                }
            };
            if !ready {
                let valid = {
                    let pt = self.page_table.read();
                    pt.translate(vpn).map_or(false, |pte| pte.is_valid())
                };
                if valid {
                    if !self.handle_cow_fault(page_start) {
                        return false;
                    }
                } else {
                    if !self.handle_page_fault(page_start, sp) {
                        return false;
                    }
                    let ok = {
                        let pt = self.page_table.read();
                        match pt.translate(vpn) {
                            Some(pte) if pte.is_valid() && pte.writable() => true,
                            Some(pte) if pte.is_valid() => {
                                drop(pt);
                                self.handle_cow_fault(page_start)
                            }
                            _ => false,
                        }
                    };
                    if !ok {
                        return false;
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
    pub fn ensure_readable_user_range(&self, start: usize, len: usize, sp: usize) -> bool {
        if len == 0 {
            return true;
        }
        let page_size = {
            let areas = self.areas.read();
            let mut found = None;
            for area_arc in areas.values() {
                let area = area_arc.lock();
                if VirtAddr::from(start) >= area.vpn_range.get_start().into()
                    && VirtAddr::from(start) < area.vpn_range.get_end().into()
                {
                    found = Some(area.page_size);
                    break;
                }
            }
            match found {
                Some(ps) => ps,
                None => return false,
            }
        };

        let mut vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(start + len - 1).std_floor();
        loop {
            let page_start = vpn.0 * PAGE_SIZE;
            let ready = {
                let pt = self.page_table.read();
                match pt.translate(vpn) {
                    Some(pte) if pte.is_valid() && pte.readable() => true,
                    _ => false,
                }
            };
            if !ready {
                let valid = {
                    let pt = self.page_table.read();
                    pt.translate(vpn).map_or(false, |pte| pte.is_valid())
                };
                if valid {
                    return false;
                }
                if !self.handle_page_fault(page_start, sp) {
                    return false;
                }
                let ok = {
                    let pt = self.page_table.read();
                    pt.translate(vpn)
                        .map_or(false, |pte| pte.is_valid() && pte.readable())
                };
                if !ok {
                    return false;
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
        let pgdl = self.page_table.read().token();
        unsafe {
            asm!("csrwr {pgdl}, 0x19", pgdl = inout(reg) pgdl => _);
            asm!("dbar 0");
        }
    }

    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        let contains = {
            let areas = self.areas.read();
            areas.values().any(|a| a.lock().contains(vpn))
        };
        if !contains {
            return None;
        }
        self.page_table.read().translate(vpn)
    }
    pub fn translate_create(
        &self,
        vpn: VirtPageNum,
        page_size: PageSize,
    ) -> Option<PageTableEntry> {
        self.page_table.write().translate_create(vpn, page_size)
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn set_pte_dirty(&self, vpn: VirtPageNum) -> bool {
        let dirty = {
            let pt = self.page_table.read();
            if let Some((pte, _)) = pt.find_pte(vpn) {
                pte.set_dirty();
                true
            } else {
                false
            }
        };
        if dirty {
            Self::flush_tlb_after_mapping_change();
        }
        dirty
    }
    #[cfg(target_arch = "riscv64")]
    pub fn set_pte_dirty(&self, vpn: VirtPageNum) -> bool {
        let need_flush = {
            let pt = self.page_table.read();
            if let Some((pte, _)) = pt.find_pte(vpn) {
                let flags = pte.flags();
                if pte.is_valid() && pte.writable() && !flags.contains(PTEFlags::D) {
                    pte.set_dirty();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if need_flush {
            self.flush_tlb_targets();
        }
        need_flush
    }
    /// 写回共享映射页面内容
    pub fn sync_shared_pages(&self) {
        let areas = self.areas.read();
        for area_arc in areas.values() {
            area_arc.lock().sync_back_to_file();
        }
    }
    /// Remove all `MapArea`
    pub fn recycle_data_pages(&self) {
        let mut frames = Vec::new();
        {
            let mut areas = self.areas.write();
            let mut guards: Vec<_> = areas.values().map(|a| a.lock()).collect();
            let mut pt = self.page_table.write();
            for area in guards.iter_mut() {
                frames.extend(area.unmap(&mut pt));
            }
            drop(guards);
            areas.clear();
        }
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(frames);
    }
    /// shrink the area to new_end
    #[allow(unused)]
    pub fn shrink_to(&self, start: VirtAddr, new_end: VirtAddr) -> bool {
        let start_vpn = start.std_floor();
        let mut areas = self.areas.write();
        let Some(area_arc) = areas.get(&start_vpn) else {
            return false;
        };
        let mut area = area_arc.lock();
        let mut pt = self.page_table.write();
        let frames = area.shrink_to(&mut pt, new_end.std_ceil());
        drop(pt);
        drop(area);
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(frames);
        true
    }

    /// append the area to new_end
    #[allow(unused)]
    pub fn append_to(&self, start: VirtAddr, new_end: VirtAddr) -> bool {
        let start_vpn = start.std_floor();
        let mut areas = self.areas.write();
        let Some(area_arc) = areas.get(&start_vpn) else {
            return false;
        };
        let mut area = area_arc.lock();
        let mut pt = self.page_table.write();
        area.append_to(&mut pt, new_end.std_ceil());
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        true
    }

    /// 检查目标地址段是否与已有的映射冲突(存在交集)
    fn has_conflict(&self, start: usize, len: usize) -> bool {
        let target_start = VirtAddr::from(start).std_floor();
        let target_end = VirtAddr::from(start + len).std_ceil();
        let areas = self.areas.read();
        for area_arc in areas.values() {
            let area = area_arc.lock();
            if target_end > area.vpn_range.get_start() && target_start < area.vpn_range.get_end() {
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
        &self,
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

        // 文件分支提前取出 file，避免循环内 move。
        let file_opt = if is_anonymous {
            None
        } else {
            Some(file_inner.ok_or_else(|| Errno::EBADF.as_isize())?)
        };

        // 并发安全：addr==0 时多个线程可能同时选中同一空隙，插入时发现
        // 起始键已被占用则重试寻找下一个空隙（Linux 同样会在竞争后换地址）。
        let mut start_va = addr;
        for _attempt in 0..16 {
            // 找合适起始地址
            if start_va == 0 {
                if let Some(new_addr) = self.find_free_area(length) {
                    start_va = new_addr;
                } else {
                    error!(
                        "mmap failed: no suitable free area found for length {:#x}",
                        length
                    );
                    return Err(Errno::EEXIST.as_isize());
                }
            } else if self.has_conflict(start_va, length) {
                // 检查冲突
                if mmap_flags.contains(mmap::MMapFlags::MAP_FIXED) {
                    if self.munmap(start_va, length).is_err() {
                        return Err(Errno::EEXIST.as_isize());
                    }
                } else {
                    return Err(Errno::EEXIST.as_isize());
                }
            }

            let inserted = if is_anonymous {
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
                    area.anonymous_shared_frames =
                        Some(Arc::new(MPSafeCell::new(BTreeMap::new())));
                }
                let key = area.vpn_range.get_start();
                let mut areas = self.areas.write();
                if areas.contains_key(&key) {
                    false
                } else {
                    areas.insert(key, Arc::new(Mutex::new(area)));
                    true
                }
            } else {
                let file = file_opt.clone().unwrap();
                let mut area = MapArea::new(
                    VirtAddr::from(start_va),
                    VirtAddr::from(start_va + length),
                    MapType::File,
                    permission,
                    PageSize::Page4K,
                );
                area.backing_file = Some((file, page_offset));
                area.is_shared = is_shared;
                let key = area.vpn_range.get_start();
                let mut areas = self.areas.write();
                if areas.contains_key(&key) {
                    false
                } else {
                    areas.insert(key, Arc::new(Mutex::new(area)));
                    true
                }
            };

            if inserted {
                #[cfg(target_arch = "loongarch64")]
                Self::flush_tlb_after_mapping_change();
                return Ok(start_va);
            }

            // 并发撞地址：addr==0 时重新找空隙；MAP_FIXED 保留原地址重试。
            start_va = if addr == 0 { 0 } else { addr };
        }
        error!(
            "mmap failed: too many concurrent collisions for length {:#x}",
            length
        );
        Err(Errno::EEXIST.as_isize())
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

        let areas = self.areas.read();
        for area_arc in areas.values() {
            let area = area_arc.lock();
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
    pub fn munmap(&self, start: usize, length: usize) -> Result<(), isize> {
        let end = start + length;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        // 收集因从中间截断而产生的新右半部分区域
        let mut new_areas: Vec<MapArea> = Vec::new();
        let mut released_frames = Vec::new();
        let mut empty_keys: Vec<VirtPageNum> = Vec::new();
        // (旧键, 新键)：区域的起始地址被前移时，需要把 BTreeMap 键一起更新，
        // 否则键序与区域实际区间不一致，find_free_area 会算出“假空洞”。
        let mut rekey: Vec<(VirtPageNum, VirtPageNum)> = Vec::new();

        {
            let mut areas = self.areas.write();
            for (key, area_arc) in areas.iter() {
                let mut area = area_arc.lock();
                let a_start = area.vpn_range.get_start();
                let a_end = area.vpn_range.get_end();

                // 检查是否有交集
                if a_start < end_vpn && a_end > start_vpn {
                    let delete_left = a_start >= start_vpn; // 目标区域覆盖了当前块的左侧
                    let delete_right = a_end <= end_vpn; // 目标区域覆盖了当前块的右侧

                    let step = area.page_size.num_pages();
                    if delete_left && delete_right {
                        // 情况1：All（当前块被目标区域完全包裹，全部删掉）
                        let mut pt = self.page_table.write();
                        let mut vpn = a_start;
                        while vpn < a_end {
                            if let Some(frame) = area.unmap_one(&mut pt, vpn) {
                                released_frames.push(frame);
                            }
                            vpn.step_by(step);
                        }
                        area.resize(a_start, a_start); // 长度设为0，稍后统一清理
                        empty_keys.push(*key);
                    } else if !delete_left && !delete_right {
                        // 情况2：Split（目标区域在当前块中间，一分为二）
                        // 2.1 清理中间被 unmap 的页表和物理页
                        let mut pt = self.page_table.write();
                        for vpn in VPNRange::new(start_vpn, end_vpn) {
                            if let Some(frame) = area.unmap_one(&mut pt, vpn) {
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
                        let mut pt = self.page_table.write();
                        let mut vpn = a_start;
                        while vpn < end_vpn {
                            if let Some(frame) = area.unmap_one(&mut pt, vpn) {
                                released_frames.push(frame);
                            }
                            vpn.step_by(step);
                        }
                        area.resize(end_vpn, a_end);
                        rekey.push((*key, end_vpn));
                    } else if delete_right {
                        // 情况4：Inc_Right（删掉右边部分）
                        let mut pt = self.page_table.write();
                        let mut vpn = start_vpn;
                        while vpn < a_end {
                            if let Some(frame) = area.unmap_one(&mut pt, vpn) {
                                released_frames.push(frame);
                            }
                            vpn.step_by(step);
                        }
                        area.resize(a_start, start_vpn);
                    }
                }
            }

            // 删除长度为0的区域
            for key in empty_keys.iter() {
                areas.remove(key);
            }

            // 起始地址前移的区域：按新起始地址重新入键
            for (old_key, new_key) in rekey.iter() {
                if let Some(area_arc) = areas.remove(old_key) {
                    areas.insert(*new_key, area_arc);
                }
            }

            // 插入劈开产生的新区域
            for area in new_areas.drain(..) {
                let key = area.vpn_range.get_start();
                areas.insert(key, Arc::new(Mutex::new(area)));
            }
        }

        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();
        #[cfg(target_arch = "riscv64")]
        self.flush_tlb_targets();
        drop(released_frames);

        Ok(())
    }

    /// 丢弃驻留页但保留虚拟映射。匿名页重新分配为零页，私有文件页重新
    /// 从文件读取，共享文件页则在下次访问时重新映射对应的 page cache。
    pub fn madvise_dontneed(&self, start: usize, length: usize) -> Result<(), isize> {
        let end = start
            .checked_add(length)
            .ok_or_else(|| Errno::EINVAL.as_isize())?;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        let mut released_frames = Vec::new();
        {
            let areas = self.areas.read();
            for area_arc in areas.values() {
                let mut area = area_arc.lock();
                let area_start = area.vpn_range.get_start();
                let area_end = area.vpn_range.get_end();
                let discard_start = core::cmp::max(start_vpn, area_start);
                let discard_end = core::cmp::min(end_vpn, area_end);
                if discard_start >= discard_end || area.map_type == MapType::Guard {
                    continue;
                }

                let step = area.page_size.num_pages();
                let mut pt = self.page_table.write();
                let mut vpn = discard_start;
                while vpn < discard_end {
                    if let Some(frame) = area.unmap_one(&mut pt, vpn) {
                        released_frames.push(frame);
                    }
                    vpn.step_by(step);
                }
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
        &self,
        old_addr: usize,
        old_size: usize,
        new_size: usize,
    ) -> Result<usize, isize> {
        let old_start_vpn = VirtAddr::from(old_addr).std_floor();
        let old_end_vpn = VirtAddr::from(old_addr + old_size).std_ceil();
        let new_end_vpn = VirtAddr::from(old_addr + new_size).std_ceil();

        let area_arc = {
            let areas = self.areas.read();
            areas
                .values()
                .find(|a| {
                    let area = a.lock();
                    area.vpn_range.get_start().0 * PAGE_SIZE <= old_addr
                        && old_addr + old_size <= area.vpn_range.get_end().0 * PAGE_SIZE
                        && area.map_type == MapType::Framed
                })
                .cloned()
        }
        .ok_or(Errno::ENOMEM.as_isize())?;

        // 扩展区间必须空闲，否则无法原地扩大
        if self.has_conflict(old_addr + old_size, new_size - old_size) {
            return Err(Errno::ENOMEM.as_isize());
        }

        area_arc.lock().resize(old_start_vpn, new_end_vpn);
        Ok(old_addr)
    }

    fn split_area_at(
        areas: &mut crate::sync::RwLockWriteGuard<'_, BTreeMap<VirtPageNum, Arc<Mutex<MapArea>>>>,
        key: VirtPageNum,
        split_vpn: VirtPageNum,
    ) -> Result<Option<VirtPageNum>, isize> {
        let Some(area_arc) = areas.get(&key).cloned() else {
            return Ok(None);
        };
        let (area_start, area_end) = {
            let area = area_arc.lock();
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if split_vpn <= area_start || split_vpn >= area_end {
                return Ok(None);
            }
            if split_vpn.0 % area.page_size.num_pages() != 0 {
                return Err(Errno::EINVAL.as_isize());
            }
            (area_start, area_end)
        };
        let mut area = area_arc.lock();
        let right_frames = area.data_frames.split_off(&split_vpn);
        let mut right_area = area.clone_meta_with_new_range(split_vpn, area_end);
        right_area.data_frames = right_frames;
        area.resize(area_start, split_vpn);
        areas.insert(split_vpn, Arc::new(Mutex::new(right_area)));
        Ok(Some(split_vpn))
    }

    pub fn mprotect(
        &self,
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

        let pte_flags = PTEFlags::from_bits(permission.bits).unwrap();
        let mut covered_until = start_vpn;
        let mut affected: Vec<VirtPageNum> = Vec::new();
        let mut permissions_tightened = false;
        // 整个操作在 areas 写锁下完成：校验、分裂、改权限、改 PTE 之间不会被
        // 其他结构修改打断，同时避免“读锁校验 -> 再写锁修改”的多次全表扫描。
        let mut areas = self.areas.write();
        // 只扫描可能与 [start_vpn, end_vpn) 相交的区域：键 < end_vpn，
        // 且区域结束 > start_vpn。区域按键有序且互不重叠，因此从高到低
        // 找到第一个 end <= start_vpn 即可停止，避免给全部 area 上锁。
        for (key, area_arc) in areas.range(..end_vpn).rev() {
            let area = area_arc.lock();
            if area.vpn_range.get_end() <= start_vpn {
                break;
            }
            affected.push(*key);
        }
        // 按升序做覆盖性/权限校验
        affected.reverse();
        for key in &affected {
            let area = areas.get(key).unwrap().lock();
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
            if !(area.map_perm & !permission).is_empty() {
                permissions_tightened = true;
            }
            if area_end > covered_until {
                covered_until = area_end;
            }
        }
        if affected.is_empty() {
            return Err(Errno::ENOMEM.as_isize());
        }
        if covered_until < end_vpn {
            return Err(Errno::ENOMEM.as_isize());
        }

        // 先按边界分裂，使后续设置权限的区域边界与请求一致
        for key in affected.iter() {
            Self::split_area_at(&mut areas, *key, end_vpn)?;
            Self::split_area_at(&mut areas, *key, start_vpn)?;
        }

        // 分裂后，完整落在 [start_vpn, end_vpn) 内的区域其键必然在该区间内，
        // 只锁这些区域，不再遍历/锁全部 area。
        let target_arcs: Vec<Arc<Mutex<MapArea>>> =
            areas.range(start_vpn..end_vpn).map(|(_, a)| a.clone()).collect();
        let mut guards: Vec<_> = target_arcs.iter().map(|a| a.lock()).collect();
        let mut pt = self.page_table.write();
        for area in guards.iter_mut() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area_start >= start_vpn && area_end <= end_vpn {
                area.map_perm = permission;
                let step = area.page_size.num_pages();
                let mut vpn = area_start;
                while vpn < area_end {
                    if let Some(pte) = pt.translate(vpn) {
                        if pte.is_valid() {
                            pt.set_flags(vpn, pte_flags, area.page_size);
                        }
                    }
                    vpn.step_by(step);
                }
            }
        }
        drop(pt);
        drop(guards);
        drop(areas);

        #[cfg(target_arch = "riscv64")]
        if permissions_tightened {
            self.flush_tlb_targets();
        }
        #[cfg(target_arch = "loongarch64")]
        Self::flush_tlb_after_mapping_change();

        Ok(())
    }

    pub fn disable_share_in_range(&self, start: usize, length: usize) -> Result<(), isize> {
        if length == 0 {
            return Ok(());
        }

        let end = start
            .checked_add(length)
            .ok_or_else(|| Errno::EINVAL.as_isize())?;
        let start_vpn = VirtAddr::from(start).std_floor();
        let end_vpn = VirtAddr::from(end).std_ceil();

        let mut covered_until = start_vpn;
        let mut affected: Vec<VirtPageNum> = Vec::new();
        let mut areas = self.areas.write();
        for (key, area_arc) in areas.range(..end_vpn).rev() {
            let area = area_arc.lock();
            if area.vpn_range.get_end() <= start_vpn {
                break;
            }
            affected.push(*key);
        }
        affected.reverse();
        for key in &affected {
            let area = areas.get(key).unwrap().lock();
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
        if affected.is_empty() {
            return Err(Errno::ENOMEM.as_isize());
        }
        if covered_until < end_vpn {
            return Err(Errno::ENOMEM.as_isize());
        }

        for key in affected.iter() {
            Self::split_area_at(&mut areas, *key, end_vpn)?;
            Self::split_area_at(&mut areas, *key, start_vpn)?;
        }
        let target_arcs: Vec<Arc<Mutex<MapArea>>> =
            areas.range(start_vpn..end_vpn).map(|(_, a)| a.clone()).collect();
        let mut guards: Vec<_> = target_arcs.iter().map(|a| a.lock()).collect();
        for area in guards.iter_mut() {
            let area_start = area.vpn_range.get_start();
            let area_end = area.vpn_range.get_end();
            if area_start >= start_vpn && area_end <= end_vpn {
                area.is_shared = false;
            }
        }
        drop(guards);
        drop(areas);

        Ok(())
    }

    /// 处理缺页异常。如果触发异常的地址在合法区域内，则为其分配物理页；否则返回 false。
    /// 待进一步完善&测试
    #[no_mangle]
    #[inline(never)]
    pub fn handle_page_fault(&self, bad_addr: usize, sp: usize) -> bool {
        let vpn = VirtAddr::from(bad_addr).std_floor();

        // 遍历寻找包含该虚拟页号的段
        if let Some(area_arc) = self.find_area_arc(vpn) {
            let mut area = area_arc.lock();
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
            let page_size = area.page_size;

            // 检查该页是否已经在页表中映射
            {
                let pt = self.page_table.read();
                if let Some(pte) = pt.translate(vpn) {
                    if pte.is_valid() {
                        // 已经映射却还报 Fault，通常是非法写只读段
                        return false;
                    }
                }
            }

            // 按映射类型惰性分配物理页或映射文件页缓存
            {
                let mut pt = self.page_table.write();
                if !area.try_map_one(&mut pt, vpn, page_size) {
                    return false;
                }
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

        // 收集候选：紧贴 vpn 下方、且位于 sp 附近的栈区域（按起始页升序）
        let candidates: Vec<(VirtPageNum, Arc<Mutex<MapArea>>)> = {
            let areas = self.areas.read();
            let mut cands = Vec::new();
            for area_arc in areas.values() {
                let area = area_arc.lock();
                if area.map_type == MapType::Guard {
                    continue;
                }
                let start_vpn = area.vpn_range.get_start();
                // 检查缺页地址(vpn)是否紧贴着该段的下方，且距离不超过限制
                if vpn < start_vpn && (start_vpn.0 - vpn.0) <= MAX_EXPAND_PAGES {
                    // 进一步确认：缺页确实发生在栈指针(sp)附近
                    if vpn.0 <= sp_vpn.0 && (sp_vpn.0 - vpn.0) <= MAX_EXPAND_PAGES {
                        cands.push((start_vpn, area_arc.clone()));
                    }
                }
            }
            cands
        };

        for (old_start_vpn, area_arc) in candidates {
            // 扩张会改变区域起点，需要同时更新 BTreeMap 键，
            // 因此整个过程持有 areas 写锁（锁序：areas -> area -> PageTable）。
            let mut areas = self.areas.write();
            let mut area = area_arc.lock();
            // 重校验：区域未被并发修改
            let start_vpn = area.vpn_range.get_start();
            if start_vpn != old_start_vpn {
                continue;
            }
            if vpn >= start_vpn || (start_vpn.0 - vpn.0) > MAX_EXPAND_PAGES {
                continue;
            }
            if !(vpn.0 <= sp_vpn.0 && (sp_vpn.0 - vpn.0) <= MAX_EXPAND_PAGES) {
                continue;
            }
            let page_size = area.page_size;
            // 执行扩张：将该 Area 的起点向下延伸到 vpn (注意保留原来的终点)
            area.vpn_range = VPNRange::new(vpn, area.vpn_range.get_end());
            // 键随起始地址前移，保持“键 == 区域起始页”的不变量
            if vpn != old_start_vpn {
                areas.remove(&old_start_vpn);
                areas.insert(vpn, area_arc.clone());
            }

            // 为刚刚扩张出来的这些虚拟页（从 vpn 到 old_start_vpn）全部分配物理帧并映射
            let mut pt = self.page_table.write();
            for v in vpn.0..old_start_vpn.0 {
                area.map_one(&mut pt, VirtPageNum::from(v), page_size);
            }
            drop(pt);
            drop(area);
            drop(areas);
            #[cfg(target_arch = "loongarch64")]
            Self::flush_tlb_after_mapping_change();
            // trace!("[kernel] User stack dynamically expanded down to {:#x}", bad_addr);
            return true; // 栈扩张修复成功！
        }

        // 既不在合法区域，也不符合栈扩张规则，野指针/溢出
        false
    }

    pub fn debug_dump_areas(&self, badv: Option<usize>, era: Option<usize>) {
        let areas = self.areas.read();
        let heap_start_vpn =
            VirtAddr::from(self.start_brk.load(Ordering::Relaxed) as usize).std_ceil();
        let brk_end_vpn = VirtAddr::from(self.brk.load(Ordering::Relaxed) as usize).std_ceil();
        println!(
            "[kernel] memory_set: asid={}, start_brk=0x{:x}, brk=0x{:x}, area_count={}",
            self.asid.0,
            self.start_brk.load(Ordering::Relaxed),
            self.brk.load(Ordering::Relaxed),
            areas.len()
        );
        for (idx, area_arc) in areas.values().enumerate() {
            let area = area_arc.lock();
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
        self.page_table.read().translate(vpn).map_or(false, |pte| {
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
        if let Some(pte) = self.page_table.read().translate(vpn) {
            if pte.is_valid() {
                return false;
            }
        }
        let areas = self.areas.read();
        for area_arc in areas.values() {
            let area = area_arc.lock();
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
            let frames = self.page_table.write().take_frames();
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
    let kernel_space = &KERNEL_SPACE;
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
    let pt = kernel_space.page_table.read();
    assert!(!pt.translate(mid_text.std_floor()).unwrap().writable(),);
    assert!(!pt.translate(mid_rodata.std_floor()).unwrap().writable(),);
    assert!(!pt.translate(mid_data.std_floor()).unwrap().executable(),);
    println!("remap_test passed!");
}
