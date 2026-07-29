//! VisionFive2 板级驱动配置

mod dwmac;
mod sdcard;

use crate::ext4fs::BlockDevice;
use sdcard::SdBlockDevice;
use crate::sync::MPSafeCell;
use alloc::sync::Arc;
use lazy_static::*;

use dwmac::DwMacWrapper;

pub type BlockDeviceImpl = SdBlockDevice;
pub type NetDeviceImpl = DwMacWrapper;