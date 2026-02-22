pub mod pte;

// la64 qemu改为48，这里是riscv，保持原来的
pub const PA_WIDTH_SV39: usize = 56;
pub const VA_WIDTH_SV39: usize = 39;