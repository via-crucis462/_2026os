//! RISC-V CPU feature detection.
//!
//! The kernel currently advertises no RISC-V HWCAP bits to userspace.
//! Returning 0 keeps the previous behaviour (no `AT_HWCAP` at all), and
//! glibc/musl tolerate a zero HWCAP gracefully.

/// Linux-style HWCAP bitset for the running CPU, advertised via `AT_HWCAP`.
pub fn elf_hwcap() -> usize {
    0
}
