use core::arch::asm;

trait GetBit {
    fn get_bit(self, bit: usize) -> bool;
}

impl GetBit for u32 {
    fn get_bit(self, bit: usize) -> bool {
        if bit >= 32 {
            return false;
        }
        ((self >> bit) & 1) != 0
    }
}

pub const LOONGARCH_IOCSR_IPI_STATUS: usize = 0x1000;
pub const LOONGARCH_IOCSR_IPI_EN: usize = 0x1004;
pub const LOONGARCH_IOCSR_IPI_SET: usize = 0x1008;
pub const LOONGARCH_IOCSR_IPI_CLEAR: usize = 0x100c;
pub const LOONGARCH_CSR_MAIL_BUF0: usize = 0x1020;
pub const LOONGARCH_CSR_MAIL_BUF1: usize = 0x1028;
pub const LOONGARCH_CSR_MAIL_BUF2: usize = 0x1030;
pub const LOONGARCH_CSR_MAIL_BUF3: usize = 0x1038;

pub const IOCSR_MBUF_SEND_CPU_SHIFT: usize = 16;
pub const IOCSR_MBUF_SEND_BUF_SHIFT: usize = 32;
pub const IOCSR_MBUF_SEND_H32_MASK: usize = 0xFFFF_FFFF_0000_0000;

pub const LOONGARCH_IOCSR_IPI_SEND: usize = 0x1040;
pub const IOCSR_IPI_SEND_IP_SHIFT: usize = 0;
pub const IOCSR_IPI_SEND_CPU_SHIFT: usize = 16;
pub const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;

/// Runtime IPI vector reserved for synchronous TLB shootdowns.
pub const TLB_SHOOTDOWN_IPI: u32 = 1 << 1;
/// The architectural IPI line is reported through `CSR.ESTAT.IS[12]`.
pub const IPI_INTERRUPT_BIT: usize = 1 << 12;

pub const LOONGARCH_IOCSR_MBUF_SEND: usize = 0x1048;
pub const IOCSR_MBUF_SEND_BLOCKING: u64 = 1 << 31;
pub const IOCSR_MBUF_SEND_BOX_SHIFT: usize = 2;

fn iocsr_write_u32(addr: usize, value: u32) {
    unsafe {
        asm!("iocsrwr.w {},{}", in(reg) value,in(reg) addr);
    }
}
fn iocsr_read_u32(addr: usize) -> u32 {
    let mut value: u32;
    unsafe {
        asm!("iocsrrd.w {},{}", out(reg) value, in(reg) addr);
    }
    value
}
fn iocsr_write_u64(addr: usize, value: u64) {
    unsafe {
        asm!("iocsrwr.d {},{}", in(reg) value, in(reg) addr);
    }
}
fn iocsr_read_u64(addr: usize) -> u64 {
    let mut value: u64;
    unsafe {
        asm!("iocsrrd.d {},{}", out(reg) value, in(reg) addr);
    }
    value
}

fn iocsr_mbuf_send_box_lo(box_: usize) -> usize {
    box_ << 1
}
fn iocsr_mbuf_send_box_hi(box_: usize) -> usize {
    (box_ << 1) + 1
}

pub fn csr_mail_send(entry: u64, cpu: usize, mailbox: usize) {
    let mut val: u64;
    val = IOCSR_MBUF_SEND_BLOCKING;
    val |= (iocsr_mbuf_send_box_hi(mailbox) << IOCSR_MBUF_SEND_BOX_SHIFT) as u64;
    val |= (cpu << IOCSR_MBUF_SEND_CPU_SHIFT) as u64;
    val |= entry & IOCSR_MBUF_SEND_H32_MASK as u64;
    iocsr_write_u64(LOONGARCH_IOCSR_MBUF_SEND, val);
    val = IOCSR_MBUF_SEND_BLOCKING;
    val |= (iocsr_mbuf_send_box_lo(mailbox) << IOCSR_MBUF_SEND_BOX_SHIFT) as u64;
    val |= (cpu << IOCSR_MBUF_SEND_CPU_SHIFT) as u64;
    val |= entry << IOCSR_MBUF_SEND_BUF_SHIFT;
    iocsr_write_u64(LOONGARCH_IOCSR_MBUF_SEND, val);
}

/// IPI_Send 0x1040 WO 32 位中断分发寄存器
/// `[31]` 等待完成标志，置 1 时会等待中断生效
///
/// `[30:26]` 保留
///
/// `[25:16]` 处理器核号
///
/// `[15:5]` 保留
///
/// `[4:0]` 中断向量号，对应 IPI_Status 中的向量
pub fn ipi_write_action(cpu: usize, action: u32) {
    for i in 0..32 {
        if action.get_bit(i) {
            let mut val: u32 = IOCSR_IPI_SEND_BLOCKING;
            val |= (cpu << IOCSR_IPI_SEND_CPU_SHIFT) as u32;
            val |= i as u32;
            iocsr_write_u32(LOONGARCH_IOCSR_IPI_SEND, val);
        }
    }
}

pub fn send_ipi_single(cpu: usize, action: u32) {
    ipi_write_action(cpu, action);
}

/// Enable the runtime TLB IPI on the current hart and route IPI interrupts.
pub fn init_runtime_ipi() {
    // Firmware may have used a different IPI action while bringing this hart
    // online. Clear every stale action before enabling the runtime vector so a
    // level-triggered IPI line cannot immediately retrigger in user mode.
    iocsr_write_u32(LOONGARCH_IOCSR_IPI_CLEAR, u32::MAX);
    unsafe { asm!("dbar 0") };
    let enabled = iocsr_read_u32(LOONGARCH_IOCSR_IPI_EN);
    iocsr_write_u32(LOONGARCH_IOCSR_IPI_EN, enabled | TLB_SHOOTDOWN_IPI);
    unsafe {
        let mut ecfg: usize;
        asm!("csrrd {}, 0x4", out(reg) ecfg);
        asm!("csrwr {}, 0x4", inout(reg) (ecfg | IPI_INTERRUPT_BIT) => _);
    }
}

/// Atomically consume the IPI actions observed on the current hart.
///
/// The architectural IPI line is level triggered. Clearing only the TLB
/// action would leave another pending action asserting `ESTAT.IS[12]`, causing
/// a user trap loop. The caller dispatches the returned action bits after the
/// whole snapshot has been acknowledged.
pub fn take_ipi_actions() -> u32 {
    let pending = iocsr_read_u32(LOONGARCH_IOCSR_IPI_STATUS);
    if pending != 0 {
        iocsr_write_u32(LOONGARCH_IOCSR_IPI_CLEAR, pending);
        unsafe { asm!("dbar 0") };
    }
    pending
}

/// Raise the TLB IPI on every hart selected by `hart_mask`.
pub fn send_tlb_shootdown(hart_mask: usize) {
    for hart_id in 0..crate::arch::config::CPU_CORE_NUM {
        if hart_mask & (1usize << hart_id) != 0 {
            send_ipi_single(hart_id, TLB_SHOOTDOWN_IPI);
        }
    }
}
