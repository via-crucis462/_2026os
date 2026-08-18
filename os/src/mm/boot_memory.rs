//! 启动阶段的物理内存发现与布局划分。
//!
//! 本模块在堆分配器启用之前运行，因此只能使用静态存储和无分配的设备树解析。
//! 布局统一采用物理地址记录，其他模块需要访问内存时再转换到内核窗口地址。

use core::cmp::min;

use spin::Once;

use crate::arch::config::{CACHED_KERNEL_BASE, CPU_CORE_NUM, DMA_SIZE, PAGE_SIZE};

/// 堆的目标大小为主物理内存区的四分之一，并受上下限约束。
const HEAP_MEMORY_FRACTION: usize = 4;
/// 内核堆不得小于 128 MiB。
const MIN_HEAP_SIZE: usize = 128 << 20;
/// 内核堆不得大于 1.5 GiB。
const MAX_HEAP_SIZE: usize = 0x6000_0000;
/// 即使堆需要缩小，也至少为页帧分配器保留 32 MiB。
const MIN_FRAME_POOL_SIZE: usize = 32 << 20;

/// 主核初始化后确定的全局物理内存布局，所有区间均为左闭右开。
#[derive(Clone, Copy, Debug)]
pub struct BootMemory {
    /// 包含内核镜像的设备树主内存区起点。
    pub memory_start: usize,
    /// 包含内核镜像的设备树主内存区终点。
    pub memory_end: usize,
    /// 为设备连续 DMA 分配预留的区间起点。
    pub dma_start: usize,
    /// 为设备连续 DMA 分配预留的区间终点。
    pub dma_end: usize,
    /// 内核全局堆占用的物理区间起点。
    pub heap_start: usize,
    /// 内核全局堆占用的物理区间终点。
    pub heap_end: usize,
    /// 页帧分配器可以管理的物理区间起点。
    pub frame_start: usize,
    /// 页帧分配器可以管理的物理区间终点。
    pub frame_end: usize,
}

/// 布局只允许由主核初始化一次，其他模块只读共享。
static BOOT_MEMORY: Once<BootMemory> = Once::new();

#[inline]
const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[inline]
const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}

#[cfg(all(target_arch = "loongarch64", board = "virt"))]
/// 从 QEMU LoongArch 传入的 EFI system table 中查找设备树物理地址。
fn device_tree_address(efi_system_table: usize) -> usize {
    // 这些偏移来自 UEFI 的 efi_system_table 与 efi_configuration_table 布局。
    // 使用显式偏移可以避免在启动 ABI 上引入带平台填充差异的 Rust 结构体。
    const EFI_SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
    const EFI_NR_TABLES_OFFSET: usize = 104;
    const EFI_TABLES_OFFSET: usize = 112;
    const EFI_CONFIGURATION_TABLE_SIZE: usize = 24;
    const DEVICE_TREE_GUID: [u8; 16] = [
        0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa,
        0xe0,
    ];

    assert_ne!(
        efi_system_table, 0,
        "LoongArch virt boot did not provide an EFI system table"
    );
    // EFI 表内保存的是物理地址，CPU 读取时必须抬入 LoongArch 缓存窗口。
    let system_table = efi_system_table | CACHED_KERNEL_BASE;
    let signature = unsafe { (system_table as *const u64).read_unaligned() };
    assert_eq!(
        signature, EFI_SYSTEM_TABLE_SIGNATURE,
        "invalid EFI system table at {:#x}",
        efi_system_table
    );
    let table_count =
        unsafe { ((system_table + EFI_NR_TABLES_OFFSET) as *const u64).read_unaligned() as usize };
    let tables =
        unsafe { ((system_table + EFI_TABLES_OFFSET) as *const u64).read_unaligned() as usize };
    assert!(table_count <= 64, "invalid EFI configuration table count");

    for index in 0..table_count {
        let entry = (tables | CACHED_KERNEL_BASE) + index * EFI_CONFIGURATION_TABLE_SIZE;
        let guid =
            unsafe { core::slice::from_raw_parts(entry as *const u8, DEVICE_TREE_GUID.len()) };
        if guid == DEVICE_TREE_GUID {
            return unsafe { ((entry + DEVICE_TREE_GUID.len()) as *const u64).read_unaligned() }
                as usize;
        }
    }
    panic!("EFI system table has no device tree entry");
}

#[cfg(all(target_arch = "riscv64", board = "virt"))]
/// RISC-V 启动 ABI 已经在 a1 中直接传入 DTB 物理地址。
fn device_tree_address(boot_dtb: usize) -> usize {
    boot_dtb
}

#[cfg(board = "virt")]
/// 从设备树选择包含当前内核镜像的主内存区。
///
/// LoongArch virt 有低端和高端两个 memory 节点，因此不能简单取第一个节点；
/// 必须用链接符号对应的镜像物理区间筛选出内核实际所在的内存区。
fn platform_memory_range(
    boot_info: usize,
    image_start: usize,
    image_end: usize,
) -> (usize, usize, usize) {
    let boot_dtb = device_tree_address(boot_info);
    assert_ne!(boot_dtb, 0, "virt boot did not provide a device tree");
    // RISC-V 早期页表保留了物理恒等映射；LoongArch 则通过 DMW 窗口访问 DTB。
    #[cfg(target_arch = "riscv64")]
    let fdt_address = boot_dtb;
    #[cfg(target_arch = "loongarch64")]
    let fdt_address = boot_dtb | CACHED_KERNEL_BASE;
    let fdt = unsafe { fdt::Fdt::from_ptr(fdt_address as *const u8) }
        .unwrap_or_else(|error| panic!("invalid boot device tree at {:#x}: {}", boot_dtb, error));

    let (memory_start, memory_end) = fdt
        .all_nodes()
        .filter(|node| {
            node.property("device_type")
                .and_then(|property| property.as_str())
                == Some("memory")
        })
        .filter_map(|node| {
            node.reg()?.find_map(|region| {
                let start = region.starting_address as usize;
                let end = start.checked_add(region.size?)?;
                (start <= image_start && image_end <= end).then_some((start, end))
            })
        })
        .next()
        .unwrap_or_else(|| {
            panic!(
                "device tree has no memory region containing kernel [{:#x}, {:#x})",
                image_start, image_end
            )
        });

    // 当前页帧分配器只支持一个连续区间。遇到内核之后的首个固件保留区时，
    // 将可用上界截断到该保留区之前，防止把固件占用页面交给页帧分配器。
    let mut usable_end = memory_end;
    for reservation in fdt.memory_reservations() {
        let start = reservation.address() as usize;
        if image_end <= start && start < usable_end {
            usable_end = start;
        }
    }

    (memory_start, memory_end, usable_end)
}

