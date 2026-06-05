/* Copyright (c) 2015 The Robigalia Project Developers
 * Licensed under the Apache License, Version 2.0
 * <LICENSE-APACHE or
 * http://www.apache.org/licenses/LICENSE-2.0> or the MIT
 * license <LICENSE-MIT or http://opensource.org/licenses/MIT>,
 * at your option. All files in the project carrying such
 * notice may not be copied, modified, or distributed except
 * according to those terms.
 */

/* 
 * Modified by 贝壳OS in 2026 for _2026os.
 * Changes:
 * 1. 重命名模块,将其作为一个mod放入项目中,并引入项目相关模块.
 * 2. 为适配LA64修改部分代码,删除了portio相关代码.
 * 3. 完善部分逻辑,增加扫描转换函数.
 */

/* 
* 参考了https://godones.github.io/rCoreloongArch/pci.html
*/

use crate::arch::config::*;
use crate::mm::PhysAddr;
const BASE_ADDR: usize = PCI_CONFIG_SPACE_BASE;
use lazy_static::lazy_static;
use crate::sync::MPSafeCell;
use virtio_drivers_la::transport::pci::{PciTransport, bus::ConfigurationAccess};
use virtio_drivers_la::transport::pci::bus::{DeviceFunction, PciRoot};

lazy_static!(
    // 维护当前已分配的MMIO地址
    pub static ref CURRENT_MMIO_END: MPSafeCell<usize> = MPSafeCell::new(PCI_MMIO_BASE);
);

// 只分配，暂时不考虑回收问题
pub fn mmio_alloc(size: usize) -> usize {
    let mut next = CURRENT_MMIO_END.exclusive_access();
    let current = next.clone();
    // 对齐
    let ppn = PhysAddr(current).std_ceil();
    let start = ppn.0 * PAGE_SIZE;
    *next = start + size;

    start
}


// 参考了loongarchrcore的实现
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CSpaceAccessMethod {
    MemoryMapped,
}

// All IO-bus ops are 32-bit, we mask and shift to get the values we want.
// la64下，pci硬件被映射到某段物理地址，直接对指定内存地址进行读写
impl CSpaceAccessMethod {
    pub unsafe fn read8(self, loc: Location, offset: u16) -> u8 {
        let val = self.read32(loc, offset & 0b11111100);
        ((val >> ((offset as usize & 0b11) << 3)) & 0xFF) as u8
    }
    /// Returns a value in native endian.
    pub unsafe fn read16(self, loc: Location, offset: u16) -> u16 {
        let val = self.read32(loc, offset & 0b11111100);
        ((val >> ((offset as usize & 0b10) << 3)) & 0xFFFF) as u16
    }
    /// Returns a value in native endian.
    pub unsafe fn read32(self, loc: Location, offset: u16) -> u32 {
        debug_assert!(
            (offset & 0b11) == 0,
            "misaligned PCI configuration dword u32 read"
        );
        let addr = loc.encode() + (offset as usize);
        match self {
            CSpaceAccessMethod::MemoryMapped => {
                // 改为窗口映射后的地址
                let addr = (addr | UNCHACHED_KERNEL_BASE) as *const u32;
                addr.read_volatile()
                }
        }
    }
    pub unsafe fn write8(self, loc: Location, offset: u16, val: u8) {
        let old = self.read32(loc, offset);
        let dest = offset as usize & 0b11 << 3;
        let mask = (0xFF << dest) as u32;
        self.write32(loc, offset, ((val as u32) << dest | (old & !mask)).to_le());
    }
    /// Converts val to little endian before writing.
    pub unsafe fn write16(self, loc: Location, offset: u16, val: u16) {
        let old = self.read32(loc, offset);
        let dest = offset as usize & 0b10 << 3;
        let mask = (0xFFFF << dest) as u32;
        self.write32(loc, offset, ((val as u32) << dest | (old & !mask)).to_le());
    }
    pub unsafe fn write32(self, loc: Location, offset: u16, val: u32) {
        debug_assert!(
            (offset & 0b11) == 0,
            "misaligned PCI configuration dword u32 read"
        );
        let addr = loc.encode() + (offset as usize);
        match self {
            CSpaceAccessMethod::MemoryMapped => {
                let addr = (addr | UNCHACHED_KERNEL_BASE) as *mut u32;
                addr.write_volatile(val);
            }
        }
    }
}

