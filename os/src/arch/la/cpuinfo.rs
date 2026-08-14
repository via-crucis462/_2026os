//! LoongArch CPU feature detection via the CPUCFG instruction.
//!
//! The HWCAP bit layout mirrors Linux `arch/loongarch/include/uapi/asm/hwcap.h`,
//! so the value returned here can be advertised to userspace through
//! `AT_HWCAP` in the process auxiliary vector.  Programs such as QEMU read
//! `AT_HWCAP` to decide whether the host CPU supports features like unaligned
//! access; QEMU's loongarch64 TCG backend refuses to start unless
//! `HWCAP_LOONGARCH_UAL` is set.
//!
//! IMPORTANT: only advertise features the kernel actually implements.  The
//! CPU (via CPUCFG) may report LASX, but the kernel does not enable EUEN.ASXE
//! nor save the 256-bit LASX registers, so executing a LASX instruction
//! raises an ASXD exception.  If `HWCAP_LOONGARCH_LASX` were advertised,
//! glibc's IFUNC resolver would pick the LASX memcpy/memset and the process
//! would die with SIGSEGV — exactly what happened when this file was first
//! added without this restriction.

/// HWCAP_LOONGARCH_CPUCFG: CPUCFG instruction available.
const HWCAP_LOONGARCH_CPUCFG: usize = 1 << 0;
/// HWCAP_LOONGARCH_UAL: unaligned access support.
const HWCAP_LOONGARCH_UAL: usize = 1 << 2;
/// HWCAP_LOONGARCH_FPU: floating point unit.
const HWCAP_LOONGARCH_FPU: usize = 1 << 3;
/// HWCAP_LOONGARCH_LSX: 128-bit SIMD extensions.
const HWCAP_LOONGARCH_LSX: usize = 1 << 4;
// 注意：不定义/不宣告 HWCAP_LOONGARCH_LASX（1 << 5）等内核未支持的位。

/// CPUCFG1.UAL (unaligned access) bit position.
const CPUCFG1_UAL: usize = 20;
/// CPUCFG2 field bit positions (see the LoongArch reference manual).
const CPUCFG2_FP: usize = 0;
const CPUCFG2_LSX: usize = 6;

/// Read the given CPUCFG register.
#[inline]
fn cpucfg(index: usize) -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) value, in(reg) index);
    }
    value
}

/// Linux-style HWCAP bitset for the running CPU, advertised via `AT_HWCAP`.
///
/// Only bits the kernel fully supports are set (see the module note on LASX
/// above).
pub fn elf_hwcap() -> usize {
    let cfg1 = cpucfg(1);
    let cfg2 = cpucfg(2);

    let mut hwcap = HWCAP_LOONGARCH_CPUCFG;
    if (cfg1 >> CPUCFG1_UAL) & 1 == 1 {
        hwcap |= HWCAP_LOONGARCH_UAL;
    }
    if (cfg2 >> CPUCFG2_FP) & 1 == 1 {
        hwcap |= HWCAP_LOONGARCH_FPU;
    }
    if (cfg2 >> CPUCFG2_LSX) & 1 == 1 {
        hwcap |= HWCAP_LOONGARCH_LSX;
    }
    hwcap
}
