//! First-stage polling probe for the JH7110 DesignWare Ethernet QoS MAC.
//!
//! Register offsets and MDIO fields follow U-Boot's `dwc_eth_qos.h` and
//! `dwc_eth_qos.c`, with the JH7110 clock-range value from
//! `dwc_eth_qos_starfive.c`.

use crate::sync::MPSafeCell;
use core::ptr::{read_volatile, write_volatile};
//DWMAC0 网卡控制器
pub const DWMAC0_BASE: usize = 0x1603_0000;
//AON 时钟/复位控制器
//AON 是 Always On
const AON_CRG_BASE: usize = 0x1700_0000;
const AON_SYSCON_BASE: usize = 0x1701_0000;
const SYS_CRG_BASE: usize = 0x1302_0000;

//不同部分需要的不同的时钟ID
//如AHB：较低速的寄存器访问总线
//AXI：DMA 访问内存使用的高速总线
//计算为AON_CRG_BASE + clock_id * 4
const AONCLK_GMAC0_AHB: usize = 2;
const AONCLK_GMAC0_AXI: usize = 3;
const AONCLK_GMAC0_TX: usize = 5;
const SYSCLK_GMAC0_GTXCLK: usize = 108;
const SYSCLK_GMAC0_PTP: usize = 109;
const SYSCLK_GMAC0_GTXC: usize = 111;
//设置使能
const CLOCK_ENABLE: u32 = 1 << 31;

const AON_RESET_ASSERT: usize = 0x0038;
const AON_RESET_STATUS: usize = 0x003c;
const AONRST_GMAC0_AXI: u32 = 1 << 0;
const AONRST_GMAC0_AHB: u32 = 1 << 1;

// GMAC是支持千兆的MAC
const GMAC0_MODE_OFFSET: usize = 0x000c;
///左移十八位
const GMAC0_MODE_SHIFT: u32 = 18;
//PHY是 Physical Layer，物理层芯片
//111占三位
const GMAC_PHY_INTERFACE_MASK: u32 = 0x7;
//RGMII 是 MAC 和 PHY 之间的一种连接接口
//bits [20:18]中001表示RGMII模式
const GMAC_PHY_INTERFACE_RGMII: u32 = 0x1;
//版本
const MAC_VERSION: usize = 0x0110;
//功能
const MAC_HW_FEATURE0: usize = 0x011c;
const MAC_HW_FEATURE1: usize = 0x0120;
const MAC_HW_FEATURE2: usize = 0x0124;
const MAC_HW_FEATURE3: usize = 0x0128;
//MDIO读取配置 PHY 的管理信息
const MAC_MDIO_ADDRESS: usize = 0x0200;
const MAC_MDIO_DATA: usize = 0x0204;
//MAC主配置
const MAC_CONFIGURATION: usize = 0x0000;
//过滤器
const MAC_PACKET_FILTER: usize = 0x0008;
//收到的数据包发送到哪个RX队列
const MAC_RXQ_CTRL0: usize = 0x00a0;
//保存自己的mac地址，四十八位拆分成高低两个三十二位寄存器
const MAC_ADDRESS0_HIGH: usize = 0x0300;
const MAC_ADDRESS0_LOW: usize = 0x0304;
//控制发送队列 0，MTL 是MAC Transaction Layer位于 MAC 与 DMA 之间
const MTL_TXQ0_OPERATION_MODE: usize = 0x0d00;
//多发送队列调度时的权重配置
const MTL_TXQ0_QUANTUM_WEIGHT: usize = 0x0d18;
//控制接收队列 0
const MTL_RXQ0_OPERATION_MODE: usize = 0x0d30;

const DMA_SYSBUS_MODE: usize = 0x1004;
const DMA_CH0_CONTROL: usize = 0x1100;
const DMA_CH0_TX_CONTROL: usize = 0x1104;
const DMA_CH0_RX_CONTROL: usize = 0x1108;
const DMA_CH0_TXDESC_LIST_HI: usize = 0x1110;
const DMA_CH0_TXDESC_LIST: usize = 0x1114;
const DMA_CH0_RXDESC_LIST_HI: usize = 0x1118;
const DMA_CH0_RXDESC_LIST: usize = 0x111c;
const DMA_CH0_TXDESC_TAIL: usize = 0x1120;
const DMA_CH0_RXDESC_TAIL: usize = 0x1128;
const DMA_CH0_TX_RING_LENGTH: usize = 0x112c;
const DMA_CH0_RX_RING_LENGTH: usize = 0x1130;
const DMA_CH0_INTERRUPT_ENABLE: usize = 0x1134;
const DMA_CH0_CURRENT_TX_DESC: usize = 0x1144;
const DMA_CH0_CURRENT_RX_DESC: usize = 0x114c;
const DMA_CH0_STATUS: usize = 0x1160;

