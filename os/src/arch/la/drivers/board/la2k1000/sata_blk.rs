//! SATA 块设备驱动，实现了对 AHCI 控制器的访问和对 SATA 磁盘的读写操作
//! 各枚举定义了 AHCI 控制器和端口的寄存器偏移
//! 寄存器偏移的具体值参考 AHCI 规范和 llm 工具

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    hint::spin_loop,
    sync::atomic::{fence, Ordering},
};
use spin::Mutex;

use crate::{
    arch::{
        config::{PAGE_SIZE, SATA_AHCI_MMIO_PA, UNCHACHED_KERNEL_BASE},
        drivers::dma::{DmaBuffer, QUEUE_FRAMES},
        timer::get_time_ms,
    }, ext4fs::BLOCK_SZ, mm::PhysAddr
};
use lazy_static::lazy_static;



lazy_static! {
    /// AHCI 控制器实例
    pub static ref AHCI_CONTROLLER: Mutex<AHCIController> = Mutex::new(
        AHCIController::new(*SATA_AHCI_MMIO_PA)
    );
}

lazy_static! {
    /// 全局唯一 SATA 块设备实例
    pub static ref SATA_BLOCK: Arc<SataBlock> = {
        Arc::new(SataBlock::new())
    };
}


/// SATA 块设备
pub struct SataBlock {
    /// 全局唯一 AHCI 控制器，锁覆盖一次完整命令的构造、提交和完成过程
    ctl: &'static Mutex<AHCIController>,
    /// 当前块设备对应的 AHCI 端口
    port: AHCIPort,
    /// 当前端口的逻辑扇区大小，单位字节
    sector_size: u32,
    // 此锁暂时未使用
    _lock: spin::Mutex<()>,
}

impl SataBlock {
    pub fn new() -> Self {
        let port = AHCI_CONTROLLER
            .lock()
            .find_first_sata_disk()
            .expect("No SATA disk found");
        let mut block = SataBlock {
            ctl: &AHCI_CONTROLLER,
            port,
            sector_size: 0,
            _lock: spin::Mutex::new(())
        };
        assert!(block.init(), "Failed to initialize SATA port {}", port.0);
        block
    }
    pub fn new_with_port(port: AHCIPort) -> Self {
        let mut block = SataBlock {
            ctl: &AHCI_CONTROLLER,
            port,
            sector_size: 0,
            _lock: spin::Mutex::new(())
        };
        assert!(block.init(), "Failed to initialize SATA port {}", port.0);
        block
    }
    pub fn init(&mut self) -> bool {
        let mut ctl = self.ctl.lock();
        // 初始化端口，返回是否成功
        if !ctl.init_port(self.port){
            return false;
        }
        // 更新逻辑扇区大小
        if let Some(sector_size) = ctl.get_port_sector_size(self.port) {
            self.sector_size = sector_size;
            true
        } else {
            false
        }
    }
    /// 读/写取指定块的数据，返回是否成功
    /// 块号以内核逻辑块为单位
    /// 需要内核保证首次调用前先完成初始化
    /// 
    /// 读
    pub fn read_block(&self, block_id: u64, buffer: &mut [u8]) -> bool {
        let mut ctl = self.ctl.lock();
        match ctl.read_sectors(self.port, block_id * (BLOCK_SZ as u64 / self.sector_size as u64), 8, buffer) {
            Ok(_) => true,
            Err(info) => {
                println!("Error reading block {}: {:?}", block_id, info);
                false
            },
        }
    }
    /// 写
    pub fn write_block(&self, block_id: u64, buffer: &[u8]) -> bool {
        let mut ctl = self.ctl.lock();
        match ctl.write_sectors(self.port, block_id * (BLOCK_SZ as u64 / self.sector_size as u64), 8, buffer) {
            Ok(_) => true,
            Err(info) => {
                println!("Error reading block {}: {:?}", block_id, info);
                false
            },
        }
    }
}

// 一些 AHCI 常量定义
//
// 最大端口数
const AHCI_MAX_PORTS: usize = 32;
// 端口引擎超时，毫秒
const PORT_ENGINE_TIMEOUT_MS: usize = 500;
// dma 区域布局偏移设置
const PORT_DMA_SIZE: usize = 0x2000;
const COMMAND_LIST_OFFSET: usize = 0x0000;
const RECEIVED_FIS_OFFSET: usize = 0x0400;
const COMMAND_TABLE_OFFSET: usize = 0x0500;
const DATA_BUFFER_OFFSET: usize = 0x1000;
const DATA_BUFFER_SIZE: usize = PORT_DMA_SIZE - DATA_BUFFER_OFFSET;
const COMMAND_SLOT: usize = 0;
const COMMAND_SLOT_MASK: u32 = 1 << COMMAND_SLOT;
const ATA_COMMAND_TIMEOUT_MS: usize = 5000;
// PxCMD 寄存器位
const PXCMD_ST: u32 = 1 << 0;
const PXCMD_FRE: u32 = 1 << 4;
const PXCMD_FR: u32 = 1 << 14;
const PXCMD_CR: u32 = 1 << 15;
// PRDT 最大传输字节数，单位为字节
const PRDT_MAX_BYTE_COUNT: usize = 1 << 22;
// PRDT 完成后请求端口中断位
const PRDT_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;
// Host to Device，从控制器到外设
const FIS_TYPE_REGISTER_H2D: u8 = 0x27;
// Device to Host，从外设到控制器
const FIS_REGISTER_H2D_COMMAND: u8 = 1 << 7;
// LBA 模式标志位
// 置1 表示让磁盘把 FIS 中的 lba0..lba5 按 LBA 地址解释
// 置0 表示旧 CHS 模式，按机械硬盘的 柱面/磁头/扇区 解释
const ATA_DEVICE_LBA: u8 = 1 << 6;
// LBA48
const ATA_LBA48_LIMIT: u64 = 1 << 48;
// LBA48 模式下每个命令最多传输的扇区数
const ATA_LBA48_MAX_SECTORS_PER_COMMAND: u32 = 1 << 16;
// ATA 命令码
const ATA_COMMAND_IDENTIFY_DEVICE: u8 = 0xEC;   // 获取设备标识
const ATA_COMMAND_READ_DMA_EXT: u8 = 0x25;      // 读取 DMA 扩展
const ATA_COMMAND_WRITE_DMA_EXT: u8 = 0x35;     // 写入 DMA 扩展
// FIS 长度，以 DWORD 为单位，固定为 5
const AHCI_COMMAND_FIS_DWORDS: u16 = 5;
// Header.flags 中的 W 位，表示数据方向，1 表示写到设备，0 表示从设备读取
const AHCI_COMMAND_HEADER_WRITE: u16 = 1 << 6;
// IS 寄存器命令错误掩码
const PXIS_COMMAND_ERROR_MASK: u32 = (1 << 30)
    | (1 << 29)
    | (1 << 28)
    | (1 << 27)
    | (1 << 26)
    | (1 << 24);
const ATA_STATUS_ERR: u32 = 1 << 0; // 错误标志，命令执行失败
const ATA_STATUS_DRQ: u32 = 1 << 3; // 请求数据传输阶段标志
const ATA_STATUS_DF: u32 = 1 << 5;  // 设备故障标志，命令执行失败
const ATA_STATUS_BSY: u32 = 1 << 7; // 设备忙标志，正在处理命令

