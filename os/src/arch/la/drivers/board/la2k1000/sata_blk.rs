//! SATA 块设备驱动，实现了对 AHCI 控制器的访问和对 SATA 磁盘的读写操作
//! 各枚举定义了 AHCI 控制器和端口的寄存器偏移
//! 寄存器偏移的具体值参考 AHCI 规范和 llm 工具

use alloc::{sync::Arc, vec::Vec};
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
    },
    mm::PhysAddr,
};
use lazy_static::lazy_static;



/// SATA 块设备
pub struct SataBlock {
    /// 全局唯一 AHCI 控制器，锁覆盖一次完整命令的构造、提交和完成过程
    ctl: &'static Mutex<AHCIController>,
    /// 当前块设备对应的 AHCI 端口
    port: AHCIPort,
    // 此锁暂时未使用
    _lock: spin::Mutex<()>,
}

impl SataBlock {
    pub fn new() -> Self {
        let port = AHCI_CONTROLLER
            .lock()
            .find_first_sata_disk()
            .expect("No SATA disk found");
        let block = SataBlock {
            ctl: &AHCI_CONTROLLER,
            port,
            _lock: spin::Mutex::new(())
        };
        assert!(block.init(), "Failed to initialize SATA port {}", port.0);
        block
    }
    pub fn new_with_port(port: AHCIPort) -> Self {
        SataBlock {
            ctl: &AHCI_CONTROLLER,
            port,
            _lock: spin::Mutex::new(())
        }
    }
    pub fn init(&self) -> bool {
        let mut ctl = self.ctl.lock();
        ctl.init_port(self.port)
    }
    pub fn read_block(){

    }
    pub fn write_block(){

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
// PxCMD 寄存器位
const PXCMD_ST: u32 = 1 << 0;
const PXCMD_FRE: u32 = 1 << 4;
const PXCMD_FR: u32 = 1 << 14;
const PXCMD_CR: u32 = 1 << 15;
// --- 其他杂项定义 ---
const PRDT_MAX_BYTE_COUNT: usize = 1 << 22;
const PRDT_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;
// Host to Device，从控制器到外设
const FIS_TYPE_REGISTER_H2D: u8 = 0x27;
const FIS_REGISTER_H2D_COMMAND: u8 = 1 << 7;
const ATA_DEVICE_LBA: u8 = 1 << 6;
const ATA_LBA48_LIMIT: u64 = 1 << 48;
const ATA_LBA48_MAX_SECTORS_PER_COMMAND: u32 = 1 << 16;

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

const _: () = assert!(core::mem::size_of::<AHCIPRDTEntry>() == 16);

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

lazy_static! {
    /// AHCI 控制器实例
    pub static ref AHCI_CONTROLLER: Mutex<AHCIController> = Mutex::new(
        AHCIController::new(*SATA_AHCI_MMIO_PA)
    );
}

lazy_static! {
    pub static ref SATA_BLOCK: Arc<SataBlock> = {
        Arc::new(SataBlock::new())
    };
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

#[derive(Debug)]
pub struct AHCIError {
    /// 发生错误时的端口寄存器快照
    ///
    /// 数组下标为寄存器相对端口基址的字节偏移除以 4
    /// 未在 AHCIPortReg::ALL 中列出的保留位置保持为 0
    pub regs: [u32; 0x44 / 4],
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
}

impl AHCIController {
    pub fn new(base_addr: usize) -> Self {
        AHCIController {
            base_addr,
            dma_buffers: Vec::new(),
            port_dma_bases: [None; AHCI_MAX_PORTS],
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
    /// 初始化端口，返回是否成功
    pub fn init_port(&mut self, port: AHCIPort) -> bool {
        let Some(port_index) = self.validate_sata_port(port) else {
            return false;
        };
        info!("Port {} is a SATA ATA disk. Initializing...", port.0);

        // 清除 U-Boot 的遗留状态，改用内核管理的 DMA 区域
        if let Err(err) = self.stop_port_engine(port) {
            error!("Failed to stop AHCI port {}: {:?}", port.0, err.regs);
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
        true
    }
    /// 查找第一个已连接的 SATA 磁盘端口，返回端口
    pub fn find_first_sata_disk(&self) -> Option<AHCIPort> {
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        let ports_implemented = self.reg_read(AHCIReg::HbaPi);
        for port_num in 0..port_count {
            if ports_implemented & (1 << port_num) == 0 {
                continue;
            }
            let port = AHCIPort(port_num as isize);
            let ssts = self.port_reg_read(port, AHCIPortReg::Ssts);
            if ssts & 0xF != 3 || (ssts >> 8) & 0xF != 1 {
                continue;
            }
            let sig = self.port_reg_read(port, AHCIPortReg::Sig);
            if sig == AHCIDevType::Ata as u32 {
                return Some(port);
            }
        }
        None
    }
    /// 调试用，列出所有已连接的 SATA 磁盘
    pub fn list_sata_disks(&self) {
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        let ports_implemented = self.reg_read(AHCIReg::HbaPi);
        for port_num in 0..port_count {
            if ports_implemented & (1 << port_num) == 0 {
                continue;
            }
            let port = AHCIPort(port_num as isize);
            let ssts = self.port_reg_read(port, AHCIPortReg::Ssts);
            if ssts & 0xF != 3 || (ssts >> 8) & 0xF != 1 {
                continue;
            }
            let sig = self.port_reg_read(port, AHCIPortReg::Sig);
            if sig == AHCIDevType::Ata as u32 {
                println!("Found SATA disk at port {}", port_num);
            }
        }
    }
    pub fn read(){

    }
    pub fn write(){

    }
}

pub fn print_ahci_info() {
    let ctl = AHCI_CONTROLLER.lock();
    ctl.print_all_regs();
}
