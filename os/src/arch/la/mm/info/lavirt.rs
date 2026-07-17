//! qemu-virt (la64) 内存空间布局信息
//! 内存段无特殊说明则：起始包含，结束不包含（前闭后开区间）

/*
实际打印 qemu ram 发现, la 的物理地址 1G 并不从 elf 起点开始连续,
而是从 0x0 - 约 0x20_0000 给固件,
0x20_0000 - 0x800_0000 有一段连续的内存,
剩下的从 0x8000_0000 开始, 连续 768MB 是主要内存空间。

因此暂时:
让内核只用 highram, 地址从 0x8000_0000 开始,
低部分有约 254MB, 留给内核栈。
*/


/// DRAM 物理地址
/// 
/// Bank0 起始物理地址 (固件之后, 低 254MB)
pub const DRAM_BANK0_START: usize = 0x20_0000;
/// Bank0 结束地址
pub const DRAM_BANK0_END: usize = 0x800_0000;
/// Bank0 大小
pub const DRAM_BANK0_SIZE: usize = DRAM_BANK0_END - DRAM_BANK0_START; // 254 MB

/// Bank1 起始物理地址 (内核和帧分配器使用, 高 768MB)
pub const DRAM_BANK1_START: usize = 0x8000_0000;
/// Bank1 大小
pub const DRAM_BANK1_SIZE: usize = 0x3000_0000; // 768 MB
/// Bank1 结束地址
pub const DRAM_BANK1_END: usize = DRAM_BANK1_START + DRAM_BANK1_SIZE; // 0xB000_0000

/// 总 DRAM 大小 (1 GB)
pub const DRAM_TOTAL_SIZE: usize = DRAM_BANK0_SIZE + DRAM_BANK1_SIZE; // ~1022 MB ≈ 1 GB


/// 固件保留区
/// 
/// 固件起始 (物理地址 0x0)
pub const FIRMWARE_START: usize = 0x0;
/// 固件结束
pub const FIRMWARE_END: usize = DRAM_BANK0_START; // 0x20_0000
/// 固件大小
pub const FIRMWARE_SIZE: usize = FIRMWARE_END - FIRMWARE_START; // 2 MB


/// MMIO (PCI 等设备)
/// 
/// 查看 qemu 的源代码可以知道配置空间的基地址为 0x2000_0000，不写成虚拟地址
pub const PCI_CONFIG_SPACE_BASE: usize = 0x2000_0000;
/// MMIO 基址设置为 0x4000_0000 起，改成更低的地址会有问题，ai 解释是 qemu 规定这里开始才是合法的 MMIO 地址
pub const PCI_MMIO_BASE: usize = 0x4000_0000;
/// MMIO 范围，并非真正的内存，cpu 尝试访问这些地址相当于给设备发信号
/// 这些地址通过 DMW 窗口映射到虚拟地址空间
/// （以下为虚拟地址，物理 MMIO 在 0x1000_0000 / 0x2000_0000 / 0x4000_0000）
pub const MMIO: &[(usize, usize)] = &[
    (0x8000_0000_1000_0000, 0x1000_0000), // 留给 UART
    (0x8000_0000_2000_0000, 0x1000_0000), // PCI 配置空间
    (0x8000_0000_4000_0000, 0x1000_0000), // PCI MMIO
];


/// 供内核使用
/// 
/// 实际可用的 Bank0
pub const BANK0_START_EFFECTIVE: usize = DRAM_BANK0_START;
pub const BANK0_END_EFFECTIVE: usize = DRAM_BANK0_END;
/// Bank1
pub const BANK1_START_EFFECTIVE: usize = DRAM_BANK1_START;
pub const BANK1_END_EFFECTIVE: usize = DRAM_BANK1_END;