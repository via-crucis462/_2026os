use crate::arch::config::SATA_AHCI_MMIO_PA;
use spin::Mutex;

/// SATA 块设备
pub struct SataBlock {
    ctl: AHCIController,
}


impl SataBlock {
    pub fn new() -> Self {
        SataBlock {
            ctl: AHCIController::new(*SATA_AHCI_MMIO_PA),
        }
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
    inner: Mutex<AHCIAccessMethodInner>,
}

pub struct AHCIAccessMethodInner {
    // AHCI 控制器相关字段
}

impl AHCIController {
    pub fn new(base_addr: usize) -> Self {
        AHCIController {
            base_addr,
            inner: Mutex::new(AHCIAccessMethodInner {
                // 初始化相关字段
            }),
        }
    }
    pub fn init(&self) {
        // 初始化AHCI控制器
    }
    pub fn read(){

    }
    pub fn write(){

    }
}
