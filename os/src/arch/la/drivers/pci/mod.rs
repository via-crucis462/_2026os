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
 * 1. 重命名模块,将其作为一个mod放入项目中.
 * 2. 为适配LA64修改部分代码,删除了portio相关代码.
 */

 /* 
  * 参考了https://godones.github.io/rCoreloongArch/pci.html
  */

const CONFIG_ADDRESS: u16 = 0x0CF8;
const CONFIG_DATA: u16 = 0x0CFC;

use crate::arch::config::*;
const BASE_ADDR: usize = PCI_CONFIG_SPACE_BASE;

use virtio_drivers_la::transport::pci::{PciTransport, bus::ConfigurationAccess};
use virtio_drivers_la::transport::pci::bus::{DeviceFunction, PciRoot};


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
                let addr = addr as *const u32;
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
                let addr = addr as *mut u32;
                addr.write_volatile(val)
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

pub enum PCIScanError {

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
    pub unsafe fn decode(loc: Location, am: CSpaceAccessMethod, idx: u16) -> (Option<BAR>, usize) {
        let raw = am.read32(loc, 16 + (idx << 2));
        if raw & 1 == 0 {
            let mut bits64 = false;
            let base: u64 =
            match (raw & 0b110) >> 1 {
                0 => { bits64 = true; ((raw & !0xF) as u64) | ((am.read32(loc, 16 + ((idx + 1) << 2)) as u64) << 32) }
                2 => (raw & !0xF) as u64,
                _ => { debug_assert!(false, "bad type in memory BAR"); return (None, idx as usize + 1) },
            };
            am.write32(loc, 16 + (idx << 2), !0);
            let len = !am.read32(loc, 16 + (idx << 2/*原作者写的12，似乎不太对*/)) + 1;
            am.write32(loc, 16 + (idx << 2), raw);
            (Some(BAR::Memory(base, len, if raw & 0b1000 == 0 { Prefetchable::No } else { Prefetchable::Yes },
                        if bits64 { Type::Bits64 } else { Type::Bits32 })),
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

// 此处仍需解决生命周期问题
use crate::arch::la::drivers::block::VirtioHal;
use alloc::boxed::Box;

pub fn scan_and_init_pci_device() -> Option<PciTransport> {
    let am = CSpaceAccessMethod::MemoryMapped;
    for dev in scan_bus(am) {
        //直接取第一个
        let root = PciRoot::new(CSpaceAccessMethod::MemoryMapped);
        let r_oot = Box::new(root);
        // 注：将生命周期暴力改为static，不过暂时不会有问题，因为不会反复调用
        let ref_root  = Box::leak(r_oot);
        return Some(
            PciTransport::new::<VirtioHal, CSpaceAccessMethod>(
                ref_root,
                loc_to_func(dev.loc)
            ).unwrap()
        );
    }
    None
}

// 为CSAM实现CA接口供使用
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


