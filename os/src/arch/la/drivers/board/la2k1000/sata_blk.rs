//! SATA 块设备驱动
//! 各枚举定义了 AHCI 控制器和端口的寄存器偏移
//! 寄存器偏移的具体值参考 AHCI 规范和 llm 工具

use crate::{UNCHACHED_KERNEL_BASE, arch::config::SATA_AHCI_MMIO_PA};
use spin::{Mutex, lazy};
use alloc::{
    sync::Arc,
    vec::Vec
};
use lazy_static::lazy_static;

lazy_static! {
    /// AHCI 控制器实例
    pub static ref AHCI_CONTROLLER: Mutex<AHCIController> = Mutex::new(
        AHCIController::new(*SATA_AHCI_MMIO_PA)
    );
}

lazy_static! {
    pub static ref SATA_BLOCK: Arc<SataBlock> = {
        SataBlock::new_with_port(AHCI_CONTROLLER.lock().find_first_sata_disk().expect("No SATA disk found")).into()
    };
}

/// SATA 块设备
pub struct SataBlock {
    ctl: &'static Mutex<AHCIController>,
    port: AHCIPort,
    // 此锁暂时未使用
    _lock: spin::Mutex<()>,
}

impl SataBlock {
    pub fn new() -> Self {
        SataBlock {
            ctl: &AHCI_CONTROLLER,
            port: AHCIPort(0),
            _lock: spin::Mutex::new(())
        }
    }
    pub fn new_with_port(port: AHCIPort) -> Self {
        SataBlock {
            ctl: &AHCI_CONTROLLER,
            port,
            _lock: spin::Mutex::new(())
        }
    }
    pub fn init(&self) {
        let ctl = self.ctl.lock();
        ctl.init();
        ctl.init_port(self.port);
    }
    pub fn read_block(){

    }
    pub fn write_block(){

    }
}

/// AHCI 控制器
pub struct AHCIController {
    // 控制器mmio基址
    base_addr: usize,
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
/// 端口号必须小于 CAP.NP + 1，且其位必须在 PI 中置位；实际读写前还应
/// 通过 PxSSTS 检查是否已连接并激活设备
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
    /// Port Interrupt Status（W1C）
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
    /// FR[14] / CR[15]：FIS 接收 / Command List 引擎运行状态，只读
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
    /// SACT[n]：NCQ 命令槽 n 正在执行，提交 FPDMA QUEUED 命令时先设置
    /// SACT[n]，再设置 CI[n]；普通非 NCQ 单槽实现不使用它
    Sact = 0x34,
    /// Command Issue
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

impl AHCIController {
    pub fn new(base_addr: usize) -> Self {
        AHCIController {
            base_addr,
        }
    }
    #[inline(always)]
    pub fn windowed_base_addr(&self) -> usize {
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
        for reg in [
            AHCIPortReg::Clb,
            AHCIPortReg::Clbu,
            AHCIPortReg::Fb,
            AHCIPortReg::Fbu,
            AHCIPortReg::Is,
            AHCIPortReg::Ie,
            AHCIPortReg::Cmd,
            AHCIPortReg::Tfd,
            AHCIPortReg::Sig,
            AHCIPortReg::Ssts,
            AHCIPortReg::Sctl,
            AHCIPortReg::Serr,
            AHCIPortReg::Sact,
            AHCIPortReg::Ci,
            AHCIPortReg::Sntf,
            AHCIPortReg::Fbs,
        ] {
            let value = self.port_reg_read(port, reg);
            println!("{:?} (0x{:02X}): 0x{:08X}", reg, reg as usize, value);
        }
    }
    pub fn init(&self) {
        // 初始化AHCI控制器
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        let slot_count = (hba_cap >> 8) & 0x1F;
        let supports_64bit = (hba_cap >> 31) & 0x1 == 1;
        // todo：根据需要初始化控制器
    }
    pub fn init_port(&self, port: AHCIPort) {
        // 初始化指定端口
        let ssts = self.port_reg_read(port, AHCIPortReg::Ssts);
        let det = ssts & 0xF;
        let ipm = (ssts >> 8) & 0xF;
        // todo：启动
    }
    pub fn find_first_sata_disk(&self) -> Option<AHCIPort> {
        let hba_cap = self.reg_read(AHCIReg::HbaCap);
        let port_count = (hba_cap & 0x1F) + 1;
        for port_num in 0..port_count {
            let port = AHCIPort(port_num as isize);
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
        for port_num in 0..port_count {
            let port = AHCIPort(port_num as isize);
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