/// AHCI Command Header
///
/// 位于 PxCLB 指向的 Command List 中，每个端口最多包含 32 个命令槽
/// 每个命令槽对应一个 32 B Command Header
#[repr(C)]
#[derive(Clone, Copy)]
struct AHCICommandHeader {
    /// DW0[15:0] 命令属性
    ///
    /// CFL[4:0]：Command FIS(Frame Information Structure) Length
    /// - 单位为 DWORD，Register H2D FIS 应填写 5
    /// A[5]：是否为 ATAPI 命令
    /// W[6]：数据方向，0 表示设备写入内存，1 表示内存写入设备
    /// P[7]：Prefetchable
    /// R[8]：Reset
    /// B[9]：BIST
    /// C[10]：Clear Busy upon R_OK
    /// PMP[15:12]：Port Multiplier 端口号，普通单盘使用 0
    flags: u16,
    /// DW0[31:16] PRDT(Physical Region Descriptor Table) Length
    /// 物理区域描述符表长度，即 Command Table 中有效 PRDT Entry 的数量
    ///
    /// 单个连续数据缓冲区通常填写 1，没有数据传输时填写 0
    prdt_length: u16,
    /// DW1 PRD Byte Count
    /// 物理区域描述符传输的字节数，单位为字节
    ///
    /// 提交命令前由软件清零，命令执行期间由 HBA 更新为已传输字节数
    prd_byte_count: u32,
    /// DW2 Command Table Base Address
    ///
    /// 命令表 DMA 物理地址低 32 位，地址必须按 128 B 对齐
    command_table_base: u32,
    /// DW3 Command Table Base Address Upper
    ///
    /// 命令表 DMA 物理地址高 32 位
    /// CAP.S64A 未置位（即不支持64位地址）时必须为 0
    command_table_base_upper: u32,
    /// DW4-DW7 保留字段，软件必须写 0
    reserved: [u32; 4],
}

const _: () = assert!(core::mem::size_of::<AHCICommandHeader>() == 32);

/// Register Host-to-Device FIS
///
/// 软件将该 FIS 放在 Command Table 的 CFIS 区域，HBA 据此生成 ATA 命令
/// 结构固定为 20 B，因此 Command Header 的 CFL 应填写 5 DWORD
#[repr(C)]
#[derive(Clone, Copy)]
struct AHCIRegisterH2DFIS {
    /// Byte 0 FIS Type，Register H2D 固定为 0x27
    fis_type: u8,
    /// Byte 1 Port Multiplier 与命令标志
    ///
    /// PM Port[3:0]：Port Multiplier 端口，普通单盘使用 0
    /// Reserved[6:4]：保留位
    /// C[7]：1 表示 Command，0 表示 Control
    pm_port_and_flags: u8,
    /// Byte 2 ATA Command，例如 IDENTIFY DEVICE 0xEC、READ DMA EXT 0x25
    command: u8,
    /// Byte 3 Features[7:0]
    feature_low: u8,
    /// Byte 4-6 LBA[23:0]
    lba0: u8,
    lba1: u8,
    lba2: u8,
    /// Byte 7 Device
    ///
    /// LBA 命令需要设置 bit 6，LBA48 模式下其余设备选择位通常为 0
    device: u8,
    /// Byte 8-10 LBA[47:24]
    lba3: u8,
    lba4: u8,
    lba5: u8,
    /// Byte 11 Features[15:8]
    feature_high: u8,
    /// Byte 12-13 Sector Count[15:0]
    count_low: u8,
    count_high: u8,
    /// Byte 14 Isochronous Command Completion，普通 ATA 命令填写 0
    icc: u8,
    /// Byte 15 ATA Control
    control: u8,
    /// Byte 16-19 保留字段，软件必须写 0
    reserved: [u8; 4],
}

impl AHCIRegisterH2DFIS {
    /// 构造不携带 LBA 参数的 ATA 命令 FIS，例如 IDENTIFY DEVICE
    fn new(command: u8) -> Self {
        Self {
            fis_type: FIS_TYPE_REGISTER_H2D,
            pm_port_and_flags: FIS_REGISTER_H2D_COMMAND,
            command,
            feature_low: 0,
            lba0: 0,
            lba1: 0,
            lba2: 0,
            device: 0,
            lba3: 0,
            lba4: 0,
            lba5: 0,
            feature_high: 0,
            count_low: 0,
            count_high: 0,
            icc: 0,
            control: 0,
            reserved: [0; 4],
        }
    }

    /// 构造 LBA48(Logical Block Addressing 48-bit) ATA 命令 FIS
    /// LBA48 即 48 位逻辑块寻址模式
    ///
    /// 构造读写磁盘的请求
    ///
    /// lba：起始ATA/SATA 协议逻辑块号
    /// - 这里的 Block 实际上就是 Sector
    /// sector_count：每个块的扇区数，最大为 65536
    /// - 读取一个内核逻辑磁盘块，即读取sector_count个扇区
    fn new_lba48(command: u8, lba: u64, sector_count: u32) -> Option<Self> {
        if sector_count == 0 || sector_count > ATA_LBA48_MAX_SECTORS_PER_COMMAND {
            return None;
        }
        let end_lba = lba.checked_add(sector_count as u64)?;
        if end_lba > ATA_LBA48_LIMIT {
            return None;
        }

        let encoded_count = if sector_count == ATA_LBA48_MAX_SECTORS_PER_COMMAND {
            0 // 0 表示 65536
        } else {
            sector_count as u16
        };
        let mut fis = Self::new(command);
        fis.lba0 = lba as u8;
        fis.lba1 = (lba >> 8) as u8;
        fis.lba2 = (lba >> 16) as u8;
        fis.device = ATA_DEVICE_LBA;
        fis.lba3 = (lba >> 24) as u8;
        fis.lba4 = (lba >> 32) as u8;
        fis.lba5 = (lba >> 40) as u8;
        fis.count_low = encoded_count as u8;
        fis.count_high = (encoded_count >> 8) as u8;
        Some(fis)
    }
}

const _: () = assert!(core::mem::size_of::<AHCIRegisterH2DFIS>() == 20);

/// AHCI PRDT (Physical Region Descriptor Table) Entry
/// 为 AHCI PRDT 的单个条目，也可叫 PRD
///
/// 位于 Command Table 的 0x80 + 16 B * PRD索引 处
/// 一个 PRDT Entry 描述一段物理连续的 DMA 数据区域
#[repr(C)]
#[derive(Clone, Copy)]
struct AHCIPRDTEntry {
    /// DW0 Data Base Address
    ///
    /// DMA 数据区域物理地址低 32 位，地址至少按 2 B 对齐
    data_base: u32,
    /// DW1 Data Base Address Upper
    ///
    /// DMA 数据区域物理地址高 32 位，CAP.S64A 为 0 时必须为 0
    data_base_upper: u32,
    /// DW2 保留字段，软件必须写 0
    reserved: u32,
    /// DW3 Data Byte Count & Interrupt on Completion
    /// 传输字节数和中断请求标志
    ///
    /// DBC[21:0]：传输字节数减 1，单个 PRDT Entry 最多描述 4 MiB
    /// Reserved[30:22]：软件必须写 0
    /// I[31]：该 PRD 完成后是否请求端口中断
    byte_count_and_flags: u32,
}

impl AHCIPRDTEntry {
    /// 根据 DMA 物理地址和实际传输字节数构造 PRDT Entry
    ///
    /// ATA 数据传输以 word 为单位，因此要求地址和字节数均按 2 B 对齐
    fn new(
        data_base: PhysAddr,
        byte_count: usize,
        interrupt_on_completion: bool,
    ) -> Option<Self> {
        if data_base.0 & 1 != 0
            || byte_count == 0
            || byte_count > PRDT_MAX_BYTE_COUNT
            || byte_count & 1 != 0
        {
            return None;
        }

        let interrupt = if interrupt_on_completion {
            PRDT_INTERRUPT_ON_COMPLETION
        } else {
            0
        };
        Some(Self {
            data_base: data_base.0 as u32,
            data_base_upper: (data_base.0 >> 32) as u32,
            reserved: 0,
            byte_count_and_flags: (byte_count as u32 - 1) | interrupt,
        })
    }
}

