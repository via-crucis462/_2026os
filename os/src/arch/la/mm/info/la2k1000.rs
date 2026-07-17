//! 2k1000 星云板 内存空间布局信息
//! 内存段无特殊说明则：起始包含，结束不包含（前闭后开区间）

use lazy_static::lazy_static;

/*
boot_params = 0x900000000cc17740

DRAM bank   = 0x0000000000000000
-> start    = 0x9000000000000000
-> size     = 0x0000000010000000

DRAM bank   = 0x0000000000000001                                                                                                                                                                                                   
-> start    = 0x9000000090000000                                                                                                                                                                                                   
-> size     = 0x0000000070000000 

flashstart  = 0x0000000000000000                                                                                                                                                                                                   
flashsize   = 0x0000000000000000                                                                                                                                                                                                   
flashoffset = 0x0000000000000000                                                                                                                                                                                                   
baudrate    = 115200 bps                                                                                                                                                                                                           
relocaddr   = 0x900000000dc00000                                                                                                                                                                                                   
reloc off   = 0x0000000000000000                                                                                                                                                                                                   
Build       = 64-bit                                                                                                                                                                                                               
current eth = ethernet@40040000                                                                                                                                                                                                    
ethaddr     = 26:b2:19:ae:43:08                                                                                                                                                                                                    
IP addr     = 192.168.1.20                                                                                                                                                                                                         
fdt_blob    = 0x900000000eccf480                                                                                                                                                                                                   
new_fdt     = 0x900000000cbf80f0                                                                                                                                                                                                   
fdt_size    = 0x0000000000003ce0                                                                                                                                                                                                   
Video       = dvo@1 active                                                                                                                                                                                                         
FB base     = 0x900000000dc00000                                                                                                                                                                                                   
FB size     = 1024x600x32

lmb_dump_all:                                                                                                                                                                                                                      
 memory.cnt  = 0x2                                                                                                                                                                                                                 
 memory[0]      [0x9000000000000000-0x900000000fffffff], 0x10000000 bytes flags: 0                                                                                                                                                 
 memory[1]      [0x9000000090000000-0x90000000ffffffff], 0x70000000 bytes flags: 0                                                                                                                                                 
 reserved.cnt  = 0x2                                                                                                                                                                                                               
 reserved[0]    [0x900000000cbf4c90-0x900000000ebfffff], 0x0200b370 bytes flags: 0                                                                                                                                                 
 reserved[1]    [0x900000000f000000-0x900000000fffffff], 0x01000000 bytes flags: 4

devicetree  = board

*/


/// DRAM 物理地址
/// 
/// Bank0 起始物理地址 (DDR3 低 256MB)
pub const DRAM_BANK0_START: usize = 0x9000_0000_0000_0000;
/// Bank0 大小
pub const DRAM_BANK0_SIZE: usize = 0x1000_0000; // 256 MB
/// Bank0 结束地址
pub const DRAM_BANK0_END: usize = DRAM_BANK0_START + DRAM_BANK0_SIZE;
/// Bank1 起始物理地址 (DDR3 高 1792MB)
pub const DRAM_BANK1_START: usize = 0x9000_0000_9000_0000;
/// Bank1 大小
pub const DRAM_BANK1_SIZE: usize = 0x7000_0000; // 1792 MB
/// Bank1 结束地址
pub const DRAM_BANK1_END: usize = DRAM_BANK1_START + DRAM_BANK1_SIZE;

/// 总 DRAM 大小
pub const DRAM_TOTAL_SIZE: usize = DRAM_BANK0_SIZE + DRAM_BANK1_SIZE; // 2 GB


/// MMIO
/// 
/// MMIO 窗口起始 (Bank0 结束后)
pub const MMIO_HOLE_START: usize = DRAM_BANK0_END;
/// MMIO 窗口结束 (Bank1 开始前)
pub const MMIO_HOLE_END: usize = DRAM_BANK1_START;
/// MMIO 窗口大小
pub const MMIO_HOLE_SIZE: usize = MMIO_HOLE_END - MMIO_HOLE_START; // 2 GB


