//! qemu virt 内存空间布局信息

// RAM 区间在启动时从设备树发现。内核装载地址仍由 linker-virt.ld 固定，
// 但该地址不再用于推导 RAM 边界。
/// virtio 设备单个槽位长度
pub const MMIO_SLOT_SIZE: usize = 0x1000;
/// virtio 设备 mmio 区域长度
pub const BLOCK_MMIO_SIZE: usize = MMIO_SLOT_SIZE * 8;
pub const MMIO: &[(usize, usize)] = &[
    (0x10001000, BLOCK_MMIO_SIZE), // Virtio Block
    (0x10_1000, 0x1000),
];