const MAC_CONFIG_CST: u32 = 1 << 21;
const MAC_CONFIG_ACS: u32 = 1 << 20;
const MAC_CONFIG_DM: u32 = 1 << 13;
const MAC_CONFIG_LOOPBACK: u32 = 1 << 12;
const MAC_CONFIG_TE: u32 = 1 << 1;
const MAC_CONFIG_RE: u32 = 1 << 0;
const MAC_PACKET_FILTER_PROMISCUOUS: u32 = 1 << 0;
const MAC_RXQ0_ENABLE_DCB: u32 = 2;

const MTL_TX_TQS_SHIFT: u32 = 16;
const MTL_TXQ_ENABLE: u32 = 2 << 2;
const MTL_TX_STORE_FORWARD: u32 = 1 << 1;
const MTL_RX_RQS_SHIFT: u32 = 20;
const MTL_RX_STORE_FORWARD: u32 = 1 << 5;

const DMA_SYSBUS_EAME: u32 = 1 << 11;
const DMA_SYSBUS_BLEN16: u32 = 1 << 3;
const DMA_SYSBUS_BLEN8: u32 = 1 << 2;
const DMA_SYSBUS_BLEN4: u32 = 1 << 1;
const DMA_CH_PBLX8: u32 = 1 << 16;
const DMA_TX_PBL_SHIFT: u32 = 16;
const DMA_TX_OSP: u32 = 1 << 4;
const DMA_TX_START: u32 = 1 << 0;
const DMA_RX_PBL_SHIFT: u32 = 16;
const DMA_RX_BUFFER_SIZE_SHIFT: u32 = 1;
const DMA_RX_START: u32 = 1 << 0;

const DESC_OWN: u32 = 1 << 31;
const DESC_RX_BUFFER1_VALID: u32 = 1 << 24;
const DESC_TX_FIRST: u32 = 1 << 29;
const DESC_TX_LAST: u32 = 1 << 28;
const DESC_PACKET_LENGTH_MASK: u32 = 0x7fff;
const DESC_RX_ERROR: u32 = 1 << 15;
const DESC_RX_LAST: u32 = 1 << 28;
const DESC_RX_FIRST: u32 = 1 << 29;

const RING_SIZE: usize = 4;
const PACKET_BUFFER_SIZE: usize = 1600;
const RAW_TEST_TIMEOUT_US: usize = 1_000_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct DmaDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

impl DmaDesc {
    const ZERO: Self = Self {
        des0: 0,
        des1: 0,
        des2: 0,
        des3: 0,
    };
}

#[repr(C, align(64))]
struct DescriptorRing([DmaDesc; RING_SIZE]);

#[repr(C, align(64))]
struct PacketBuffers([[u8; PACKET_BUFFER_SIZE]; RING_SIZE]);

static mut TX_RING: DescriptorRing = DescriptorRing([DmaDesc::ZERO; RING_SIZE]);
static mut RX_RING: DescriptorRing = DescriptorRing([DmaDesc::ZERO; RING_SIZE]);
static mut TX_BUFFERS: PacketBuffers = PacketBuffers([[0; PACKET_BUFFER_SIZE]; RING_SIZE]);
static mut RX_BUFFERS: PacketBuffers = PacketBuffers([[0; PACKET_BUFFER_SIZE]; RING_SIZE]);

const MDIO_PA_SHIFT: u32 = 21;
const MDIO_RDA_SHIFT: u32 = 16;
const MDIO_CR_SHIFT: u32 = 8;
const MDIO_GOC_SHIFT: u32 = 2;
const MDIO_GOC_WRITE: u32 = 1;
const MDIO_GOC_READ: u32 = 3;
const MDIO_BUSY: u32 = 1;

