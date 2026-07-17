use super::virtio_blk::VirtioHal; 
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;
use virtio_drivers::{DeviceType, VirtIONet, VirtIOHeader};

// 网卡驱动的外包装
pub struct VirtIONetWrapper(pub MPSafeCell<VirtIONet<'static, VirtioHal>>);

impl VirtIONetWrapper {
    /// 动态扫描 MMIO 区域，找到并初始化 VirtIO 网卡
    pub fn new() -> Self {
        let mut net_addr: usize = 0;
        
        // 遍历 QEMU 预留给 virtio 设备的 8 个 MMIO 槽位
        for i in 1..=8 {
            let addr = 0x10000000 + 0x1000 * i;
            let header = unsafe { &mut *(addr as *mut VirtIOHeader) };
            
            // 验证设备是否有效，并检查它是不是网卡 (DeviceType::Network)
            if header.verify() && header.device_type() == DeviceType::Network {
                println!("[kernel] Found virtio-net device at 0x{:x}", addr);
                net_addr = addr;
                break;
            }
        }
        
        if net_addr == 0 {
            panic!("[kernel] virtio-net device not found!");
        }

        // 使用写好的 VirtioHal 进行初始化
        let net = unsafe {
            VirtIONet::<VirtioHal>::new(&mut *(net_addr as *mut VirtIOHeader))
                .expect("Failed to initialize virtio-net")
        };
        
        Self(MPSafeCell::new(net))
    }
    //获得mac地址
    pub fn get_mac_address(&self) -> [u8; 6] {
        let net_guard = self.0.exclusive_access(); 
        net_guard.mac() 
    }
}