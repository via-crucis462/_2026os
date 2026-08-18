//! qemu-virt (la64) 的固定低端内存与 MMIO 布局
//! 主内存区间在启动时从设备树发现。

/*
QEMU virt 使用常见的 36 GiB 配置时会暴露两个物理内存区：
    lowram  [0x0,         0x1000_0000)
    highram [0x8000_0000, 0x9_7000_0000)
固件占用 lowram 的前 2 MiB，内核使用其余 lowram 放置内核栈。
highram 区间会随 QEMU 的 `-m` 参数变化。
*/


/// DRAM 物理地址
/// 
/// Bank0 起始物理地址（固件之后）
pub const DRAM_BANK0_START: usize = 0x20_0000;
/// Bank0 结束地址
pub const DRAM_BANK0_END: usize = 0x1000_0000;
/// Bank0 大小
pub const DRAM_BANK0_SIZE: usize = DRAM_BANK0_END - DRAM_BANK0_START;

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
