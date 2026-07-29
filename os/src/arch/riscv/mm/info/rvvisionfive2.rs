//! VisionFive 2 (StarFive JH7110) 内存空间布局信息。

/// DRAM 物理起始地址，必须与 linker-visionfive2.ld 保持一致。
pub const MEMORY_BASE: usize = 0x4000_0000;
///  DRAM 大小：4 GiB。
pub const MEMORY_SIZE: usize = 0x1_0000_0000;
/// DRAM 物理结束地址（不包含）。
pub const MEMORY_END: usize = MEMORY_BASE + MEMORY_SIZE;

///  OpenSBI 保留区。
pub const OPENSBI_RESERVED_START: usize = 0x4000_0000;
pub const OPENSBI_RESERVED_SIZE: usize = 0x0008_0000;
pub const OPENSBI_RESERVED_END: usize = OPENSBI_RESERVED_START + OPENSBI_RESERVED_SIZE;

///  U-Boot 重定位地址。
pub const UBOOT_RELOC_ADDR: usize = 0xfff4_4000;
/// 设备树地址。
pub const FDT_BLOB_ADDR: usize = 0xfffc_56a0;

/// 物理页分配器的连续上限（结束地址，不包含）。
/// 在获得更完整的固件保留区描述前，页分配器只使用该地址以下的连续 DRAM。
pub const FRAME_ALLOC_END: usize = UBOOT_RELOC_ADDR;

/// VisionFive 2 UART0。
pub const UART0_BASE: usize = 0x1000_0000;
pub const UART0_SIZE: usize = 0x0001_0000;
/// DesignWare SDIO1 控制器。
pub const SDIO1_BASE: usize = 0x1602_0000;
pub const SDIO1_SIZE: usize = 0x0001_0000;
/// JH7110 DWMAC0 以太网控制器。
pub const DWMAC0_BASE: usize = 0x1603_0000;
pub const DWMAC0_SIZE: usize = 0x0001_0000;
/// JH7110 RTC 控制器。
pub const RTC_BASE: usize = 0x1704_0000;
pub const RTC_SIZE: usize = 0x0001_0000;

pub const MMIO: &[(usize, usize)] = &[
    (UART0_BASE, UART0_SIZE),
    (0x1302_0000, 0x0001_0000), // SYS_CRG
    (SDIO1_BASE, SDIO1_SIZE),
    (DWMAC0_BASE, DWMAC0_SIZE),
    (0x1700_0000, 0x0001_0000), // AON_CRG
    (0x1701_0000, 0x0000_1000),
    (RTC_BASE, RTC_SIZE),
];