// 确认布局符合预期
const _: () = assert!(core::mem::size_of::<AHCIPRDTEntry>() == 16);

/// slot 0 使用的 AHCI Command Table
///
/// CFIS 从偏移 0x00 开始，PRDT 从偏移 0x80 开始
/// 当前实现只使用一个 PRDT Entry，因此结构体大小为 0x90
#[repr(C)]
#[derive(Clone, Copy)]
struct AHCICommandTable {
    /// Command FIS 的前 20 B
    command_fis: AHCIRegisterH2DFIS,
    /// CFIS 区域剩余空间，使整个 CFIS 区域达到 64 B
    command_fis_reserved: [u8; 44],
    /// ATAPI Command 区域，普通 ATA 磁盘保持为 0
    atapi_command: [u8; 16],
    /// 偏移 0x50-0x7f 的保留区域
    reserved: [u8; 48],
    /// 从偏移 0x80 开始的 PRDT，当前只使用一个物理连续数据区域
    prdt: [AHCIPRDTEntry; 1],
}

// 确认布局符合预期
const _: () = assert!(core::mem::size_of::<AHCICommandTable>() == 0x90);
const _: () = assert!(core::mem::offset_of!(AHCICommandTable, prdt) == 0x80);
const _: () = assert!(COMMAND_TABLE_OFFSET % 128 == 0);
const _: () = assert!(
    COMMAND_TABLE_OFFSET + core::mem::size_of::<AHCICommandTable>() <= DATA_BUFFER_OFFSET
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ATADataDirection {
    // Device to Host
    D2H,
    // Host to Device
    H2D,
}

/// 单个端口所使用的 DMA 区域布局
///
/// 所有字段均为提供给 HBA 的物理地址，CPU 访问时需要转换到非缓存 DMW
#[derive(Clone, Copy)]
struct AHCIPortDmaLayout {
    /// Command List 物理地址，写入 PxCLB/PxCLBU，必须按 1 KB 对齐
    ///
    /// 命令列表，有 32 个 slot（每个32B），一个 slot 中装一个命令头，共 1KB
    pub command_list: PhysAddr,
    /// Received FIS Buffer 物理地址，写入 PxFB/PxFBU，必须按 256 B 对齐
    ///
    /// HBA 会将硬盘返回的FIS 写入此缓冲区
    pub received_fis: PhysAddr,
    /// slot 0 Command Table 物理地址，写入 Command Header 的 CTBA/CTBAU
    ///
    /// - CFIS(0x00,64B)
    /// - ATAPI(0x40,16B,ATA填0)
    /// - PRDT(0x80,每项PRD 16B)
    /// 目前只初始化 slot 0
    pub command_table: PhysAddr,
    /// 命令数据缓冲区物理地址，后续由 PRDT Entry 引用
    ///
    /// 放读写数据的缓冲区，目前暂时固定 4KB，后续改为动态分配并支持大块预读
    pub data_buffer: PhysAddr,
}

impl AHCIPortDmaLayout {
    fn from_base(base: PhysAddr) -> Self {
        Self {
            command_list: PhysAddr(base.0 + COMMAND_LIST_OFFSET),
            received_fis: PhysAddr(base.0 + RECEIVED_FIS_OFFSET),
            command_table: PhysAddr(base.0 + COMMAND_TABLE_OFFSET),
            data_buffer: PhysAddr(base.0 + DATA_BUFFER_OFFSET),
        }
    }
}

/// AHCI HBA 全局寄存器，相对控制器 MMIO 基址的 32 位偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AHCIReg {
    /// HBA Capabilities
    ///
    /// NP[4:0]：最大端口号，控制器端口数为 NP + 1
    /// SXS[5]：是否支持外置 SATA 端口标识
    /// EMS[6]：是否支持 Enclosure Management
    /// CCCS[7]：是否支持 Command Completion Coalescing
    /// NCS[12:8]：最大命令槽号，命令槽数为 NCS + 1
    /// PSC[13] / SSC[14]：是否支持 Partial / Slumber 电源状态
    /// PMD[15]：是否支持机械式设备存在检测
    /// FBSS[16]：是否支持 FIS-based switching
    /// - FIS 即 Frame Information Structure，sata 报文帧
    /// SPM[17]：是否支持 SATA Port Multiplier
    /// SAM[18]：是否只支持 AHCI 模式
    /// ISS[23:20]：支持的 SATA 接口速率代际
    /// SCLO[24]：是否支持 Command List Override
    /// SAL[25] / SALP[26]：是否支持 activity LED / aggressive link power management
    /// SSS[27]：是否支持逐槽位状态
    /// SMPS[28]：是否支持机械式 Presence Switch
    /// SSNTF[29]：是否支持 SATA Notification
    /// SNCQ[30]：是否支持 Native Command Queuing
    /// S64A[31]：是否支持 64 位 DMA 地址
    HbaCap = 0x00,
    /// HBA Global Host Control
    ///
    /// HR[0]：写 1 请求 HBA reset，硬件完成后自动清零
    /// IE[1]：全局中断使能；端口中断还需在 PxIE 中打开
    /// MRSM[2]：MSI Revert to Single Message，仅用于 MSI
    /// AE[31]：AHCI Enable，置 1 后按 AHCI 寄存器模型工作
    HbaGhc = 0x04,
    /// HBA Interrupt Status（只读）
    ///
    /// IS[n]：端口 n 有待处理的中断；该寄存器只用于定位端口，具体事件从
    /// PxIS 获取，且需通过对 PxIS 写 1 清除
    HbaIs = 0x08,
    /// HBA Ports Implemented（只读）
    ///
    /// PI[n]：端口 n 的寄存器组已实现；仅当 n <= CAP.NP 且 PI[n] 为 1 时
    /// 才能访问该端口，仍需通过 PxSSTS 确认已接入可通信设备
    HbaPi = 0x0C,
}

/// AHCI 端口号
///
/// 端口号必须小于 CAP.NP + 1，且其位必须在 PI 中置位；
/// 实际读写前还应通过 PxSSTS 检查是否已连接并激活设备
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AHCIPort(pub isize);

impl AHCIPort {
    /// 取得端口寄存器组相对于 HBA MMIO 基址的偏移
    ///
    /// AHCI 规定端口寄存器区从 0x100 开始，每个端口占用 0x80 字节
    pub fn offset(&self) -> usize {
        if self.0 < 0 {
            panic!("AHCI port is NOT initialized");
        }
        (0x100 + (self.0 as usize) * 0x80)
    }
}