/// 保留区
/// 
/// U-Boot 保留区起始 (含 U-Boot 重定位、设备树、帧缓冲等)
pub const UBOOT_RESERVED_START: usize = 0x9000_0000_0CBF_4C90;
/// U-Boot 保留区结束
pub const UBOOT_RESERVED_END: usize = 0x9000_0000_0EC0_0000;
/// U-Boot 保留区大小
pub const UBOOT_RESERVED_SIZE: usize = UBOOT_RESERVED_END - UBOOT_RESERVED_START; // ~33 MB
/// 第二保留区 (Bank0 顶部 16MB)
pub const UBOOT_RESERVED2_START: usize = 0x9000_0000_0F00_0000;
/// 第二保留区结束
pub const UBOOT_RESERVED2_END: usize = 0x9000_0000_1000_0000;
/// 第二保留区大小
pub const UBOOT_RESERVED2_SIZE: usize = UBOOT_RESERVED2_END - UBOOT_RESERVED2_START; // 16 MB


/// 其他
/// 
/// U-Boot 重定位地址 (= FB base, 共享同一地址)
pub const UBOOT_RELOC_ADDR: usize = 0x9000_0000_0DC0_0000;
/// 设备树 (FDT) 地址
pub const FDT_BLOB_ADDR: usize = 0x9000_0000_0ECC_F480;
/// 新设备树地址 (U-Boot 修改后的)
pub const NEW_FDT_ADDR: usize = 0x9000_0000_0CBF_80F0;
/// 帧缓冲基址
pub const FB_BASE: usize = 0x9000_0000_0DC0_0000;
/// 帧缓冲大小: 1024×600×32bpp
pub const FB_SIZE: usize = 1024 * 600 * 4; // 2.34 MB


/// 外设寄存器
/// 用deepseek v4从手册中提取，待验证
/// 
/// SATA 控制器 (Dev 8, Fun 0), 兼容 AHCI 1.1
/// SATA PCI 配置头 (芯片级, 不建议直接访问)
pub const SATA_PCI_CFG_HEADER: usize = 0x1fe0_3240;
/// SATA PHY 配置寄存器 (芯片级, PLL/电气特性)
pub const SATA_PHY_CFG: usize = 0x1fe0_0460;
/// 获取 sata 控制器基址
lazy_static!(
    pub static ref SATA_BASE: usize = {
        use super::super::super::drivers::pci;
        let loc = pci::Location::new(SATA_PCI_CFG_HEADER, 0, 8, 0);
        0
    };
);

/// GMAC0 以太网控制器 (从 U-Boot "current eth" 可知)
pub const GMAC0_BASE: usize = 0x4004_0000;
/// GMAC0 PCI 配置头
pub const GMAC0_PCI_CFG_HEADER: usize = 0x1fe0_3040;


/// PCI 配置空间 & MMIO
///
/// 2K1000 手册表6-2, 32位模式 Type0 配置空间基址 (物理地址)
pub const PCI_CONFIG_SPACE_BASE: usize = 0x1A00_0000;
/// PCI/PCIE MEM 空间基址 (手册表6-1)
pub const PCI_MMIO_BASE: usize = 0x4000_0000;
/// MMIO 范围, 通过 DMW 窗口映射到虚拟地址空间
pub const MMIO: &[(usize, usize)] = &[
    (0x8000_0000_1FE0_0000, 0x0020_0000), // 芯片配置寄存器空间 (2MB)
    (0x8000_0000_1000_0000, 0x0100_0000), // IO 设备 MEM 空间 (16MB)
    (0x8000_0000_1A00_0000, 0x0100_0000), // PCI 配置空间 (Type0+Type1, 16MB)
    (0x8000_0000_4000_0000, 0x0100_0000), // PCIe MEM 空间
];


/// 供内核使用
/// 
/// 实际可用的 Bank0
pub const BANK0_START_EFFECTIVE: usize = DRAM_BANK0_START;
const BANK0_END_NO_RESERVED: usize = UBOOT_RESERVED_START;
const BANK0_END_NO_RESERVED_ALIGN: usize = BANK0_END_NO_RESERVED & !(super::super::super::config::PAGE_SIZE - 1); // 对齐到页大小
pub const BANK0_END_EFFECTIVE: usize = BANK0_END_NO_RESERVED_ALIGN;
/// Bank1
pub const BANK1_START_EFFECTIVE: usize = DRAM_BANK1_START;
pub const BANK1_END_EFFECTIVE: usize = DRAM_BANK1_END;