// U-Boot's JH7110 EQoS platform config selects the 250-300 MHz CSR range.
const JH7110_MDIO_CR: u32 = 5;
const MDIO_TIMEOUT_US: usize = 1_000_000;

const MII_BMCR: u8 = 0;
const MII_BMSR: u8 = 1;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;
const BMSR_LINK_STATUS: u16 = 1 << 2;
const BMSR_AUTONEG_COMPLETE: u16 = 1 << 5;

#[derive(Debug, Clone, Copy)]
pub enum MdioError {
    Timeout,
    InvalidAddress,
}

#[derive(Debug, Clone, Copy)]
pub enum DwMacError {
    Busy,
    BufferTooSmall,
    FrameTooLarge,
    InvalidDescriptor(u32),
    Timeout,
}

pub struct DwMacDevice {
    mac: Jh7110DwMac,
    tx_index: usize,
    rx_index: usize,
}

pub struct DwMacWrapper(pub MPSafeCell<DwMacDevice>);

impl DwMacWrapper {
    pub fn new() -> Self {
        Self(MPSafeCell::new(DwMacDevice::new()))
    }

    pub fn get_mac_address(&self) -> [u8; 6] {
        self.0.exclusive_access().mac()
    }
}

pub struct Jh7110DwMac {
    base: usize,
}