/// AHCI 端口寄存器，相对 Px 端口基址的 32 位偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum AHCIPortReg {
    /// Command List Base
    ///
    /// CLB[31:10]：Command List DMA/物理地址低 32 位，低 10 位必须为 0，
    /// 即 1 KiB 对齐，列表最多含 32 个、每项 32 B 的 Command Header
    /// 仅在 PxCMD.ST 和 PxCMD.FRE 已停止后更新
    Clb = 0x00,
    /// Command List Base Upper
    ///
    /// CLBU[31:0]：Command List DMA 地址高 32 位，CAP.S64A 未置位时必须为 0
    Clbu = 0x04,
    /// FIS Receive Buffer Base
    ///
    /// FB[31:8]：Received FIS Buffer DMA/物理地址低 32 位，低 8 位必须为 0，
    /// 即 256 B 对齐，HBA 会将硬盘返回的 D2H、SDB 等 FIS 写入此缓冲区
    Fb = 0x08,
    /// FIS Receive Buffer Base Upper
    ///
    /// FBU[31:0]：Received FIS Buffer DMA 地址高 32 位，CAP.S64A 未置位时必须为 0
    Fbu = 0x0C,
    /// Port Interrupt Status（W1C，写1清除）
    ///
    /// DHRS[0]：收到 Device-to-Host Register FIS，非 NCQ 完成时常见
    /// PSS[1] / DSS[2] / SDBS[3]：收到 PIO Setup、DMA Setup、Set Device Bits FIS
    /// PCS[6] / PRCS[22]：端口连接或 PHY Ready 状态变化
    /// IPMS[23]：接口电源管理状态变化；OFS[24]：FIS 溢出
    /// IFS[27] / HBDS[28] / HBFS[29]：接口或 HBA DMA 错误
    /// TFES[30]：Task File Error，应结合 PxTFD、PxSERR 诊断
    ///
    /// 对已处理的置位位写 1 清除；写 0 不清除状态
    Is = 0x10,
    /// Port Interrupt Enable
    ///
    /// 位定义与 PxIS 对应，需要的完成、链路变化和错误位可置 1；还需同时置
    /// GHC.IE 并完成平台 IRQ 路由，CPU 才会收到中断
    Ie = 0x14,
    /// Command and Status
    ///
    /// ST[0]：启动 Command List 引擎
    /// FRE[4]：启动 FIS Receive 引擎
    /// CCS[12:8]：当前执行命令槽号，只读
    /// FR[14] FIS 接收，只读
    /// CR[15]：Command List 引擎运行状态，只读
    ///
    /// 停止时清 ST、FRE，等待 FR、CR 清零后才能更新 CLB/FB；启动时先置 FRE，
    /// 再置 ST
    Cmd = 0x18,
    /// Task File Data（只读）
    ///
    /// STS[7:0]：ATA Status，包含 BSY、DRQ、ERR 等状态
    /// ERR[15:8]：ATA Error，PxIS.TFES 置位时需与 PxSERR 一并读取
    Tfd = 0x20,
    /// Signature（只读）
    ///
    /// 0x0000_0101：普通 SATA ATA 磁盘
    /// 0xeb14_0101：SATAPI 设备，例如光驱
    /// 0xc33c_0101：SEMB 设备
    /// 0x9669_0101：SATA Port Multiplier
    Sig = 0x24,
    /// SATA Status / SCR0（只读）
    ///
    /// DET[3:0]：设备检测状态，0 为未检测，1 为设备存在但未建立通信，3 为
    /// 已建立 PHY 通信
    /// SPD[7:4]：协商后的 SATA 链路速率；IPM[11:8]：接口电源状态，1 为 Active
    /// 普通 SATA 磁盘以 DET == 3 && IPM == 1 判断端口可通信
    Ssts = 0x28,
    /// SATA Control / SCR2
    ///
    /// DET[3:0]：链路控制，1 请求 COMRESET，随后恢复为 0 以重新协商；4 可
    /// 禁用端口，SPD[7:4] 可限制速率，0 表示不限制；IPM[11:8] 控制电源策略
    Sctl = 0x2C,
    /// SATA Error / SCR1（W1C）
    ///
    /// 记录恢复、传输、协议和 PHY 诊断错误，超时、TFES 或接口错误时先读取
    /// 并记录，再向置位位写 1 清除
    Serr = 0x30,
    /// SATA Active / SCR3
    ///
    /// SACT[n]：NCQ 命令槽 n 正在执行
    /// 提交 FPDMA QUEUED 命令时先设置 SACT[n]，再设置 CI[n]
    /// 普通非 NCQ(Native Command Queuing)，即单槽，情况下不使用
    Sact = 0x34,
    /// Command Issue
    /// 用于通知 HBA 执行对应命令槽的命令，目前只使用 slot 0
    ///
    /// CI[n]：向空闲命令槽 n 写 1，通知 HBA 读取该槽 Command Header、CFIS 和
    /// PRDT，HBA 完成命令后清除该位
    ///
    /// 写入前必须填完描述符，并确保描述符和写数据对 DMA 可见；不得复用 CI
    /// 仍置位的命令槽
    Ci = 0x38,
    /// SATA Notification / SCR4
    ///
    /// 用于 Port Multiplier 下游设备的状态变化通知；普通单盘 SATA 通常不使用
    Sntf = 0x3C,
    /// FIS-based Switching Control
    ///
    /// 管理 Port Multiplier 的 FIS-based switching，仅当 CAP.FBSS 和设备能力
    /// 均支持时使用；普通单盘 SATA 初始化无需配置
    Fbs = 0x40,
}

impl AHCIPortReg {
    pub const ALL: &'static [Self] = &[
        Self::Clb,
        Self::Clbu,
        Self::Fb,
        Self::Fbu,
        Self::Is,
        Self::Ie,
        Self::Cmd,
        Self::Tfd,
        Self::Sig,
        Self::Ssts,
        Self::Sctl,
        Self::Serr,
        Self::Sact,
        Self::Ci,
        Self::Sntf,
        Self::Fbs,
    ];

    pub const fn offset(self) -> usize {
        self as usize
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AHCIDevType {
    /// 普通 SATA ATA 磁盘
    Ata = 0x0000_0101,
    /// SATAPI 设备，例如光驱
    Satapi = 0xeb14_0101,
    /// SEMB 设备
    Semb = 0xc33c_0101,
    /// SATA Port Multiplier
    Pm = 0x9669_0101,
}

impl TryFrom<u32> for AHCIDevType {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0x0000_0101 => Ok(AHCIDevType::Ata),
            0xeb14_0101 => Ok(AHCIDevType::Satapi),
            0xc33c_0101 => Ok(AHCIDevType::Semb),
            0x9669_0101 => Ok(AHCIDevType::Pm),
            _ => Err(()),
        }
    }
}

/// IDENTIFY DEVICE 返回的磁盘信息
#[derive(Debug, Clone)]
pub struct ATAIdentifyInfo {
    pub serial_number: String,
    pub firmware_revision: String,
    pub model_number: String,
    /// 磁盘包含的逻辑扇区总数
    pub logical_sector_count: u64,
    /// 每个逻辑扇区的字节数
    pub logical_sector_size: u32,
    pub supports_dma: bool,
    pub supports_lba48: bool,
}

#[derive(Debug)]
pub struct AHCIError {
    /// 发生错误时的端口寄存器快照
    ///
    /// 数组下标为寄存器相对端口基址的字节偏移除以 4
    /// 未在 AHCIPortReg::ALL 中列出的保留位置保持为 0
    pub regs: [u32; 0x44 / 4],
}

