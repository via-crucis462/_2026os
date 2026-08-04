//! qemu virt 内存空间布局信息

/// 主要内存起始地址, 注意linker.ld需要与此同步
pub const MEMORY_BASE: usize = 0x8000_0000;
/// qemu memory size（启动参数 -m 16G）
pub const MEMORY_SIZE: usize = 16 << 30; // 0x4_0000_0000
/// the physical memory end
pub const MEMORY_END: usize = MEMORY_BASE + MEMORY_SIZE; // 0x4_8000_0000
/// virtio 设备单个槽位长度
pub const MMIO_SLOT_SIZE: usize = 0x1000;
/// virtio 设备 mmio 区域长度
pub const BLOCK_MMIO_SIZE: usize = MMIO_SLOT_SIZE * 8;
pub const MMIO: &[(usize, usize)] = &[
    (0x10001000, BLOCK_MMIO_SIZE), // Virtio Block
    (0x10_1000, 0x1000),
];
