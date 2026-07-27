// VisionFive2 内存空间布局信息

/// 主要内存起始地址 (JH7110 DRAM), 注意linker.ld需要与此同步
pub const MEMORY_BASE: usize = 0x4000_0000;
/// 内存大小 (典型配置 2GB)
pub const MEMORY_SIZE: usize = 0x8000_0000;
/// the physical memory end
pub const MEMORY_END: usize = MEMORY_BASE + MEMORY_SIZE;

pub const MMIO: &[(usize, usize)] = &[
    (0x1000_0000, 0x0001_0000),
    (0x1302_0000, 0x0001_0000),
    (0x1602_0000, 0x0001_0000),
    (0x1603_0000, 0x0001_0000),
    (0x1700_0000, 0x0001_0000),
    (0x1701_0000, 0x0000_1000),
];
