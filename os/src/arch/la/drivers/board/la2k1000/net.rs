//! LA2k1000星云板网卡驱动
//!
//! 部分常量定义和初始化流程等参考了 U-Boot 和 2025-RocketOS 的实现

use core::hint::spin_loop;
use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};

use spin::Mutex;

use crate::arch::dma_barriar;
use crate::arch::drivers::pci::{scan_bus, CSpaceAccessMethod, BAR};
use crate::arch::timer::get_time_ms;
use crate::drivers::dma::{DmaBuffer, DMA_MEMORY};
use crate::drivers::net::{EthernetDevice, EthernetError};
use crate::{PAGE_SIZE, UNCACHED_KERNEL_BASE};

/// GMAC 在 PCI 总线上地址与类型
const GMAC_PCI_BUS: u8 = 0;
const GMAC_PCI_DEVICE: u8 = 3;
const GMAC_PCI_FUNCTION: u8 = 0;
const PCI_CLASS_NETWORK: u8 = 0x02;
const PCI_SUBCLASS_ETHERNET: u8 = 0x00;
/// PCI 命令寄存器的两个相关位
///
/// 允许 CPU 访问 MMIO
const PCI_COMMAND_MEMORY: u16 = 1 << 1;
/// 运行设备发起 DMA
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

/// 2K1000 GMAC DMA 相关常量
///
/// DMA 寄存器 MMIO 区域起始偏移
const DMA_REG_OFFSET: usize = 0x1000;
/// DMA 描述符环大小，每个环有 16 个描述符
const RING_SIZE: usize = 16;
/// 每个描述符有 2KB 缓冲区
const PACKET_BUFFER_SIZE: usize = 2048;
/// 定义 DMA 缓冲区布局
///
/// 为描述符预留一个标准页
/// 其中包含 TX 描述符表 + RX 描述符表
const DESCRIPTOR_PAGE_SIZE: usize = PAGE_SIZE;
/// 一个环的所有缓冲区总大小
const RING_BUFFER_SIZE: usize = RING_SIZE * PACKET_BUFFER_SIZE;
/// TX 缓冲区起始偏移
const TX_BUFFER_OFFSET: usize = DESCRIPTOR_PAGE_SIZE;
/// RX 缓冲区起始偏移
const RX_BUFFER_OFFSET: usize = TX_BUFFER_OFFSET + RING_BUFFER_SIZE;
/// DMA 内存区域总大小
const DMA_LAYOUT_SIZE: usize = RX_BUFFER_OFFSET + RING_BUFFER_SIZE;
/// 按页计
const DMA_LAYOUT_PAGES: usize = (DMA_LAYOUT_SIZE + PAGE_SIZE - 1) / PAGE_SIZE;
/// 超时
const OPERATION_TIMEOUT_MS: usize = 1000;

/// DMA 寄存器位定义
///
/// BusMode
/// Software Reset
const DMA_BUS_MODE_SWR: u32 = 1 << 0;
///
const DMA_BUS_MODE_PBL_32: u32 = 32 << 8;
const DMA_BUS_MODE_PBL_X8: u32 = 1 << 24;
const DMA_BUS_MODE_MB: u32 = 1 << 26;
/// Control
const DMA_CONTROL_SR: u32 = 1 << 1;
const DMA_CONTROL_OSF: u32 = 1 << 2;
const DMA_CONTROL_ST: u32 = 1 << 13;
const DMA_CONTROL_TSF: u32 = 1 << 21;
const DMA_CONTROL_RSF: u32 = 1 << 25;

/// MAC 寄存器位定义
///
/// Config
/// r/x enable
const MAC_CONFIG_RE: u32 = 1 << 2;
const MAC_CONFIG_TE: u32 = 1 << 3;
const MAC_CONFIG_DM: u32 = 1 << 11;
const MAC_CONFIG_FES: u32 = 1 << 14;
const MAC_CONFIG_PS: u32 = 1 << 15;
const MAC_CONFIG_TC: u32 = 1 << 24;
/// FrameFilter
/// 接收所有多播帧
const MAC_FRAME_FILTER_RA: u32 = 1 << 31;
/// FlowControl
/// Pause Time 字段位于高 16 位，当前设置为最大值
const MAC_FLOW_CONTROL_PAUSE_TIME_MAX: u32 = 0xffff << 16;
/// InterruptMask
/// 屏蔽 RGMII 链路状态变化中断
const MAC_INTERRUPT_MASK_RGSMII: u32 = 1 << 0;
/// RgsmiiStatus
/// 双工模式，置位表示全双工，清零表示半双工
const MAC_RGSMII_DUPLEX: u32 = 1 << 0;
/// 链路速度字段
const MAC_RGSMII_SPEED_MASK: u32 = 0x3 << 1;
const MAC_RGSMII_SPEED_10: u32 = 0x0 << 1;
const MAC_RGSMII_SPEED_100: u32 = 0x1 << 1;
const MAC_RGSMII_SPEED_1000: u32 = 0x2 << 1;
/// 链路状态，置位表示链路已建立
const MAC_RGSMII_LINK_UP: u32 = 1 << 3;