#[cfg(not(board = "virt"))]
/// 物理板仍使用板级配置，但在这里统一去掉内核窗口位，转换为物理地址。
fn platform_memory_range(
    _boot_dtb: usize,
    _image_start: usize,
    _image_end: usize,
) -> (usize, usize, usize) {
    let start = crate::arch::config::MEMORY_BASE & !CACHED_KERNEL_BASE;
    let end = crate::arch::config::MEMORY_END & !CACHED_KERNEL_BASE;
    (start, end, end)
}

/// 初始化唯一的启动内存布局，必须在堆和页帧分配器之前调用。
pub fn init(boot_info: usize) {
    extern "C" {
        fn skernel();
        fn ekernel();
    }

    BOOT_MEMORY.call_once(|| {
        // 链接符号可能位于高半地址或 LoongArch DMW 窗口，布局计算只使用物理地址。
        let image_start = skernel as *const () as usize & !CACHED_KERNEL_BASE;
        let image_end = align_up(
            ekernel as *const () as usize & !CACHED_KERNEL_BASE,
            PAGE_SIZE,
        );
        let (memory_start, memory_end, usable_end) =
            platform_memory_range(boot_info, image_start, image_end);
        #[cfg(target_arch = "riscv64")]
        // Sv39 高半窗口最多表示 256 GiB 物理地址，超出后窗口地址会发生混叠。
        assert!(
            memory_end <= 1usize << 38,
            "RAM end {:#x} exceeds the Sv39 kernel window",
            memory_end
        );

        #[cfg(all(target_arch = "riscv64", board = "visionfive2"))]
        let frame_end = min(usable_end, crate::arch::config::FRAME_ALLOC_END);
        #[cfg(not(all(target_arch = "riscv64", board = "visionfive2")))]
        let frame_end = usable_end;

        assert!(
            memory_start <= image_start && image_end <= frame_end,
            "kernel image [{:#x}, {:#x}) is outside usable memory [{:#x}, {:#x})",
            image_start,
            image_end,
            memory_start,
            frame_end
        );

        // 按固定顺序划分，确保 DMA、堆和页帧池互不重叠：
        // [内核镜像][DMA 预留][动态堆][页帧池]。
        let dma_start = image_end;
        let dma_end = align_up(
            dma_start.checked_add(DMA_SIZE).expect("DMA range overflow"),
            PAGE_SIZE,
        );
        let allocatable = frame_end
            .checked_sub(dma_end)
            .expect("kernel image and DMA reservation exceed available memory");
        let memory_size = memory_end - memory_start;
        // 先按总物理内存计算目标值，再应用 128 MiB/1.5 GiB 上下限。
        let desired_heap = (memory_size / HEAP_MEMORY_FRACTION)
            .clamp(MIN_HEAP_SIZE, MAX_HEAP_SIZE);
        // 如果目标堆会侵占最小页帧池，则缩到当前机器实际能容纳的大小；
        // 但机器必须至少能够同时容纳最小堆和最小页帧池。
        let max_heap = allocatable
            .checked_sub(MIN_FRAME_POOL_SIZE)
            .expect("not enough memory for the minimum frame pool");
        assert!(
            max_heap >= MIN_HEAP_SIZE,
            "not enough memory for the minimum heap and frame pool"
        );
        let heap_size = align_down(min(desired_heap, max_heap), PAGE_SIZE);
        let minimum_arenas = PAGE_SIZE * (CPU_CORE_NUM + 1);
        assert!(
            heap_size >= minimum_arenas,
            "heap is too small for {} local arenas and one large arena",
            CPU_CORE_NUM
        );

        let heap_start = dma_end;
        let heap_end = heap_start + heap_size;
        let layout = BootMemory {
            memory_start,
            memory_end,
            dma_start,
            dma_end,
            heap_start,
            heap_end,
            frame_start: heap_end,
            frame_end,
        };

        println!(
            "[memory] RAM [{:#x}, {:#x}), DMA [{:#x}, {:#x}), heap [{:#x}, {:#x}), frames [{:#x}, {:#x})",
            layout.memory_start,
            layout.memory_end,
            layout.dma_start,
            layout.dma_end,
            layout.heap_start,
            layout.heap_end,
            layout.frame_start,
            layout.frame_end,
        );
        layout
    });
}

/// 获取已经初始化的启动内存布局。
pub fn boot_memory() -> &'static BootMemory {
    BOOT_MEMORY
        .get()
        .expect("boot memory layout used before initialization")
}

/// 返回包含内核镜像的主物理内存区大小，供 `/proc/meminfo` 等接口使用。
pub fn memory_size() -> usize {
    let memory = boot_memory();
    memory.memory_end - memory.memory_start
}