impl Jh7110DwMac {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.base + offset) as *const u32) }
    }

    #[inline]
    fn write_reg(&self, offset: usize, value: u32) {
        unsafe { write_volatile((self.base + offset) as *mut u32, value) }
    }

    #[inline]
    fn read_mmio(address: usize) -> u32 {
        unsafe { read_volatile(address as *const u32) }
    }

    #[inline]
    fn write_mmio(address: usize, value: u32) {
        unsafe { write_volatile(address as *mut u32, value) }
    }

    #[inline]
    fn dma_sync() {
        // JH7110's U74 does not advertise Zicbom. Keep DMA memory isolated and
        // ordered; raw tests below verify whether the interconnect is coherent.
        unsafe { core::arch::asm!("fence rw, rw", options(nostack, preserves_flags)) }
    }

    fn mac_address(&self) -> [u8; 6] {
        let low = self.read_reg(MAC_ADDRESS0_LOW);
        let high = self.read_reg(MAC_ADDRESS0_HIGH);
        [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ]
    }

    fn set_mac_address(&self, address: [u8; 6]) {
        let low = address[0] as u32
            | ((address[1] as u32) << 8)
            | ((address[2] as u32) << 16)
            | ((address[3] as u32) << 24);
        let high = address[4] as u32 | ((address[5] as u32) << 8);
        self.write_reg(MAC_ADDRESS0_LOW, low);
        self.write_reg(MAC_ADDRESS0_HIGH, high);
    }

    fn ensure_mac_address(&self) {
        let address = self.mac_address();
        let invalid = address.iter().all(|byte| *byte == 0)
            || address.iter().all(|byte| *byte == 0xff)
            || address[0] & 1 != 0;
        if invalid {
            self.set_mac_address([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
            println!("[dwmac] hardware MAC unavailable; using temporary local address");
        }
    }

    unsafe fn init_dma_rings(&self) {
        let tx_ring = core::ptr::addr_of_mut!(TX_RING.0);
        let rx_ring = core::ptr::addr_of_mut!(RX_RING.0);
        let rx_buffers = core::ptr::addr_of_mut!(RX_BUFFERS.0);

        for index in 0..RING_SIZE {
            (*tx_ring)[index] = DmaDesc::ZERO;
            let buffer_address = core::ptr::addr_of_mut!((*rx_buffers)[index]) as usize as u64;
            (*rx_ring)[index] = DmaDesc {
                des0: buffer_address as u32,
                des1: (buffer_address >> 32) as u32,
                des2: 0,
                des3: DESC_OWN | DESC_RX_BUFFER1_VALID,
            };
        }
        Self::dma_sync();

        let tx_address = tx_ring as usize as u64;
        let rx_address = rx_ring as usize as u64;
        self.write_reg(DMA_CH0_TXDESC_LIST_HI, (tx_address >> 32) as u32);
        self.write_reg(DMA_CH0_TXDESC_LIST, tx_address as u32);
        self.write_reg(DMA_CH0_RXDESC_LIST_HI, (rx_address >> 32) as u32);
        self.write_reg(DMA_CH0_RXDESC_LIST, rx_address as u32);
        self.write_reg(DMA_CH0_TX_RING_LENGTH, (RING_SIZE - 1) as u32);
        self.write_reg(DMA_CH0_RX_RING_LENGTH, (RING_SIZE - 1) as u32);
        self.write_reg(DMA_CH0_TXDESC_TAIL, tx_address as u32);
        let rx_tail = rx_address + ((RING_SIZE - 1) * core::mem::size_of::<DmaDesc>()) as u64;
        self.write_reg(DMA_CH0_RXDESC_TAIL, rx_tail as u32);
    }

    unsafe fn init_raw_dma(&self) {
        // VF2 DTS fixes both FIFO depths at 2048 bytes: (2048 / 256) - 1 = 7.
        self.write_reg(
            MTL_TXQ0_OPERATION_MODE,
            (7 << MTL_TX_TQS_SHIFT) | MTL_TXQ_ENABLE | MTL_TX_STORE_FORWARD,
        );
        self.write_reg(MTL_TXQ0_QUANTUM_WEIGHT, 0x10);
        self.write_reg(
            MTL_RXQ0_OPERATION_MODE,
            (7 << MTL_RX_RQS_SHIFT) | MTL_RX_STORE_FORWARD,
        );
        self.write_reg(MAC_RXQ_CTRL0, MAC_RXQ0_ENABLE_DCB);
        self.write_reg(
            MAC_PACKET_FILTER,
            self.read_reg(MAC_PACKET_FILTER) | MAC_PACKET_FILTER_PROMISCUOUS,
        );

        self.write_reg(DMA_CH0_INTERRUPT_ENABLE, 0);
        self.write_reg(DMA_CH0_CONTROL, DMA_CH_PBLX8);
        self.write_reg(DMA_CH0_TX_CONTROL, (8 << DMA_TX_PBL_SHIFT) | DMA_TX_OSP);
        self.write_reg(
            DMA_CH0_RX_CONTROL,
            (8 << DMA_RX_PBL_SHIFT) | ((PACKET_BUFFER_SIZE as u32) << DMA_RX_BUFFER_SIZE_SHIFT),
        );
        self.write_reg(
            DMA_SYSBUS_MODE,
            (2 << 16) | DMA_SYSBUS_EAME | DMA_SYSBUS_BLEN16 | DMA_SYSBUS_BLEN8 | DMA_SYSBUS_BLEN4,
        );
        self.init_dma_rings();

        self.write_reg(
            DMA_CH0_TX_CONTROL,
            self.read_reg(DMA_CH0_TX_CONTROL) | DMA_TX_START,
        );
        self.write_reg(
            DMA_CH0_RX_CONTROL,
            self.read_reg(DMA_CH0_RX_CONTROL) | DMA_RX_START,
        );
        self.write_reg(
            MAC_CONFIGURATION,
            self.read_reg(MAC_CONFIGURATION)
                | MAC_CONFIG_CST
                | MAC_CONFIG_ACS
                | MAC_CONFIG_DM
                | MAC_CONFIG_TE
                | MAC_CONFIG_RE,
        );
        self.ensure_mac_address();
        println!(
            "[dwmac-dma] rings: tx={:#x} rx={:#x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            tx_ring_address(),
            rx_ring_address(),
            self.mac_address()[0],
            self.mac_address()[1],
            self.mac_address()[2],
            self.mac_address()[3],
            self.mac_address()[4],
            self.mac_address()[5],
        );
    }

    unsafe fn raw_transmit_test(&self) {
        self.write_reg(
            MAC_CONFIGURATION,
            self.read_reg(MAC_CONFIGURATION) | MAC_CONFIG_LOOPBACK,
        );
        let source = self.mac_address();
        let buffer = &mut (*core::ptr::addr_of_mut!(TX_BUFFERS.0))[0];
        buffer.fill(0);
        buffer[..6].fill(0xff);
        buffer[6..12].copy_from_slice(&source);
        buffer[12..14].copy_from_slice(&0x88b5u16.to_be_bytes());
        let marker = b"SHELLCORE-DWMAC-TEST";
        buffer[14..14 + marker.len()].copy_from_slice(marker);
        let length = 64usize;
        let buffer_address = buffer.as_mut_ptr() as usize as u64;
        let tx_ring = core::ptr::addr_of_mut!(TX_RING.0);
        (*tx_ring)[0] = DmaDesc {
            des0: buffer_address as u32,
            des1: (buffer_address >> 32) as u32,
            des2: length as u32,
            des3: DESC_OWN | DESC_TX_FIRST | DESC_TX_LAST | length as u32,
        };
        Self::dma_sync();
        let next = tx_ring_address() + core::mem::size_of::<DmaDesc>();
        self.write_reg(DMA_CH0_TXDESC_TAIL, next as u32);

        let deadline = crate::arch::timer::get_time_us().saturating_add(RAW_TEST_TIMEOUT_US);
        loop {
            Self::dma_sync();
            let status = read_volatile(core::ptr::addr_of!((*tx_ring)[0].des3));
            if status & DESC_OWN == 0 {
                println!("[dwmac-dma] raw TX complete: descriptor={:#010x}", status);
                break;
            }
            if crate::arch::timer::get_time_us() >= deadline {
                println!(
                    "[dwmac-dma] raw TX timeout: descriptor={:#010x} dma_status={:#010x} current={:#010x}",
                    status,
                    self.read_reg(DMA_CH0_STATUS),
                    self.read_reg(DMA_CH0_CURRENT_TX_DESC),
                );
                self.write_reg(
                    MAC_CONFIGURATION,
                    self.read_reg(MAC_CONFIGURATION) & !MAC_CONFIG_LOOPBACK,
                );
                break;
            }
        }
    }

    unsafe fn raw_receive_test(&self) {
        let rx_ring = core::ptr::addr_of_mut!(RX_RING.0);
        let deadline = crate::arch::timer::get_time_us().saturating_add(RAW_TEST_TIMEOUT_US);
        loop {
            Self::dma_sync();
            let status = read_volatile(core::ptr::addr_of!((*rx_ring)[0].des3));
            if status & DESC_OWN == 0 {
                let length = (status & DESC_PACKET_LENGTH_MASK) as usize;
                let buffer = &(*core::ptr::addr_of!(RX_BUFFERS.0))[0];
                println!(
                    "[dwmac-dma] raw RX: len={} descriptor={:#010x} head={:02x?}",
                    length,
                    status,
                    &buffer[..core::cmp::min(length, 16)],
                );
                self.write_reg(
                    MAC_CONFIGURATION,
                    self.read_reg(MAC_CONFIGURATION) & !MAC_CONFIG_LOOPBACK,
                );
                break;
            }
            if crate::arch::timer::get_time_us() >= deadline {
                println!(
                    "[dwmac-dma] raw RX timeout: descriptor={:#010x} dma_status={:#010x} current={:#010x}",
                    status,
                    self.read_reg(DMA_CH0_STATUS),
                    self.read_reg(DMA_CH0_CURRENT_RX_DESC),
                );
                self.write_reg(
                    MAC_CONFIGURATION,
                    self.read_reg(MAC_CONFIGURATION) & !MAC_CONFIG_LOOPBACK,
                );
                break;
            }
        }
    }

    unsafe fn reset_runtime_rings(&self) {
        self.write_reg(
            DMA_CH0_TX_CONTROL,
            self.read_reg(DMA_CH0_TX_CONTROL) & !DMA_TX_START,
        );
        self.write_reg(
            DMA_CH0_RX_CONTROL,
            self.read_reg(DMA_CH0_RX_CONTROL) & !DMA_RX_START,
        );
        self.init_dma_rings();
        self.write_reg(DMA_CH0_STATUS, u32::MAX);
        self.write_reg(
            DMA_CH0_TX_CONTROL,
            self.read_reg(DMA_CH0_TX_CONTROL) | DMA_TX_START,
        );
        self.write_reg(
            DMA_CH0_RX_CONTROL,
            self.read_reg(DMA_CH0_RX_CONTROL) | DMA_RX_START,
        );
    }

    fn enable_platform(&self) -> Result<(), MdioError> {
        let ahb_reg = AON_CRG_BASE + AONCLK_GMAC0_AHB * 4;
        let axi_reg = AON_CRG_BASE + AONCLK_GMAC0_AXI * 4;
        let tx_reg = AON_CRG_BASE + AONCLK_GMAC0_TX * 4;
        let gtxclk_reg = SYS_CRG_BASE + SYSCLK_GMAC0_GTXCLK * 4;
        let ptp_reg = SYS_CRG_BASE + SYSCLK_GMAC0_PTP * 4;
        let gtxc_reg = SYS_CRG_BASE + SYSCLK_GMAC0_GTXC * 4;
        let reset_assert_reg = AON_CRG_BASE + AON_RESET_ASSERT;
        let reset_status_reg = AON_CRG_BASE + AON_RESET_STATUS;
        let reset_mask = AONRST_GMAC0_AXI | AONRST_GMAC0_AHB;

        Self::write_mmio(ahb_reg, Self::read_mmio(ahb_reg) | CLOCK_ENABLE);
        Self::write_mmio(axi_reg, Self::read_mmio(axi_reg) | CLOCK_ENABLE);
        Self::write_mmio(tx_reg, Self::read_mmio(tx_reg) | CLOCK_ENABLE);
        Self::write_mmio(gtxclk_reg, Self::read_mmio(gtxclk_reg) | CLOCK_ENABLE);
        Self::write_mmio(ptp_reg, Self::read_mmio(ptp_reg) | CLOCK_ENABLE);
        Self::write_mmio(gtxc_reg, Self::read_mmio(gtxc_reg) | CLOCK_ENABLE);

        // Linux deasserts by clearing the corresponding assert bits after clocks run.
        Self::write_mmio(
            reset_assert_reg,
            Self::read_mmio(reset_assert_reg) & !reset_mask,
        );

        let deadline = crate::arch::timer::get_time_us().saturating_add(1_000);
        while Self::read_mmio(reset_status_reg) & reset_mask != reset_mask {
            if crate::arch::timer::get_time_us() >= deadline {
                println!(
                    "[dwmac] reset timeout: assert={:#010x} status={:#010x}",
                    Self::read_mmio(reset_assert_reg),
                    Self::read_mmio(reset_status_reg),
                );
                return Err(MdioError::Timeout);
            }
            core::hint::spin_loop();
        }

        let mode_reg = AON_SYSCON_BASE + GMAC0_MODE_OFFSET;
        let mode_mask = GMAC_PHY_INTERFACE_MASK << GMAC0_MODE_SHIFT;
        let mode = (Self::read_mmio(mode_reg) & !mode_mask)
            | (GMAC_PHY_INTERFACE_RGMII << GMAC0_MODE_SHIFT);
        Self::write_mmio(mode_reg, mode);

        println!(
            "[dwmac] platform: ahb={:#010x} axi={:#010x} reset={:#010x}/{:#010x} syscon={:#010x}",
            Self::read_mmio(ahb_reg),
            Self::read_mmio(axi_reg),
            Self::read_mmio(reset_assert_reg),
            Self::read_mmio(reset_status_reg),
            Self::read_mmio(mode_reg),
        );
        println!(
            "[dwmac] clocks: tx={:#010x} gtxclk={:#010x} ptp={:#010x} gtxc={:#010x}",
            Self::read_mmio(tx_reg),
            Self::read_mmio(gtxclk_reg),
            Self::read_mmio(ptp_reg),
            Self::read_mmio(gtxc_reg),
        );
        Ok(())
    }

    fn wait_mdio_idle(&self) -> Result<(), MdioError> {
        let deadline = crate::arch::timer::get_time_us().saturating_add(MDIO_TIMEOUT_US);
        while self.read_reg(MAC_MDIO_ADDRESS) & MDIO_BUSY != 0 {
            if crate::arch::timer::get_time_us() >= deadline {
                return Err(MdioError::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn mdio_command(phy: u8, reg: u8, operation: u32) -> Result<u32, MdioError> {
        if phy >= 32 || reg >= 32 {
            return Err(MdioError::InvalidAddress);
        }
        Ok(((phy as u32) << MDIO_PA_SHIFT)
            | ((reg as u32) << MDIO_RDA_SHIFT)
            | (JH7110_MDIO_CR << MDIO_CR_SHIFT)
            | (operation << MDIO_GOC_SHIFT)
            | MDIO_BUSY)
    }

    pub fn mdio_read(&self, phy: u8, reg: u8) -> Result<u16, MdioError> {
        self.wait_mdio_idle()?;
        let skip_address_packet = self.read_reg(MAC_MDIO_ADDRESS) & (1 << 4);
        self.write_reg(
            MAC_MDIO_ADDRESS,
            skip_address_packet | Self::mdio_command(phy, reg, MDIO_GOC_READ)?,
        );
        self.wait_mdio_idle()?;
        Ok((self.read_reg(MAC_MDIO_DATA) & 0xffff) as u16)
    }

    #[allow(dead_code)]
    pub fn mdio_write(&self, phy: u8, reg: u8, value: u16) -> Result<(), MdioError> {
        self.wait_mdio_idle()?;
        let skip_address_packet = self.read_reg(MAC_MDIO_ADDRESS) & (1 << 4);
        self.write_reg(MAC_MDIO_DATA, value as u32);
        self.write_reg(
            MAC_MDIO_ADDRESS,
            skip_address_packet | Self::mdio_command(phy, reg, MDIO_GOC_WRITE)?,
        );
        self.wait_mdio_idle()
    }

    fn print_registers(&self) {
        println!(
            "[dwmac] version={:#010x} hw_feature0={:#010x} hw_feature1={:#010x}",
            self.read_reg(MAC_VERSION),
            self.read_reg(MAC_HW_FEATURE0),
            self.read_reg(MAC_HW_FEATURE1),
        );
        println!(
            "[dwmac] hw_feature2={:#010x} hw_feature3={:#010x}",
            self.read_reg(MAC_HW_FEATURE2),
            self.read_reg(MAC_HW_FEATURE3),
        );
    }

    fn scan_phys(&self) {
        let mut found = 0usize;
        for phy in 0..32u8 {
            let id1 = match self.mdio_read(phy, MII_PHYSID1) {
                Ok(value) => value,
                Err(error) => {
                    println!("[dwmac] MDIO scan failed at PHY {}: {:?}", phy, error);
                    return;
                }
            };
            let id2 = match self.mdio_read(phy, MII_PHYSID2) {
                Ok(value) => value,
                Err(error) => {
                    println!("[dwmac] MDIO scan failed at PHY {}: {:?}", phy, error);
                    return;
                }
            };
            if (id1 == 0 && id2 == 0) || (id1 == 0xffff && id2 == 0xffff) {
                continue;
            }

            // BMSR link is latch-low, so use the second read for current state.
            let _ = self.mdio_read(phy, MII_BMSR);
            let bmsr = self.mdio_read(phy, MII_BMSR).unwrap_or(0);
            let bmcr = self.mdio_read(phy, MII_BMCR).unwrap_or(0);
            let phy_id = ((id1 as u32) << 16) | id2 as u32;
            println!(
                "[dwmac] PHY addr={} id={:#010x} bmcr={:#06x} bmsr={:#06x} link={} autoneg_complete={}",
                phy,
                phy_id,
                bmcr,
                bmsr,
                bmsr & BMSR_LINK_STATUS != 0,
                bmsr & BMSR_AUTONEG_COMPLETE != 0,
            );
            found += 1;
        }
        if found == 0 {
            println!("[dwmac] no Clause 22 PHY found");
        }
    }
}

impl DwMacDevice {
    pub fn new() -> Self {
        let mac = Jh7110DwMac::new(DWMAC0_BASE);
        if let Err(error) = mac.enable_platform() {
            panic!("[dwmac] platform initialization failed: {:?}", error);
        }
        mac.print_registers();
        mac.scan_phys();
        unsafe {
            mac.init_raw_dma();
            mac.raw_transmit_test();
            mac.raw_receive_test();
            mac.reset_runtime_rings();
        }
        println!("[dwmac] polling device ready");
        Self {
            mac,
            tx_index: 0,
            rx_index: 0,
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac.mac_address()
    }

    pub fn can_send(&self) -> bool {
        unsafe {
            Jh7110DwMac::dma_sync();
            let ring = core::ptr::addr_of!(TX_RING.0);
            read_volatile(core::ptr::addr_of!((*ring)[self.tx_index].des3)) & DESC_OWN == 0
        }
    }

    pub fn send(&mut self, frame: &[u8]) -> Result<(), DwMacError> {
        if frame.len() > PACKET_BUFFER_SIZE {
            return Err(DwMacError::FrameTooLarge);
        }
        if !self.can_send() {
            return Err(DwMacError::Busy);
        }

        unsafe {
            let index = self.tx_index;
            let buffers = core::ptr::addr_of_mut!(TX_BUFFERS.0);
            let buffer = &mut (*buffers)[index];
            buffer[..frame.len()].copy_from_slice(frame);
            let address = buffer.as_mut_ptr() as usize as u64;
            let ring = core::ptr::addr_of_mut!(TX_RING.0);
            (*ring)[index] = DmaDesc {
                des0: address as u32,
                des1: (address >> 32) as u32,
                des2: frame.len() as u32,
                des3: DESC_OWN | DESC_TX_FIRST | DESC_TX_LAST | frame.len() as u32,
            };
            Jh7110DwMac::dma_sync();
            self.tx_index = (index + 1) % RING_SIZE;
            let tail = tx_ring_address() + self.tx_index * core::mem::size_of::<DmaDesc>();
            self.mac.write_reg(DMA_CH0_TXDESC_TAIL, tail as u32);

            let deadline = crate::arch::timer::get_time_us().saturating_add(RAW_TEST_TIMEOUT_US);
            loop {
                Jh7110DwMac::dma_sync();
                let status = read_volatile(core::ptr::addr_of!((*ring)[index].des3));
                if status & DESC_OWN == 0 {
                    return Ok(());
                }
                if crate::arch::timer::get_time_us() >= deadline {
                    return Err(DwMacError::Timeout);
                }
                core::hint::spin_loop();
            }
        }
    }

    pub fn can_recv(&self) -> bool {
        unsafe {
            Jh7110DwMac::dma_sync();
            let ring = core::ptr::addr_of!(RX_RING.0);
            read_volatile(core::ptr::addr_of!((*ring)[self.rx_index].des3)) & DESC_OWN == 0
        }
    }

    pub fn recv(&mut self, output: &mut [u8]) -> Result<usize, DwMacError> {
        if !self.can_recv() {
            return Err(DwMacError::Busy);
        }
        unsafe {
            let index = self.rx_index;
            let ring = core::ptr::addr_of_mut!(RX_RING.0);
            let status = read_volatile(core::ptr::addr_of!((*ring)[index].des3));
            if status & (DESC_RX_ERROR | DESC_RX_FIRST | DESC_RX_LAST)
                != (DESC_RX_FIRST | DESC_RX_LAST)
            {
                self.recycle_rx(index);
                return Err(DwMacError::InvalidDescriptor(status));
            }
            let length = (status & DESC_PACKET_LENGTH_MASK) as usize;
            if length > output.len() || length > PACKET_BUFFER_SIZE {
                self.recycle_rx(index);
                return Err(DwMacError::BufferTooSmall);
            }
            let buffers = core::ptr::addr_of!(RX_BUFFERS.0);
            let buffer = &(*buffers)[index];
            output[..length].copy_from_slice(&buffer[..length]);
            self.recycle_rx(index);
            Ok(length)
        }
    }

    unsafe fn recycle_rx(&mut self, index: usize) {
        let ring = core::ptr::addr_of_mut!(RX_RING.0);
        let buffers = core::ptr::addr_of_mut!(RX_BUFFERS.0);
        let address = core::ptr::addr_of_mut!((*buffers)[index]) as usize as u64;
        (*ring)[index] = DmaDesc {
            des0: address as u32,
            des1: (address >> 32) as u32,
            des2: 0,
            des3: DESC_OWN | DESC_RX_BUFFER1_VALID,
        };
        Jh7110DwMac::dma_sync();
        let descriptor = rx_ring_address() + index * core::mem::size_of::<DmaDesc>();
        self.mac.write_reg(DMA_CH0_RXDESC_TAIL, descriptor as u32);
        self.rx_index = (index + 1) % RING_SIZE;
    }
}

fn tx_ring_address() -> usize {
    unsafe { core::ptr::addr_of!(TX_RING.0) as usize }
}

fn rx_ring_address() -> usize {
    unsafe { core::ptr::addr_of!(RX_RING.0) as usize }
}