/// DMA 描述符位定义
const DESC_OWN: u32 = 1 << 31;
const DESC_ERROR: u32 = 1 << 15;
const DESC_RX_FIRST: u32 = 1 << 9;
const DESC_RX_LAST: u32 = 1 << 8;
const DESC_RX_FRAME_LENGTH_MASK: u32 = 0x3fff << 16;
const DESC_RX_FRAME_LENGTH_SHIFT: u32 = 16;
const DESC_BUFFER1_SIZE_MASK: u32 = 0x1fff;
const RX_DESC_END_OF_RING: u32 = 1 << 15;
const TX_DESC_END_OF_RING: u32 = 1 << 21;
const DESC_TX_FIRST: u32 = 1 << 28;
const DESC_TX_LAST: u32 = 1 << 29;
const DESC_TX_INTERRUPT: u32 = 1 << 30;

const DMA_STATUS_CLEAR_ALL: u32 = 0x1ffff;
const DMA_STATUS_FATAL_BUS_ERROR: u32 = 1 << 13;

/// MAC MMIO 寄存器偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum LA2k1000GmacReg {
    Config = 0x0000,
    FrameFilter = 0x0004,
    GmiiAddr = 0x0010,
    GmiiData = 0x0014,
    FlowControl = 0x0018,
    Version = 0x0020,
    InterruptStatus = 0x0038,
    InterruptMask = 0x003c,
    Addr0High = 0x0040,
    Addr0Low = 0x0044,
    RgsmiiStatus = 0x00d8,
}

impl LA2k1000GmacReg {
    pub const ALL: &'static [Self] = &[
        Self::Config,
        Self::FrameFilter,
        Self::GmiiAddr,
        Self::GmiiData,
        Self::FlowControl,
        Self::Version,
        Self::InterruptStatus,
        Self::InterruptMask,
        Self::Addr0High,
        Self::Addr0Low,
        Self::RgsmiiStatus,
    ];

    pub const fn offset(self) -> usize {
        self as usize
    }
}

/// DMA MMIO 寄存器偏移
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum LA2k1000DmaReg {
    BusMode = 0x0000,
    TxPollDemand = 0x0004,
    RxPollDemand = 0x0008,
    RxBaseAddr = 0x000c,
    TxBaseAddr = 0x0010,
    Status = 0x0014,
    Control = 0x0018,
    Interrupt = 0x001c,
    AxiBusMode = 0x0028,
    TxCurrDesc = 0x0048,
    RxCurrDesc = 0x004c,
    TxCurrAddr = 0x0050,
    RxCurrAddr = 0x0054,
    HwFeature = 0x0058,
}

impl LA2k1000DmaReg {
    pub const ALL: &'static [Self] = &[
        Self::BusMode,
        Self::TxPollDemand,
        Self::RxPollDemand,
        Self::RxBaseAddr,
        Self::TxBaseAddr,
        Self::Status,
        Self::Control,
        Self::Interrupt,
        Self::AxiBusMode,
        Self::TxCurrDesc,
        Self::RxCurrDesc,
        Self::TxCurrAddr,
        Self::RxCurrAddr,
        Self::HwFeature,
    ];

    pub const fn offset(self) -> usize {
        self as usize
    }
}

/// DMA 描述符
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(16))]
struct DmaDesc {
    status: u32,
    length: u32,
    buffer1: u32,
    buffer2: u32,
}

const _: () = assert!(size_of::<DmaDesc>() == 16);

/// 目前使用的 DMA 缓冲区布局定义
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum GmacDmaRegion {
    TxDescriptors = 0,
    RxDescriptors = RING_SIZE * size_of::<DmaDesc>(),
    TxBuffers = TX_BUFFER_OFFSET,
    RxBuffers = RX_BUFFER_OFFSET,
}