/// Physical location of a device on the bus
/// 参考loongarchrcore的实现
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Location {
    base_addr: usize, //base address of the device
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}
impl Location {
    #[inline(always)]
    fn encode(self) -> usize {
        self.base_addr
            | ((self.bus as usize) << 16)
            | ((self.device as usize) << 11)
            | ((self.function as usize) << 8)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Identifier {
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision_id: u8,
    pub class: u8,
    pub subclass: u8,
}

/// A device on the PCI bus.
///
/// Although accessing configuration space may be expensive, it is not cached.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct PCIDevice {
    pub loc: Location,
    pub id: Identifier,
    pub bars: [Option<BAR>; 6],
    pub cspace_access_method: CSpaceAccessMethod,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Prefetchable {
    Yes,
    No
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Type {
    Bits32,
    Bits64
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BAR {
    Memory(u64, u32, Prefetchable, Type),
    IO(u32),
}

impl BAR {
    // 解析和探测
    pub unsafe fn decode(loc: Location, am: CSpaceAccessMethod, idx: u16) -> (Option<BAR>, usize) {
        let raw = am.read32(loc, 16 + (idx << 2));
        if raw & 1 == 0 {
            let mut bits64 = false;
            let base: u64 =
            match (raw & 0b110) >> 1 {
                // 原作者似乎写反了,10才是64位，已修改
                0b10 => { bits64 = true; ((raw & !0xF) as u64) | ((am.read32(loc, 16 + ((idx + 1) << 2)) as u64) << 32) }
                0b00 => (raw & !0xF) as u64,
                _ => { debug_assert!(false, "bad type in memory BAR"); return (None, idx as usize + 1) },
            };
            am.write32(loc, 16 + (idx << 2), !0);
            let len32 = am.read32(loc, 16 + (idx << 2));
            let len = !(len32 & !0xF) as u64 + 1;
            am.write32(loc, 16 + (idx << 2), raw);
            (if len > 1 { Some(BAR::Memory(base, len as u32, if raw & 0b1000 == 0 { Prefetchable::No } else { Prefetchable::Yes },
                        if bits64 { Type::Bits64 } else { Type::Bits32 })) } else { None },
             if bits64 { idx + 2 } else { idx + 1 } as usize)
        } else {
            (Some(BAR::IO(raw & !0x3)), idx as usize + 1)
        }
    }
}

#[derive(Debug)]
pub struct BusScan {
    loc: Location,
    am: CSpaceAccessMethod,
}

impl BusScan {
    fn done(&self) -> bool {
        if self.loc.bus == 255 && self.loc.device == 31 && self.loc.function == 7 {
            true
        } else {
            false
        }
    }
    fn increment(&mut self) {
        // TODO: Decide whether this is actually nicer than taking a u16 and incrementing until it
        // wraps.
        if self.loc.function < 7 {
            self.loc.function += 1;
            return
        } else {
            self.loc.function = 0;
            if self.loc.device < 31 {
                self.loc.device += 1;
                return;
            } else {
                self.loc.device = 0;
                if self.loc.bus == 255 {
                    self.loc.device = 31;
                    self.loc.device = 7;
                } else {
                    self.loc.bus += 1;
                    return;
                }
            }
        }
    }
}

impl<'a> ::core::iter::Iterator for BusScan {
    type Item = PCIDevice;
    #[inline]
    fn next(&mut self) -> Option<PCIDevice> {
        // FIXME: very naive atm, could be smarter and waste much less time by only scanning used
        // busses.
        let mut ret = None;
        loop {
            if self.done() {
                return ret;
            }
            ret = unsafe { probe_function(self.loc, self.am) };
            self.increment();
            if ret.is_some() {
                return ret;
            }
        }
    }
}

pub unsafe fn probe_function(loc: Location, am: CSpaceAccessMethod) -> Option<PCIDevice> {
    // FIXME: it'd be more efficient to use read32 and decode separately.
    let vid = am.read16(loc, 0);
    if vid == 0xFFFF {
        return None;
    }
    let did = am.read16( loc, 2);
    let rid = am.read8(loc, 8);
    let subclass = am.read8( loc, 10);
    let class = am.read8( loc, 11);
    let id = Identifier {
        vendor_id: vid,
        device_id: did,
        revision_id: rid,
        class: class,
        subclass: subclass,
    };
    let hdrty = am.read8( loc, 14);
    let mut bars = [None, None, None, None, None, None];
    let max = match hdrty {
        0 => 6,
        1 => 2,
        _ => 0,
    };
    let mut i = 0;
    while i < max {
        let (bar, next) = BAR::decode( loc, am, i as u16);
        bars[i] = bar;
        i = next;
    }
    Some(PCIDevice {
        loc: loc,
        id: id,
        bars: bars,
        cspace_access_method: am,
    })
}

pub fn scan_bus(am: CSpaceAccessMethod) -> BusScan {
    BusScan { loc: Location { base_addr: BASE_ADDR, bus: 0, device: 0, function: 0 }, am: am }
}


use crate::drivers::{DeviceType , block::VirtioHal};
use alloc::boxed::Box;


pub fn scan_and_init_pci_device_to_trans(dev_type: DeviceType) -> Option<PciTransport> {
    //! bug: root会被泄露到堆中，可能会有问题
    //! 如果不使用这样的方式，此函数会有生命周期问题，不过目前的实现能跑
    let am = CSpaceAccessMethod::MemoryMapped;
    // 调用库中的扫描函数扫描第一个块设备
    for dev in scan_bus(am) {
        // 调试用，输出信息
        info!("found a device: bus={:#x} dev={:#x} func={:#x}", 
            dev.loc.bus,
            dev.loc.device,
            dev.loc.function
        );
        match dev_type {
            DeviceType::VirtIOBlock => {
                if !(dev.id.vendor_id == 0x1AF4 && dev.id.device_id == 0x1001) {
                    // 0x1AF4是virtio的vendor id，0x1001是virtio块设备的device id
                    continue;
                }
            },
            DeviceType::VirtIONet => {
                if !(dev.id.vendor_id == 0x1AF4 && dev.id.device_id == 0x1000) {
                    // 0x1AF4是virtio的vendor id，0x1000是virtio网卡设备的device id
                    continue;
                }
            },
            // _ => continue,
        }
        info!("found a target device, info: vendor_id={:#x}, device_id={:#x}, class={:#x}, subclass={:#x}",
            dev.id.vendor_id, dev.id.device_id, dev.id.class, dev.id.subclass);
        // 初始化bar
        for (idx, obar) in dev.bars.iter().enumerate() {
            if let Some(bar) = obar {
                match bar {
                    BAR::Memory(_base, len, prefetchable, ty) => {
                        debug!("BAR{}: type Memory at {:#x}, length {:#x}, {:?}, {:?}",
                            idx, _base, len, prefetchable, ty
                        );
                        // 分配MMIO地址
                        let base_addr = mmio_alloc(*len as usize);
                        // 写入 BAR
                        if ty == &Type::Bits64 {
                            // 64位分两部分写入
                            unsafe{
                                CSpaceAccessMethod::MemoryMapped.write32(
                                    dev.loc, 16 + (idx << 2) as u16,
                                    (base_addr & 0xFFFF_FFFF) as u32
                                );
                                CSpaceAccessMethod::MemoryMapped.write32(
                                    dev.loc, 16 + ((idx + 1) << 2) as u16,
                                    (base_addr >> 32) as u32
                                );
                            }
                        } else {
                            unsafe{
                                CSpaceAccessMethod::MemoryMapped.write32(
                                    dev.loc, 16 + (idx << 2) as u16,
                                    base_addr as u32
                                );
                            }
                        }
                    }
                    BAR::IO(port) => {
                        debug!("BAR{}: type IO at {:#x}", idx, port);
                    }
                }
            }
        }
        // 启用设备
        unsafe {
            // 设置command寄存器：开启内存访问，开始相应dma请求
            let old = am.read16(dev.loc, 0x04);
            let new = old | 0x6;
            am.write16(dev.loc, 0x04, new);
        }
        let root = PciRoot::new(CSpaceAccessMethod::MemoryMapped);
        let r_oot = Box::new(root);
        // 注：将生命周期暴力改为static（会泄露内存），不过暂时不会有问题，因为不会反复调用
        let ref_root  = Box::leak(r_oot);
        info!("creating transport for device: bus={:#x} dev={:#x} func={:#x}", 
            dev.loc.bus,
            dev.loc.device,
            dev.loc.function
        );
        return Some(
            PciTransport::new::<VirtioHal, CSpaceAccessMethod>(
                ref_root,
                loc_to_func(dev.loc)
            ).unwrap()
        );
    }
    warn!("no target device found");
    None
}

// 为CSAM实现CA接口供库使用
impl ConfigurationAccess for CSpaceAccessMethod{
    fn read_word(&self, device_function: DeviceFunction, register_offset: u8) -> u32 {
        unsafe{
            self.read32(Location { 
                    base_addr: BASE_ADDR, 
                    bus: device_function.bus, 
                    device: device_function.device, 
                    function: device_function.function 
                }, 
                register_offset as u16
            )
        }
    }
    fn write_word(&mut self, device_function: DeviceFunction, register_offset: u8, data: u32) {
                unsafe{
            self.write32(Location { 
                    base_addr: BASE_ADDR, 
                    bus: device_function.bus, 
                    device: device_function.device, 
                    function: device_function.function 
                }, 
                register_offset as u16,
                data
            )
        }
    }
    unsafe fn unsafe_clone(&self) -> Self {
        *self
    }
}

pub fn loc_to_func(loc: Location) -> DeviceFunction {
    DeviceFunction {
        bus: loc.bus,
        device: loc.device,
        function: loc.function,
    }
}