/// ATA 命令构造、提交或执行阶段的错误
#[derive(Debug)]
pub enum AHCICommandError {
    InvalidArgument(&'static str),
    PortNotInitialized,
    Unsupported(&'static str),
    Timeout {
        phase: &'static str,
        state: AHCIError,
    },
    DeviceError(AHCIError),
    ShortTransfer {
        expected: usize,
        actual: usize,
        state: AHCIError,
    },
}


/// AHCI 控制器
/// 即 HBA（Host Bus Adapter）
pub struct AHCIController {
    /// 从 PCI BAR0 读取的 AHCI MMIO 物理基址，不包含 DMW 虚拟窗口位
    base_addr: usize,
    /// 控制器持有的 DMA 分配对象，保证端口仍在使用时对应物理页不会被回收
    dma_buffers: Vec<DmaBuffer>,
    /// 每个端口对应的 DMA 区域物理基址，用于复用已分配的端口内存
    port_dma_bases: [Option<PhysAddr>; AHCI_MAX_PORTS],
    /// 每个端口通过 IDENTIFY DEVICE 读取的设备信息
    identify_info: [Option<ATAIdentifyInfo>; AHCI_MAX_PORTS],
}

impl AHCIController {
    pub fn new(base_addr: usize) -> Self {
        AHCIController {
            base_addr,
            dma_buffers: Vec::new(),
            port_dma_bases: [None; AHCI_MAX_PORTS],
            identify_info: core::array::from_fn(|_| None),
        }
    }
    pub const fn windowed_base_addr(&self) -> usize {
        self.base_addr | UNCHACHED_KERNEL_BASE
    }
    pub fn reg_read(&self, reg: AHCIReg) -> u32 {
        let reg_addr = self.windowed_base_addr() + reg as usize;
        unsafe { core::ptr::read_volatile(reg_addr as *const u32) }
    }
    pub fn reg_write(&self, reg: AHCIReg, value: u32) {
        let reg_addr = self.windowed_base_addr() + reg as usize;
        unsafe { core::ptr::write_volatile(reg_addr as *mut u32, value) }
    }
    pub fn port_reg_read(&self, port: AHCIPort, reg: AHCIPortReg) -> u32 {
        let port_base = self.windowed_base_addr() + port.offset();
        let reg_addr = port_base + reg as usize;
        unsafe { core::ptr::read_volatile(reg_addr as *const u32) }
    }
    pub fn port_reg_write(&self, port: AHCIPort, reg: AHCIPortReg, value: u32) {
        let port_base = self.windowed_base_addr() + port.offset();
        let reg_addr = port_base + reg as usize;
        unsafe { core::ptr::write_volatile(reg_addr as *mut u32, value) }
    }
    pub fn port_regs_read(&self, port: AHCIPort) -> [u32; 0x44 / 4] {
        let mut regs = [0u32; 0x44 / 4];
        for reg in AHCIPortReg::ALL.iter() {
            let value = self.port_reg_read(port, *reg);
            regs[*reg as usize / 4] = value;
        }
        regs
    }
    pub fn print_all_regs(&self) {
        println!("AHCI Controller Registers:");
        for reg in [
            AHCIReg::HbaCap,
            AHCIReg::HbaGhc,
            AHCIReg::HbaIs,
            AHCIReg::HbaPi,
        ] {
            let value = self.reg_read(reg);
            println!("{:?} (0x{:02X}): 0x{:08X}", reg, reg as usize, value);
        }
        let port_num = (self.reg_read(AHCIReg::HbaCap) & 0x1F) + 1;
        for port_num in 0..port_num {
            info!("--- Printing registers for port {} ---", port_num);
            self.print_port_regs(AHCIPort(port_num as isize));
            info!("--- Finished printing registers for port {} ---", port_num);
        }
    }
    pub fn print_port_regs(&self, port: AHCIPort) {
        println!("AHCI Port {:?} Registers:", port);
        for reg in AHCIPortReg::ALL.iter() {
            let value = self.port_reg_read(port, *reg);
            println!("{:?} (0x{:02X}): 0x{:08X}", reg, *reg as usize, value);
        }
    }
    // 等待 PxCMD 的 mask 位清零
    fn wait_port_cmd_clear(&self, port: AHCIPort, mask: u32) -> Result<(), AHCIError> {
        let start_ms = get_time_ms();
        loop {
            let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
            if cmd & mask == 0 {
                return Ok(());
            }
            if get_time_ms().saturating_sub(start_ms) >= PORT_ENGINE_TIMEOUT_MS {
                return Err(AHCIError {
                    regs: self.port_regs_read(port),
                });
            }
            spin_loop();
        }
    }
    // 停止端口命令引擎（Command List Engine）和端口帧信息接收引擎（FIS Receive Engine）
    fn stop_port_engine(&self, port: AHCIPort) -> Result<(), AHCIError> {
        let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
        let new_cmd = cmd & !PXCMD_ST;
        self.port_reg_write(port, AHCIPortReg::Cmd, new_cmd);
        if let Err(err) = self.wait_port_cmd_clear(port, PXCMD_CR) {
            return Err(err);
        }
        let new_cmd = self.port_reg_read(port, AHCIPortReg::Cmd) & !PXCMD_FRE;
        self.port_reg_write(port, AHCIPortReg::Cmd, new_cmd);
        self.wait_port_cmd_clear(port, PXCMD_FR)
    }
    // 启动端口命令引擎（Command List Engine）
    //
    // 启动时必须先启动 FIS 接收引擎，再启动命令列表引擎
    fn start_port_engine(&self, port: AHCIPort) -> Result<(), AHCIError> {
        // 确保启动引擎前，命令列表和端口寄存器的写入对 HBA 可见
        fence(Ordering::SeqCst);

        let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
        self.port_reg_write(port, AHCIPortReg::Cmd, cmd | PXCMD_FRE);
        let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
        self.port_reg_write(port, AHCIPortReg::Cmd, cmd | PXCMD_ST);

        let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
        if cmd & (PXCMD_ST | PXCMD_FRE) == (PXCMD_ST | PXCMD_FRE) {
            Ok(())
        } else {
            Err(AHCIError {
                regs: self.port_regs_read(port),
            })
        }
    }
    // 检查端口号、PI、链路状态和设备类型
    fn validate_sata_port(&self, port: AHCIPort) -> Option<usize> {
        let Ok(port_index) = usize::try_from(port.0) else {
            error!("Invalid negative AHCI port {}", port.0);
            return None;
        };
        if port_index >= AHCI_MAX_PORTS {
            error!("AHCI port {} is out of range", port.0);
            return None;
        }
        if self.reg_read(AHCIReg::HbaPi) & (1u32 << port_index) == 0 {
            error!("Port {} is not implemented in this controller", port.0);
            return None;
        }

        let ssts = self.port_reg_read(port, AHCIPortReg::Ssts);
        let det = ssts & 0xF;
        let ipm = (ssts >> 8) & 0xF;
        if det != 3 || ipm != 1 {
            error!("Port {} is not ready for communication", port.0);
            return None;
        }

        let sig = self.port_reg_read(port, AHCIPortReg::Sig);
        if !matches!(AHCIDevType::try_from(sig), Ok(AHCIDevType::Ata)) {
            error!(
                "Port {} is not a supported SATA ATA disk, PxSIG=0x{:08x}",
                port.0, sig
            );
            return None;
        }
        Some(port_index)
    }
    // 关闭端口中断并清除 U-Boot 遗留的中断和 SATA 错误状态
    fn clear_port_status(&self, port: AHCIPort) {
        let old_is = self.port_reg_read(port, AHCIPortReg::Is);
        let old_serr = self.port_reg_read(port, AHCIPortReg::Serr);
        if old_is != 0 || old_serr != 0 {
            debug!(
                "Clearing AHCI port {} status: PxIS=0x{:08x}, PxSERR=0x{:08x}",
                port.0, old_is, old_serr
            );
        }
        self.port_reg_write(port, AHCIPortReg::Ie, 0);
        self.port_reg_write(port, AHCIPortReg::Is, u32::MAX);
        self.port_reg_write(port, AHCIPortReg::Serr, u32::MAX);
    }
    // 分配 DMA 缓冲区，返回首地址（物理地址）
    pub fn alloc_dma_buffer(&mut self, pages: usize) -> Option<PhysAddr> {
        let dma_buf = QUEUE_FRAMES.exclusive_access().alloc(pages)?;
        let phys_addr = dma_buf.phys_addr();
        unsafe {
            core::ptr::write_bytes(dma_buf.uncached_ptr(), 0, pages * PAGE_SIZE);
        }
        self.dma_buffers.push(dma_buf);
        Some(phys_addr)
    }
    // 分配或复用端口 DMA 区域，并构造各部分物理地址
    fn prepare_port_dma(
        &mut self,
        port: AHCIPort,
        port_index: usize,
    ) -> Option<AHCIPortDmaLayout> {
        let dma_pa = match self.port_dma_bases[port_index] {
            Some(pa) => pa,
            None => {
                let Some(pa) = self.alloc_dma_buffer(PORT_DMA_SIZE / PAGE_SIZE) else {
                    error!("Failed to allocate DMA memory for AHCI port {}", port.0);
                    return None;
                };
                self.port_dma_bases[port_index] = Some(pa);
                pa
            }
        };

        // DMA 区域按页分配，同时满足 CLB 的 1 KiB 对齐要求
        debug_assert_eq!(dma_pa.0 & (PAGE_SIZE - 1), 0);
        let supports_64bit = self.reg_read(AHCIReg::HbaCap) & (1 << 31) != 0;
        if !supports_64bit && dma_pa.0 >> 32 != 0 {
            error!("AHCI controller does not support the allocated 64-bit DMA address");
            return None;
        }

        // 端口可能被重新初始化，旧的命令、FIS 和数据不能继续交给 HBA
        unsafe {
            core::ptr::write_bytes(
                (dma_pa.0 | UNCHACHED_KERNEL_BASE) as *mut u8,
                0,
                PORT_DMA_SIZE,
            );
        }
        Some(AHCIPortDmaLayout::from_base(dma_pa))
    }
    // 设置端口的 DMA 区域
    fn program_port_dma(&self, port: AHCIPort, layout: AHCIPortDmaLayout) {
        // 构造 slot0 的命令头
        let command_header = AHCICommandHeader {
            flags: 0,
            prdt_length: 0,
            prd_byte_count: 0,
            command_table_base: layout.command_table.0 as u32,
            command_table_base_upper: (layout.command_table.0 >> 32) as u32,
            reserved: [0; 4],
        };
        // 将命令头写入 DMA 区域
        let command_header_va =
            (layout.command_list.0 | UNCHACHED_KERNEL_BASE) as *mut AHCICommandHeader;
        unsafe {
            core::ptr::write_volatile(command_header_va, command_header);
        }

        self.port_reg_write(port, AHCIPortReg::Clb, layout.command_list.0 as u32);
        self.port_reg_write(
            port,
            AHCIPortReg::Clbu,
            (layout.command_list.0 >> 32) as u32,
        );
        self.port_reg_write(port, AHCIPortReg::Fb, layout.received_fis.0 as u32);
        self.port_reg_write(
            port,
            AHCIPortReg::Fbu,
            (layout.received_fis.0 >> 32) as u32,
        );
    }
    /// 错误时调用，读取端口寄存器状态，返回 AHCIError
    fn command_error_state(&self, port: AHCIPort) -> AHCIError {
        AHCIError {
            regs: self.port_regs_read(port),
        }
    }
    /// 获取端口的 DMA 区域布局
    fn port_dma_layout(
        &self,
        port: AHCIPort,
    ) -> Result<AHCIPortDmaLayout, AHCICommandError> {
        let port_index = usize::try_from(port.0)
            .ok()
            .filter(|&index| index < AHCI_MAX_PORTS)
            .ok_or(AHCICommandError::InvalidArgument("AHCI port is out of range"))?;
        self.port_dma_bases[port_index]
            .map(AHCIPortDmaLayout::from_base)
            .ok_or(AHCICommandError::PortNotInitialized)
    }
    /// 等待 ATA 命令完成，检查错误状态
    ///
    /// 确保 slot 0 空闲且 ATA Task File 不处于 BSY/DRQ 状态
    fn wait_command_ready(&self, port: AHCIPort) -> Result<(), AHCICommandError> {
        let start_ms = get_time_ms();
        loop {
            let ci = self.port_reg_read(port, AHCIPortReg::Ci);
            let sact = self.port_reg_read(port, AHCIPortReg::Sact);
            let tfd = self.port_reg_read(port, AHCIPortReg::Tfd);
            if ci == 0
                && sact == 0
                && tfd & (ATA_STATUS_BSY | ATA_STATUS_DRQ) == 0
            {
                return Ok(());
            }
            if get_time_ms().saturating_sub(start_ms) >= ATA_COMMAND_TIMEOUT_MS {
                return Err(AHCICommandError::Timeout {
                    phase: "waiting for slot 0 and ATA device ready",
                    state: self.command_error_state(port),
                });
            }
            spin_loop();
        }
    }
    /// 发送 ATA 命令，返回 DMA 布局或错误
    ///
    /// 等待命令完成，检查错误状态后才返回
    fn issue_ata_command(
        &self,
        port: AHCIPort,
        fis: AHCIRegisterH2DFIS,
        direction: ATADataDirection,
        transfer_bytes: usize,
    ) -> Result<AHCIPortDmaLayout, AHCICommandError> {
        if transfer_bytes == 0 || transfer_bytes > DATA_BUFFER_SIZE {
            return Err(AHCICommandError::InvalidArgument(
                "ATA transfer does not fit in the port data buffer",
            ));
        }
        let layout = self.port_dma_layout(port)?;
        let cmd = self.port_reg_read(port, AHCIPortReg::Cmd);
        if cmd & (PXCMD_ST | PXCMD_FRE) != (PXCMD_ST | PXCMD_FRE) {
            return Err(AHCICommandError::PortNotInitialized);
        }
        self.wait_command_ready(port)?;
        // 构造 PRDT，Command Table 和 Command Header，写入相应 DMA 区域
        let prd = AHCIPRDTEntry::new(layout.data_buffer, transfer_bytes, false).ok_or(
            AHCICommandError::InvalidArgument("invalid PRDT address or transfer length"),
        )?;
        let command_table = AHCICommandTable {
            command_fis: fis,
            command_fis_reserved: [0; 44],
            atapi_command: [0; 16],
            reserved: [0; 48],
            prdt: [prd],
        };
        let header_flags = AHCI_COMMAND_FIS_DWORDS
            | if direction == ATADataDirection::H2D {
                AHCI_COMMAND_HEADER_WRITE
            } else {
                0
            };
        let command_header = AHCICommandHeader {
            flags: header_flags,
            prdt_length: 1,
            prd_byte_count: 0,
            command_table_base: layout.command_table.0 as u32,
            command_table_base_upper: (layout.command_table.0 >> 32) as u32,
            reserved: [0; 4],
        };
        // 读操作先清空数据区域，误用上一次命令残留的数据
        if direction == ATADataDirection::D2H {
            unsafe {
                core::ptr::write_bytes(
                    (layout.data_buffer.0 | UNCHACHED_KERNEL_BASE) as *mut u8,
                    0,
                    transfer_bytes,
                );
            }
        }
        // 将命令表和命令头写入 DMA 区域
        unsafe {
            core::ptr::write_volatile(
                (layout.command_table.0 | UNCHACHED_KERNEL_BASE) as *mut AHCICommandTable,
                command_table,
            );
            core::ptr::write_volatile(
                (layout.command_list.0 | UNCHACHED_KERNEL_BASE) as *mut AHCICommandHeader,
                command_header,
            );
        }
        // 向设备发送请求并轮询等待至完成
        // 清除上一条命令的状态，再确保描述符和写数据先于 PxCI 对 HBA 可见
        self.port_reg_write(port, AHCIPortReg::Is, u32::MAX);
        self.port_reg_write(port, AHCIPortReg::Serr, u32::MAX);
        fence(Ordering::SeqCst);
        // 通知设备执行 slot 0 的命令
        self.port_reg_write(port, AHCIPortReg::Ci, COMMAND_SLOT_MASK);
        // 等待
        let start_ms = get_time_ms();
        loop {
            let interrupt_status = self.port_reg_read(port, AHCIPortReg::Is);
            if interrupt_status & PXIS_COMMAND_ERROR_MASK != 0 {
                let state = self.command_error_state(port);
                let serr = self.port_reg_read(port, AHCIPortReg::Serr);
                self.port_reg_write(port, AHCIPortReg::Is, interrupt_status);
                self.port_reg_write(port, AHCIPortReg::Serr, serr);
                return Err(AHCICommandError::DeviceError(state));
            }
            // slot 0 命令已完成，DMA 传输完成
            // 现在 DMA 区域的数据已经写入磁盘或从磁盘读取完成
            if self.port_reg_read(port, AHCIPortReg::Ci) & COMMAND_SLOT_MASK == 0 {
                fence(Ordering::SeqCst);
                let tfd = self.port_reg_read(port, AHCIPortReg::Tfd);
                let serr = self.port_reg_read(port, AHCIPortReg::Serr);
                if tfd & (ATA_STATUS_ERR | ATA_STATUS_DF) != 0 || serr != 0 {
                    let state = self.command_error_state(port);
                    self.port_reg_write(port, AHCIPortReg::Is, interrupt_status);
                    self.port_reg_write(port, AHCIPortReg::Serr, serr);
                    return Err(AHCICommandError::DeviceError(state));
                }
                let command_header = unsafe {
                    core::ptr::read_volatile(
                        (layout.command_list.0 | UNCHACHED_KERNEL_BASE)
                            as *const AHCICommandHeader,
                    )
                };
                if command_header.prd_byte_count as usize != transfer_bytes {
                    let state = self.command_error_state(port);
                    self.port_reg_write(port, AHCIPortReg::Is, interrupt_status);
                    return Err(AHCICommandError::ShortTransfer {
                        expected: transfer_bytes,
                        actual: command_header.prd_byte_count as usize,
                        state,
                    });
                }
                self.port_reg_write(port, AHCIPortReg::Is, interrupt_status);
                return Ok(layout);
            }
            // 超时处理
            if get_time_ms().saturating_sub(start_ms) >= ATA_COMMAND_TIMEOUT_MS {
                return Err(AHCICommandError::Timeout {
                    phase: "waiting for PxCI slot 0 completion",
                    state: self.command_error_state(port),
                });
            }
            spin_loop();
        }
    }
    /// 获取标识信息中指定位置的单个词（16 位）
    fn identify_word(data: &[u8; 512], word_index: usize) -> u16 {
        u16::from_le_bytes([data[word_index * 2], data[word_index * 2 + 1]])
    }
    /// 按词获取标识信息中的字符串
    fn identify_string(data: &[u8; 512], first_word: usize, word_count: usize) -> String {
        let mut bytes = Vec::with_capacity(word_count * 2);
        for word_index in first_word..first_word + word_count {
            let word = Self::identify_word(data, word_index);
            bytes.push((word >> 8) as u8);
            bytes.push(word as u8);
        }
        while matches!(bytes.last(), Some(b' ' | 0)) {
            bytes.pop();
        }
        for byte in bytes.iter_mut() {
            if !byte.is_ascii_graphic() && *byte != b' ' {
                *byte = b'?';
            }
        }
        String::from_utf8(bytes).unwrap_or_default()
    }
    /// 分词标识信息，返回结构体
    fn parse_identify_data(data: &[u8; 512]) -> Result<ATAIdentifyInfo, AHCICommandError> {
        let supports_dma = Self::identify_word(data, 49) & (1 << 8) != 0;
        let command_set_support = Self::identify_word(data, 83);
        let supports_lba48 = command_set_support & 0xC000 == 0x4000
            && command_set_support & (1 << 10) != 0;
        let logical_sector_count = if supports_lba48 {
            (Self::identify_word(data, 100) as u64)
                | ((Self::identify_word(data, 101) as u64) << 16)
                | ((Self::identify_word(data, 102) as u64) << 32)
                | ((Self::identify_word(data, 103) as u64) << 48)
        } else {
            (Self::identify_word(data, 60) as u64)
                | ((Self::identify_word(data, 61) as u64) << 16)
        };
        if logical_sector_count == 0 {
            return Err(AHCICommandError::InvalidArgument(
                "IDENTIFY DEVICE returned zero logical sectors",
            ));
        }
        let sector_size_info = Self::identify_word(data, 106);
        let logical_sector_size = if sector_size_info & 0xD000 == 0x5000 {
            let words_per_sector = (Self::identify_word(data, 117) as u32)
                | ((Self::identify_word(data, 118) as u32) << 16);
            words_per_sector.checked_mul(2).filter(|&size| size >= 512).ok_or(
                AHCICommandError::InvalidArgument(
                    "IDENTIFY DEVICE returned an invalid logical sector size",
                ),
            )?
        } else {
            512
        };

        Ok(ATAIdentifyInfo {
            serial_number: Self::identify_string(data, 10, 10),
            firmware_revision: Self::identify_string(data, 23, 4),
            model_number: Self::identify_string(data, 27, 20),
            logical_sector_count,
            logical_sector_size,
            supports_dma,
            supports_lba48,
        })
    }

    /// 识别设备
    /// 发送 IDENTIFY DEVICE 并保存该端口的磁盘信息
    pub fn identify_device(
        &mut self,
        port: AHCIPort,
    ) -> Result<ATAIdentifyInfo, AHCICommandError> {
        let port_index = usize::try_from(port.0)
            .ok()
            .filter(|&index| index < AHCI_MAX_PORTS)
            .ok_or(AHCICommandError::InvalidArgument("AHCI port is out of range"))?;
        let fis = AHCIRegisterH2DFIS::new(ATA_COMMAND_IDENTIFY_DEVICE);
        let layout = self.issue_ata_command(
            port,
            fis,
            ATADataDirection::D2H,
            512,
        )?;
        let mut raw_identify = [0u8; 512];
        unsafe {
            core::ptr::copy_nonoverlapping(
                (layout.data_buffer.0 | UNCHACHED_KERNEL_BASE) as *const u8,
                raw_identify.as_mut_ptr(),
                raw_identify.len(),
            );
        }
        let info = Self::parse_identify_data(&raw_identify)?;
        self.identify_info[port_index] = Some(info.clone());
        Ok(info)
    }
    /// 获取指定端口磁盘信息，未识别过返回 None
    pub fn identify_info(&self, port: AHCIPort) -> Option<&ATAIdentifyInfo> {
        let port_index = usize::try_from(port.0).ok()?;
        self.identify_info.get(port_index)?.as_ref()
    }
    /// 发送 IO 请求
    fn validate_io_request(
        &self,
        port: AHCIPort,
        lba: u64,
        sector_count: u32,
        buffer_len: usize,
    ) -> Result<usize, AHCICommandError> {
        let info = self
            .identify_info(port)
            .ok_or(AHCICommandError::Unsupported(
                "IDENTIFY DEVICE must complete before disk I/O",
            ))?;
        if !info.supports_dma {
            return Err(AHCICommandError::Unsupported(
                "the SATA disk does not report DMA support",
            ));
        }
        if !info.supports_lba48 {
            return Err(AHCICommandError::Unsupported(
                "the SATA disk does not report LBA48 support",
            ));
        }
        if sector_count == 0 || sector_count > ATA_LBA48_MAX_SECTORS_PER_COMMAND {
            return Err(AHCICommandError::InvalidArgument(
                "invalid LBA48 sector count",
            ));
        }
        let end_lba = lba
            .checked_add(sector_count as u64)
            .ok_or(AHCICommandError::InvalidArgument("LBA range overflow"))?;
        if end_lba > info.logical_sector_count {
            return Err(AHCICommandError::InvalidArgument(
                "disk request exceeds IDENTIFY DEVICE capacity",
            ));
        }
        let transfer_bytes = (sector_count as usize)
            .checked_mul(info.logical_sector_size as usize)
            .ok_or(AHCICommandError::InvalidArgument(
                "disk request byte count overflow",
            ))?;
        if transfer_bytes > DATA_BUFFER_SIZE || buffer_len != transfer_bytes {
            return Err(AHCICommandError::InvalidArgument(
                "buffer length must equal the request size and fit in 4 KiB",
            ));
        }
        Ok(transfer_bytes)
    }
    /// 使用 READ DMA EXT 读取连续逻辑扇区
    pub fn read_sectors(
        &mut self,
        port: AHCIPort,
        lba: u64,
        sector_count: u32,
        buffer: &mut [u8],
    ) -> Result<(), AHCICommandError> {
        let transfer_bytes = self.validate_io_request(port, lba, sector_count, buffer.len())?;
        let fis = AHCIRegisterH2DFIS::new_lba48(
            ATA_COMMAND_READ_DMA_EXT,
            lba,
            sector_count,
        )
        .ok_or(AHCICommandError::InvalidArgument("invalid LBA48 read request"))?;
        let layout = self.issue_ata_command(
            port,
            fis,
            ATADataDirection::D2H,
            transfer_bytes,
        )?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                (layout.data_buffer.0 | UNCHACHED_KERNEL_BASE) as *const u8,
                buffer.as_mut_ptr(),
                transfer_bytes,
            );
        }
        Ok(())
    }
    /// 使用 WRITE DMA EXT 写入连续逻辑扇区
    pub fn write_sectors(
        &mut self,
        port: AHCIPort,
        lba: u64,
        sector_count: u32,
        buffer: &[u8],
    ) -> Result<(), AHCICommandError> {
        let transfer_bytes = self.validate_io_request(port, lba, sector_count, buffer.len())?;
        let layout = self.port_dma_layout(port)?;
        // 共享 data buffer 可能仍被失败后未完成的命令使用，覆盖前必须确认端口空闲
        self.wait_command_ready(port)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.as_ptr(),
                (layout.data_buffer.0 | UNCHACHED_KERNEL_BASE) as *mut u8,
                transfer_bytes,
            );
        }
        let fis = AHCIRegisterH2DFIS::new_lba48(
            ATA_COMMAND_WRITE_DMA_EXT,
            lba,
            sector_count,
        )
        .ok_or(AHCICommandError::InvalidArgument("invalid LBA48 write request"))?;
        self.issue_ata_command(
            port,
            fis,
            ATADataDirection::H2D,
            transfer_bytes,
        )?;
        Ok(())
    }
    /// 初始化端口，返回是否成功
    /// 停止引擎、清除状态、分配 DMA 区域、启动引擎并 IDENTIFY DEVICE
    pub fn init_port(&mut self, port: AHCIPort) -> bool {
        let Some(port_index) = self.validate_sata_port(port) else {
            return false;
        };
        self.identify_info[port_index] = None;
        info!("Port {} is a SATA ATA disk. Initializing...", port.0);

        // 清除 U-Boot 的遗留状态，改用内核管理的 DMA 区域
        if let Err(err) = self.stop_port_engine(port) {
            error!("Failed to stop AHCI port {}: {:?}", port.0, err.regs);
            return false;
        }
        let pending_ci = self.port_reg_read(port, AHCIPortReg::Ci);
        let pending_sact = self.port_reg_read(port, AHCIPortReg::Sact);
        if pending_ci != 0 || pending_sact != 0 {
            error!(
                "AHCI port {} still has pending commands after stopping: PxCI=0x{:08x}, PxSACT=0x{:08x}",
                port.0, pending_ci, pending_sact
            );
            return false;
        }
        self.clear_port_status(port);

        let Some(layout) = self.prepare_port_dma(port, port_index) else {
            return false;
        };
        self.program_port_dma(port, layout);

        if let Err(err) = self.start_port_engine(port) {
            error!("Failed to start AHCI port {}: {:?}", port.0, err.regs);
            return false;
        }
        info!(
            "AHCI port {} initialized: CLB=0x{:x}, FB=0x{:x}, CTBA=0x{:x}, data=0x{:x}",
            port.0,
            layout.command_list.0,
            layout.received_fis.0,
            layout.command_table.0,
            layout.data_buffer.0
        );
        let identify = match self.identify_device(port) {
            Ok(info) => info,
            Err(err) => {
                error!("IDENTIFY DEVICE failed on AHCI port {}: {:?}", port.0, err);
                return false;
            }
        };
        info!(
            "SATA disk on port {}: model={}, serial={}, firmware={}, sectors={}, sector_size={}, DMA={}, LBA48={}",
            port.0,
            identify.model_number,
            identify.serial_number,
            identify.firmware_revision,
            identify.logical_sector_count,
            identify.logical_sector_size,
            identify.supports_dma,
            identify.supports_lba48
        );
        true
    }

    /// 判断指定端口是否连接了可用的 SATA ATA 磁盘
    pub fn is_usable_sata_disk(&self, port_num: u32) -> bool {
        if port_num >= AHCI_MAX_PORTS as u32 {
            return false;
        }
        let ports_implemented = self.reg_read(AHCIReg::HbaPi);
        // 端口已实现
        if ports_implemented & (1 << port_num) == 0 {
            return false;
        }
        let port = AHCIPort(port_num as isize);
        // 设备处于活动态并且建立了通信
        let ssts = self.port_reg_read(port, AHCIPortReg::Ssts);
        if ssts & 0xF != 3 || (ssts >> 8) & 0xF != 1 {
            return false;
        }
        // 签名为 ATA
        let sig = self.port_reg_read(port, AHCIPortReg::Sig);
        if sig != AHCIDevType::Ata as u32 {
            return false;
        }
        true
    }
    /// 获取指定端口的逻辑扇区大小，单位字节
    pub fn get_port_sector_size(&self, port: AHCIPort) -> Option<u32> {
        let port_num = port.0 as u32;
        if port_num >= AHCI_MAX_PORTS as u32 {
            return None;
        }
        let info = self.identify_info(port)?;
        Some(info.logical_sector_size)
    }
    /// 查找第一个已连接的 SATA 磁盘端口，返回端口
    pub fn find_first_sata_disk(&self) -> Option<AHCIPort> {
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        for port_num in 0..port_count {
            if self.is_usable_sata_disk(port_num) {
                return Some(AHCIPort(port_num as isize));
            }
        }
        None
    }
    /// 调试用，列出所有已连接的 SATA 磁盘
    pub fn list_sata_disks(&self) {
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        for port_num in 0..port_count {
            if self.is_usable_sata_disk(port_num) {
                println!("Found SATA disk at port {}", port_num);
            }
        }
    }
}

pub fn print_ahci_info() {
    let ctl = AHCI_CONTROLLER.lock();
    ctl.print_all_regs();
}