impl GmacDmaRegion {
    const fn offset(self) -> usize {
        self as usize
    }
}

pub struct LA2k1000NetWrapper {
    inner: Mutex<LA2k1000NetDevice>,
}

pub struct LA2k1000NetDevice {
    /// Physical MMIO address from PCI BAR0.
    base_addr: usize,
    mac_addr: [u8; 6],
    /// One contiguous allocation containing all descriptor rings and buffers.
    dma_buffer: DmaBuffer,
    tx_index: usize,
    rx_index: usize,
}

impl LA2k1000NetWrapper {
    pub fn new() -> Self {
        let base_addr = scan_and_enable_gmac0().expect("2K1000 GMAC0 PCI device is unavailable");
        let device = LA2k1000NetDevice::new(base_addr)
            .expect("failed to allocate or initialize 2K1000 GMAC0");
        Self {
            inner: Mutex::new(device),
        }
    }
}

impl EthernetDevice for LA2k1000NetWrapper {
    fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().mac_addr
    }

    fn can_receive(&self) -> bool {
        self.inner.lock().can_recv()
    }

    fn can_transmit(&self) -> bool {
        self.inner.lock().can_send()
    }

    fn receive_frame(&self, buffer: &mut [u8]) -> Result<usize, EthernetError> {
        self.inner.lock().recv_frame(buffer)
    }

    fn transmit_frame(&self, frame: &[u8]) -> Result<(), EthernetError> {
        self.inner.lock().send_frame(frame)
    }
}

/// 从 PCI 总线扫描并启用 2K1000 GMAC0 设备，返回 MMIO 区域基址
fn scan_and_enable_gmac0() -> Option<usize> {
    let access = CSpaceAccessMethod::MemoryMapped;
    let device = scan_bus(access).find(|device| {
        device.loc.bus == GMAC_PCI_BUS
            && device.loc.device == GMAC_PCI_DEVICE
            && device.loc.function == GMAC_PCI_FUNCTION
            && device.id.class == PCI_CLASS_NETWORK
            && device.id.subclass == PCI_SUBCLASS_ETHERNET
    })?;

    let base = match device.get_bar(0)? {
        BAR::Memory(base, _, _, _) if base != 0 && base <= usize::MAX as u64 => base as usize,
        _ => return None,
    };

    unsafe {
        let command = access.read16(device.loc, 0x04);
        access.write16(
            device.loc,
            0x04,
            command | PCI_COMMAND_MEMORY | PCI_COMMAND_BUS_MASTER,
        );
    }
    dma_barriar();

    let command = device.command();
    if command & (PCI_COMMAND_MEMORY | PCI_COMMAND_BUS_MASTER)
        != PCI_COMMAND_MEMORY | PCI_COMMAND_BUS_MASTER
    {
        return None;
    }

    info!(
        "2K1000 GMAC0: PCI {:02x}:{:02x}.{} vendor={:04x} device={:04x} BAR0=0x{:x}",
        device.loc.bus,
        device.loc.device,
        device.loc.function,
        device.id.vendor_id,
        device.id.device_id,
        base
    );
    Some(base)
}

impl LA2k1000NetDevice {
    fn new(base_addr: usize) -> Result<Self, EthernetError> {
        let dma = DMA_MEMORY
            .exclusive_access()
            .alloc(DMA_LAYOUT_PAGES)
            .ok_or(EthernetError::Driver)?;
        dma.zero();
        let dma_base = dma.phys_addr();
        // 检查 DMA 内存是否越界，越界则释放并返回错误
        if dma_base.0.checked_add(DMA_LAYOUT_SIZE).is_none()
            || dma_base.0 + DMA_LAYOUT_SIZE > u32::MAX as usize
        {
            DMA_MEMORY
                .exclusive_access()
                .dealloc(dma.phys_addr(), dma.pages());
            return Err(EthernetError::Driver);
        }

        let mut device = Self {
            base_addr,
            mac_addr: [0; 6],
            dma_buffer: dma,
            tx_index: 0,
            rx_index: 0,
        };
        device.init()?;
        Ok(device)
    }
    /// 启动与初始化网卡设备
    fn init(&mut self) -> Result<(), EthernetError> {
        // 关中断
        self.dma_write(LA2k1000DmaReg::Interrupt, 0);
        // 停止收发
        self.dma_write(LA2k1000DmaReg::Control, 0);
        // 停止 MAC
        self.gmac_write(
            LA2k1000GmacReg::Config,
            self.gmac_read(LA2k1000GmacReg::Config) & !(MAC_CONFIG_RE | MAC_CONFIG_TE),
        );

        // 复位 DMA 并等待至完成
        self.dma_write(LA2k1000DmaReg::BusMode, DMA_BUS_MODE_SWR);
        self.wait_reg_clear(
            DMA_REG_OFFSET + LA2k1000DmaReg::BusMode.offset(),
            DMA_BUS_MODE_SWR,
        )?;
        // 读取 MAC 地址，若无效则使用一个硬编码的地址
        self.mac_addr = self.read_mac_address();
        if !valid_unicast_mac(self.mac_addr) {
            self.mac_addr = [0x02, 0x00, 0x00, 0x2b, 0x10, 0x00];
            warn!("2K1000 GMAC0: firmware MAC invalid, using a local address");
        }
        // 写入 MAC 地址到寄存器
        self.write_mac_address(self.mac_addr);

        self.init_descriptor_rings();

        /* 配置 DMA */
        // 设置 Mixed Burst，支持混合突发访问
        // 设置 PBL 为 32，最大突发长度为 32 个数据传输
        // 启用 PBL 扩展模式
        self.dma_write(
            LA2k1000DmaReg::BusMode,
            DMA_BUS_MODE_MB | DMA_BUS_MODE_PBL_X8 | DMA_BUS_MODE_PBL_32,
        );
        // 设置 GMAC 进行 DMA 时的 AXI 总线模式
        // 参考 RocketOS 的实现
        // 没有找到官方文档说明，但 linux 也有对应设置，暂时沿用
        self.dma_write(LA2k1000DmaReg::AxiBusMode, 0x0077_00ff);
        // 配置收发缓冲区地址
        self.dma_write(
            LA2k1000DmaReg::RxBaseAddr,
            self.dma_region_phys(GmacDmaRegion::RxDescriptors, 0) as u32,
        );
        self.dma_write(
            LA2k1000DmaReg::TxBaseAddr,
            self.dma_region_phys(GmacDmaRegion::TxDescriptors, 0) as u32,
        );
        // 清除 DMA 状态寄存器
        self.dma_write(LA2k1000DmaReg::Status, DMA_STATUS_CLEAR_ALL);

        /* 配置 MAC */
        // 先获取 RGMII 状态寄存器，判断链路状态和速率
        let rgmii = self.gmac_read(LA2k1000GmacReg::RgsmiiStatus);
        if rgmii & MAC_RGSMII_LINK_UP == 0 {
            warn!("2K1000 GMAC0: RGMII link is down");
        }
        let mut config = self.gmac_read(LA2k1000GmacReg::Config);
        // 清除旧的速度和双工模式位
        config &= !(MAC_CONFIG_PS | MAC_CONFIG_FES);
        config |= MAC_CONFIG_TC;
        // 根据 RGMII 状态设置速度和双工模式
        if rgmii & MAC_RGSMII_DUPLEX != 0 {
            config |= MAC_CONFIG_DM;
        } else {
            config &= !MAC_CONFIG_DM;
        }
        match rgmii & MAC_RGSMII_SPEED_MASK {
            MAC_RGSMII_SPEED_10 => config |= MAC_CONFIG_PS,
            MAC_RGSMII_SPEED_100 => config |= MAC_CONFIG_PS | MAC_CONFIG_FES,
            MAC_RGSMII_SPEED_1000 => {}
            _ => warn!("2K1000 GMAC0: invalid RGMII speed status=0x{:08x}", rgmii),
        }
        // 配置接收所有帧
        self.gmac_write(LA2k1000GmacReg::FrameFilter, MAC_FRAME_FILTER_RA);
        self.gmac_write(
            LA2k1000GmacReg::FlowControl,
            MAC_FLOW_CONTROL_PAUSE_TIME_MAX,
        );
        self.gmac_write(
            LA2k1000GmacReg::InterruptMask,
            self.gmac_read(LA2k1000GmacReg::InterruptMask) | MAC_INTERRUPT_MASK_RGSMII,
        );
        // 配置好 MAC 后再使能收发
        // 使能收发
        self.gmac_write(
            LA2k1000GmacReg::Config,
            config | MAC_CONFIG_RE | MAC_CONFIG_TE,
        );
        // 启动 DMA
        self.dma_write(
            LA2k1000DmaReg::Control,
            DMA_CONTROL_RSF | DMA_CONTROL_TSF | DMA_CONTROL_OSF | DMA_CONTROL_SR | DMA_CONTROL_ST,
        );
        // 提醒 RX DMA 重新检查描述符环
        self.dma_write(LA2k1000DmaReg::RxPollDemand, 0);
        // 打印调试信息
        let version = self.gmac_read(LA2k1000GmacReg::Version);
        let phy_id = self.read_phy_id(0);
        info!(
            "2K1000 GMAC0: version=0x{:08x} MAC={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} RGMII=0x{:08x} PHY={:?}",
            version,
            self.mac_addr[0],
            self.mac_addr[1],
            self.mac_addr[2],
            self.mac_addr[3],
            self.mac_addr[4],
            self.mac_addr[5],
            rgmii,
            phy_id
        );
        Ok(())
    }
    /// 初始化 DMA 描述符环
    fn init_descriptor_rings(&mut self) {
        for index in 0..RING_SIZE {
            self.write_desc(
                self.tx_desc_ptr(index),
                DmaDesc {
                    status: if index + 1 == RING_SIZE {
                        TX_DESC_END_OF_RING
                    } else {
                        0
                    },
                    length: 0,
                    buffer1: self.tx_buffer_phys(index) as u32,
                    buffer2: 0,
                },
            );
            self.recycle_rx_desc(index);
        }
        dma_barriar();
    }
    pub fn can_recv(&self) -> bool {
        dma_barriar();
        self.read_desc(self.rx_desc_ptr(self.rx_index)).status & DESC_OWN == 0
    }
    pub fn can_send(&self) -> bool {
        dma_barriar();
        self.read_desc(self.tx_desc_ptr(self.tx_index)).status & DESC_OWN == 0
    }
    /// 发送一帧
    pub fn send_frame(&mut self, frame: &[u8]) -> Result<(), EthernetError> {
        if frame.is_empty() || frame.len() > PACKET_BUFFER_SIZE {
            return Err(EthernetError::BufferTooSmall);
        }
        if !self.can_send() {
            return Err(EthernetError::Busy);
        }

        let index = self.tx_index;
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                self.dma_region_ptr(
                    GmacDmaRegion::TxBuffers,
                    index * PACKET_BUFFER_SIZE,
                ),
                frame.len(),
            );
        }
        let end_of_ring = if index + 1 == RING_SIZE {
            TX_DESC_END_OF_RING
        } else {
            0
        };
        self.write_desc(
            self.tx_desc_ptr(index),
            DmaDesc {
                status: DESC_OWN | DESC_TX_INTERRUPT | DESC_TX_FIRST | DESC_TX_LAST | end_of_ring,
                length: frame.len() as u32 & DESC_BUFFER1_SIZE_MASK,
                buffer1: self.tx_buffer_phys(index) as u32,
                buffer2: 0,
            },
        );
        dma_barriar();
        self.dma_write(LA2k1000DmaReg::TxPollDemand, 0);

        let start = get_time_ms();
        loop {
            dma_barriar();
            let descriptor = self.read_desc(self.tx_desc_ptr(index));
            if descriptor.status & DESC_OWN == 0 {
                if descriptor.status & DESC_ERROR != 0 {
                    self.finish_tx(index);
                    return Err(EthernetError::Driver);
                }
                self.finish_tx(index);
                return Ok(());
            }
            if get_time_ms().saturating_sub(start) >= OPERATION_TIMEOUT_MS {
                error!(
                    "2K1000 GMAC0 TX timeout: desc={} status=0x{:08x} dma=0x{:08x}",
                    index,
                    descriptor.status,
                    self.dma_read(LA2k1000DmaReg::Status)
                );
                return Err(EthernetError::Driver);
            }
            spin_loop();
        }
    }
    /// 接收一帧
    pub fn recv_frame(&mut self, output: &mut [u8]) -> Result<usize, EthernetError> {
        if !self.can_recv() {
            self.check_dma_error();
            return Err(EthernetError::Busy);
        }

        let index = self.rx_index;
        let descriptor = self.read_desc(self.rx_desc_ptr(index));
        let raw_length = ((descriptor.status & DESC_RX_FRAME_LENGTH_MASK)
            >> DESC_RX_FRAME_LENGTH_SHIFT) as usize;
        let valid = descriptor.status & DESC_ERROR == 0
            && descriptor.status & DESC_RX_FIRST != 0
            && descriptor.status & DESC_RX_LAST != 0
            && raw_length >= 4
            && raw_length <= PACKET_BUFFER_SIZE;
        if !valid {
            self.finish_rx(index);
            return Err(EthernetError::Driver);
        }

        // Legacy GMAC reports the Ethernet FCS in the receive frame length.
        let length = raw_length - 4;
        if length > output.len() {
            self.finish_rx(index);
            return Err(EthernetError::BufferTooSmall);
        }
        dma_barriar();
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.dma_region_ptr(
                    GmacDmaRegion::RxBuffers,
                    index * PACKET_BUFFER_SIZE,
                ),
                output.as_mut_ptr(),
                length,
            );
        }
        self.finish_rx(index);
        Ok(length)
    }
    /// 发送完成后更新描述符状态和索引
    fn finish_tx(&mut self, index: usize) {
        self.write_desc(
            self.tx_desc_ptr(index),
            DmaDesc {
                status: if index + 1 == RING_SIZE {
                    TX_DESC_END_OF_RING
                } else {
                    0
                },
                length: 0,
                buffer1: self.tx_buffer_phys(index) as u32,
                buffer2: 0,
            },
        );
        self.tx_index = (index + 1) % RING_SIZE;
        self.clear_dma_status();
    }
    /// 接收完成后更新描述符状态和索引
    fn finish_rx(&mut self, index: usize) {
        self.recycle_rx_desc(index);
        dma_barriar();
        self.rx_index = (index + 1) % RING_SIZE;
        self.dma_write(LA2k1000DmaReg::RxPollDemand, 0);
        self.clear_dma_status();
    }
    /// 回收 RX 描述符，将其状态设置为 DESC_OWN 并重新放入环中
    fn recycle_rx_desc(&self, index: usize) {
        self.write_desc(
            self.rx_desc_ptr(index),
            DmaDesc {
                status: DESC_OWN,
                length: (PACKET_BUFFER_SIZE as u32 & DESC_BUFFER1_SIZE_MASK)
                    | if index + 1 == RING_SIZE {
                        RX_DESC_END_OF_RING
                    } else {
                        0
                    },
                buffer1: self.rx_buffer_phys(index) as u32,
                buffer2: 0,
            },
        );
    }
    /// 清除 DMA 状态寄存器中的所有中断标志位
    fn clear_dma_status(&self) {
        let status = self.dma_read(LA2k1000DmaReg::Status);
        if status != 0 {
            self.dma_write(LA2k1000DmaReg::Status, status);
        }
    }
    /// 检查错误
    fn check_dma_error(&self) {
        let status = self.dma_read(LA2k1000DmaReg::Status);
        if status & DMA_STATUS_FATAL_BUS_ERROR != 0 {
            error!("2K1000 GMAC0 fatal DMA bus error: status=0x{:08x}", status);
        }
    }
    /// 读取 MAC 地址
    fn read_mac_address(&self) -> [u8; 6] {
        let low = self.gmac_read(LA2k1000GmacReg::Addr0Low);
        let high = self.gmac_read(LA2k1000GmacReg::Addr0High);
        [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ]
    }
    // 修改 MAC 地址
    fn write_mac_address(&self, address: [u8; 6]) {
        let low = u32::from_le_bytes([address[0], address[1], address[2], address[3]]);
        let high = u16::from_le_bytes([address[4], address[5]]) as u32;
        self.gmac_write(LA2k1000GmacReg::Addr0Low, low);
        self.gmac_write(LA2k1000GmacReg::Addr0High, high);
    }
    /// 读取 PHY ID
    fn read_phy_id(&self, phy: u8) -> Option<u32> {
        let id1 = self.mdio_read(phy, 2)?;
        let id2 = self.mdio_read(phy, 3)?;
        let id = ((id1 as u32) << 16) | id2 as u32;
        if id == 0 || id == u32::MAX {
            None
        } else {
            Some(id)
        }
    }
    /// 通过 MDIO 读取 PHY 寄存器
    fn mdio_read(&self, phy: u8, register: u8) -> Option<u16> {
        const GMII_BUSY: u32 = 1;
        const GMII_CSR_CLOCK_4: u32 = 4 << 2;
        let command = ((phy as u32 & 0x1f) << 11)
            | ((register as u32 & 0x1f) << 6)
            | GMII_CSR_CLOCK_4
            | GMII_BUSY;
        self.gmac_write(LA2k1000GmacReg::GmiiAddr, command);
        let start = get_time_ms();
        // 等待 GMII_BUSY 清除，表示读取完成
        while self.gmac_read(LA2k1000GmacReg::GmiiAddr) & GMII_BUSY != 0 {
            if get_time_ms().saturating_sub(start) >= OPERATION_TIMEOUT_MS {
                return None;
            }
            spin_loop();
        }
        Some(self.gmac_read(LA2k1000GmacReg::GmiiData) as u16)
    }
    // 等待指定偏移处寄存器 mask 位清除，用于发送命令后确保操作完成
    fn wait_reg_clear(&self, offset: usize, mask: u32) -> Result<(), EthernetError> {
        let start = get_time_ms();
        while self.read_reg(offset) & mask != 0 {
            if get_time_ms().saturating_sub(start) >= OPERATION_TIMEOUT_MS {
                return Err(EthernetError::Driver);
            }
            spin_loop();
        }
        Ok(())
    }
    /* 描述符指针 */
    fn tx_desc_ptr(&self, index: usize) -> *mut DmaDesc {
        self.dma_region_ptr(
            GmacDmaRegion::TxDescriptors,
            index * size_of::<DmaDesc>(),
        )
    }
    fn rx_desc_ptr(&self, index: usize) -> *mut DmaDesc {
        self.dma_region_ptr(
            GmacDmaRegion::RxDescriptors,
            index * size_of::<DmaDesc>(),
        )
    }
    /* 从索引获取缓冲区物理地址 */
    fn tx_buffer_phys(&self, index: usize) -> usize {
        self.dma_region_phys(GmacDmaRegion::TxBuffers, index * PACKET_BUFFER_SIZE)
    }
    fn rx_buffer_phys(&self, index: usize) -> usize {
        self.dma_region_phys(GmacDmaRegion::RxBuffers, index * PACKET_BUFFER_SIZE)
    }
    fn dma_region_phys(&self, region: GmacDmaRegion, offset: usize) -> usize {
        self.dma_buffer.phys_addr().0 + region.offset() + offset
    }
    /// 获取可内核访问的带（非缓存）窗口地址
    fn dma_region_window_addr(&self, region: GmacDmaRegion, offset: usize) -> usize {
        self.dma_region_phys(region, offset) | UNCACHED_KERNEL_BASE
    }
    fn dma_region_ptr<T>(&self, region: GmacDmaRegion, offset: usize) -> *mut T {
        self.dma_region_window_addr(region, offset) as *mut T
    }
    /* 读写描述符 */
    fn read_desc(&self, descriptor: *mut DmaDesc) -> DmaDesc {
        unsafe { read_volatile(descriptor) }
    }
    fn write_desc(&self, descriptor: *mut DmaDesc, value: DmaDesc) {
        unsafe { write_volatile(descriptor, value) }
    }
    /* 读写寄存器 */
    fn gmac_read(&self, reg: LA2k1000GmacReg) -> u32 {
        self.read_reg(reg.offset())
    }
    fn gmac_write(&self, reg: LA2k1000GmacReg, value: u32) {
        self.write_reg(reg.offset(), value)
    }
    fn dma_read(&self, reg: LA2k1000DmaReg) -> u32 {
        self.read_reg(DMA_REG_OFFSET + reg.offset())
    }
    fn dma_write(&self, reg: LA2k1000DmaReg, value: u32) {
        self.write_reg(DMA_REG_OFFSET + reg.offset(), value)
    }
    fn read_reg(&self, offset: usize) -> u32 {
        let address = (self.base_addr + offset) | UNCACHED_KERNEL_BASE;
        let value = unsafe { read_volatile(address as *const u32) };
        dma_barriar();
        value
    }
    fn write_reg(&self, offset: usize, value: u32) {
        let address = (self.base_addr + offset) | UNCACHED_KERNEL_BASE;
        unsafe { write_volatile(address as *mut u32, value) };
        dma_barriar();
    }
}

// / 检查 MAC 地址是否为有效的单播地址
fn valid_unicast_mac(address: [u8; 6]) -> bool {
    address != [0; 6] && address != [0xff; 6] && address[0] & 1 == 0
}
