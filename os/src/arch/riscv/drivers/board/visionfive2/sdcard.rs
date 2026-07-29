use crate::ext4fs::*;
use core::hint::spin_loop;
use core::ptr::{read_volatile, write_volatile};
const SDIO1_BASE: usize = 0x1602_0000;

const CTRL: usize = 0x000;
const PWREN: usize = 0x004;
const CLKDIV: usize = 0x008;
const CLKSRC: usize = 0x00c;
const CLKENA: usize = 0x010;
const TMOUT: usize = 0x014;
const CTYPE: usize = 0x018;
const BLKSIZ: usize = 0x01c;
const BYTCNT: usize = 0x020;
const INTMASK: usize = 0x024;
const CMDARG: usize = 0x028;
const CMD: usize = 0x02c;
const RESP0: usize = 0x030;
const RESP1: usize = 0x034;
const RESP2: usize = 0x038;
const RESP3: usize = 0x03c;
const MINTSTS: usize = 0x040;
const RINTSTS: usize = 0x044;
const STATUS: usize = 0x048;
const FIFOTH: usize = 0x04c;
const CDETECT: usize = 0x050;
const WRTPRT: usize = 0x054;
const TCBCNT: usize = 0x05c;
const TBBCNT: usize = 0x060;
const DEBNCE: usize = 0x064;
const VERID: usize = 0x06c;
const HCON: usize = 0x070;
const UHS_REG: usize = 0x074;
const BMOD: usize = 0x080;
const DBADDR: usize = 0x088;
const IDSTS: usize = 0x08c;
const IDINTEN: usize = 0x090;
const CARDTHRCTL: usize = 0x100;
const DATA: usize = 0x200;
const CMD_RESP_EXPECT: u32 = 1 << 6;
const CMD_RESP_LONG: u32 = 1 << 7;
const CMD_CHECK_RESP_CRC: u32 = 1 << 8;
const CMD_DATA_EXPECTED: u32 = 1 << 9;
const CMD_WAIT_PRVDATA: u32 = 1 << 13;
const CMD_SEND_INITIALIZATION: u32 = 1 << 15;
const CMD_USE_HOLD_REG: u32 = 1 << 29;
const CMD_START: u32 = 1 << 31;
const CMD_WRITE: u32 = 1 << 10;
const CTRL_FIFO_RESET: u32 = 1 << 1;
const CTRL_DMA_ENABLE: u32 = 1 << 5;
const CTRL_USE_IDMAC: u32 = 1 << 25;
const BMOD_DE: u32 = 1 << 7;
const INT_RE: u32 = 1 << 1;
const INT_CMD_DONE: u32 = 1 << 2;
const INT_DATA_OVER: u32 = 1 << 3;
const INT_RXDR: u32 = 1 << 5;
const INT_RCRC: u32 = 1 << 6;
const INT_DCRC: u32 = 1 << 7;
const INT_RTO: u32 = 1 << 8;
const INT_DRTO: u32 = 1 << 9;
const INT_HTO: u32 = 1 << 10;
const INT_FRUN: u32 = 1 << 11;
const INT_HLE: u32 = 1 << 12;
const INT_SBE: u32 = 1 << 13;
const INT_EBE: u32 = 1 << 15;

const CMD_UPDATE_CLOCK: u32 = 1 << 21;

const CMD_ERROR_MASK: u32 = INT_RE | INT_RCRC | INT_RTO | INT_HLE;
const STATUS_FIFO_COUNT_SHIFT: u32 = 17;
const STATUS_FIFO_COUNT_MASK: u32 = 0x1fff;
const DATA_ERROR_MASK: u32 = INT_DCRC | INT_DRTO | INT_FRUN | INT_SBE | INT_EBE;
const HCON_DATA_WIDTH_SHIFT: u32 = 7;
const HCON_DATA_WIDTH_MASK: u32 = 0x7;
const FIFOTH_RX_WMARK_SHIFT: u32 = 16;
const FIFOTH_WMARK_MASK: u32 = 0xfff;
#[inline(always)]
fn reg(offset: usize) -> *mut u32 {
    (SDIO1_BASE + offset) as *mut u32
}

#[inline(always)]
fn read_reg(offset: usize) -> u32 {
    unsafe { read_volatile(reg(offset)) }
}

#[inline(always)]
fn write_reg(offset: usize, value: u32) {
    unsafe {
        write_volatile(reg(offset), value);
    }
}
pub fn probe() {
    let ctrl = read_reg(CTRL);
    let pwren = read_reg(PWREN);
    let clkena = read_reg(CLKENA);
    let status = read_reg(STATUS);
    let cdetect = read_reg(CDETECT);
    let fifoth = read_reg(FIFOTH);

    println!(
        "[sd] ctrl={:#x} pwren={:#x} clkena={:#x}",
        ctrl, pwren, clkena
    );
    println!(
        "[sd] status={:#x} cdetect={:#x} fifoth={:#x}",
        status, cdetect, fifoth
    );
    println!(
        "[sd] verid={:#x} hcon={:#x}",
        read_reg(VERID),
        read_reg(HCON),
    );
}
#[derive(Debug, Clone, Copy)]
pub enum SdError {
    NoCard,
    Timeout,
    CommandBusy,
    Command { index: u32, status: u32 },
    Data(u32),
    UnsupportedFifoWidth(u32),
    BadResponse { index: u32, response: u32 },
}
fn wait_cmd_idle() -> Result<(), SdError> {
    for _ in 0..1_000_000 {
        if read_reg(CMD) & CMD_START == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }

    Err(SdError::CommandBusy)
}

fn reset_fifo() -> Result<(), SdError> {
    write_reg(
        CTRL,
        (read_reg(CTRL) & !(CTRL_DMA_ENABLE | CTRL_USE_IDMAC)) | CTRL_FIFO_RESET,
    );

    for _ in 0..1_000_000 {
        if read_reg(CTRL) & CTRL_FIFO_RESET == 0 {
            return Ok(());
        }
        spin_loop();
    }

    Err(SdError::Timeout)
}

fn send_cmd(index: u32, argument: u32, flags: u32) -> Result<[u32; 4], SdError> {
    wait_cmd_idle()?;

    write_reg(RINTSTS, u32::MAX);
    write_reg(CMDARG, argument);

    write_reg(CMD, CMD_START | CMD_USE_HOLD_REG | (index & 0x3f) | flags);

    for _ in 0..5_000_000 {
        let status = read_reg(RINTSTS);

        if status & CMD_ERROR_MASK != 0 {
            write_reg(RINTSTS, status);

            return Err(SdError::Command { index, status });
        }

        if status & INT_CMD_DONE != 0 {
            write_reg(RINTSTS, INT_CMD_DONE);

            return Ok([
                read_reg(RESP0),
                read_reg(RESP1),
                read_reg(RESP2),
                read_reg(RESP3),
            ]);
        }

        core::hint::spin_loop();
    }

    Err(SdError::Timeout)
}
fn prepare_controller() -> Result<(), SdError> {
    if read_reg(CDETECT) & 1 != 0 {
        return Err(SdError::NoCard);
    }

    // 轮询模式，不使用硬件中断。
    write_reg(INTMASK, 0);

    // PIO only: disable both the controller DMA request and the internal DMA engine.
    write_reg(CTRL, read_reg(CTRL) & !(CTRL_DMA_ENABLE | CTRL_USE_IDMAC));
    write_reg(BMOD, read_reg(BMOD) & !BMOD_DE);
    reset_fifo()?;

    // RX watermark 0 makes RXDR assert as soon as data is available.  The
    // status FIFO count is still used to drain every available word.
    write_reg(FIFOTH, 2 << 28);

    // 清除遗留中断。
    write_reg(RINTSTS, u32::MAX);

    // 响应和数据超时设为最大。
    write_reg(TMOUT, 0xffff_ffff);

    // 初始化时先使用 1-bit。
    write_reg(CTYPE, 0);

    // 一个 SD 扇区。
    write_reg(BLKSIZ, 512);
    write_reg(BYTCNT, 0);

    println!(
        "[sd] prepared: ctrl={:#x}, clkena={:#x}, status={:#x}",
        read_reg(CTRL),
        read_reg(CLKENA),
        read_reg(STATUS),
    );

    Ok(())
}
fn cmd0_go_idle() -> Result<(), SdError> {
    send_cmd(0, 0, CMD_SEND_INITIALIZATION)?;

    Ok(())
}
pub struct SdCard {
    rca: u32,
    high_capacity: bool,
    initialized: bool,
    fifo_width: usize,
    fifo_depth: usize,
}

impl SdCard {
    pub const fn new() -> Self {
        Self {
            rca: 0,
            high_capacity: false,
            initialized: false,
            fifo_width: 4,
            fifo_depth: 32,
        }
    }

    pub fn init(&mut self) -> Result<(), SdError> {
        let initial_fifoth = read_reg(FIFOTH);
        prepare_controller()?;

        let hcon_width = (read_reg(HCON) >> HCON_DATA_WIDTH_SHIFT) & HCON_DATA_WIDTH_MASK;
        self.fifo_width = match hcon_width {
            0 => 2,
            1 => 4,
            2 => 8,
            value => return Err(SdError::UnsupportedFifoWidth(value)),
        };
        let rx_watermark = ((initial_fifoth >> FIFOTH_RX_WMARK_SHIFT) & FIFOTH_WMARK_MASK) as usize;
        let tx_watermark = (initial_fifoth & FIFOTH_WMARK_MASK) as usize;
        self.fifo_depth = rx_watermark + tx_watermark + 1;
        if self.fifo_depth == 0 {
            self.fifo_depth = 32;
        }
        println!(
            "[sd] FIFO access width: {} bits, depth: {}",
            self.fifo_width * 8,
            self.fifo_depth,
        );

        // CMD0：进入 Idle 状态。
        send_cmd(0, 0, CMD_SEND_INITIALIZATION)?;

        // CMD8：确认 SD v2，检查电压和 pattern。
        let cmd8 = send_cmd(8, 0x1aa, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;

        if cmd8[0] & 0xfff != 0x1aa {
            return Err(SdError::BadResponse {
                index: 8,
                response: cmd8[0],
            });
        }

        // CMD55 + ACMD41：等待卡上电初始化完成。
        let mut ocr = 0;

        for _ in 0..100_000 {
            send_cmd(55, 0, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;

            let response = send_cmd(41, 0x40ff_8000, CMD_RESP_EXPECT)?;

            ocr = response[0];

            if ocr & (1 << 31) != 0 {
                break;
            }
        }

        if ocr & (1 << 31) == 0 {
            return Err(SdError::Timeout);
        }
        self.high_capacity = ocr & (1 << 30) != 0;

        // CMD2：读取 CID，长响应。
        send_cmd(2, 0, CMD_RESP_EXPECT | CMD_RESP_LONG | CMD_CHECK_RESP_CRC)?;

        // CMD3：获取 RCA。
        let response = send_cmd(3, 0, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;

        self.rca = response[0] >> 16;

        if self.rca == 0 {
            return Err(SdError::BadResponse {
                index: 3,
                response: response[0],
            });
        }

        // CMD7：选择卡，进入 Transfer 状态。
        send_cmd(
            7,
            self.rca << 16,
            CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC | CMD_WAIT_PRVDATA,
        )?;

        // ACMD6：切换到 4-bit 数据总线。
        send_cmd(55, self.rca << 16, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;

        send_cmd(6, 2, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;

        // 控制器端也切到 4-bit。
        write_reg(CTYPE, 1);
        set_card_clock_divider(16)?;
        // SDSC 使用字节地址，需要显式设置 512 字节块长。
        // SDHC/SDXC 固定为 512 字节扇区，不需要 CMD16。
        if !self.high_capacity {
            send_cmd(16, 512, CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC)?;
        }

        self.initialized = true;

        println!(
            "[sd] initialized: rca={:#x}, high_capacity={}",
            self.rca, self.high_capacity,
        );

        Ok(())
    }
}
impl SdCard {
    unsafe fn write_fifo_entry(&self, src: *const u8) {
        match self.fifo_width {
            2 => write_volatile(
                (SDIO1_BASE + DATA) as *mut u16,
                u16::from_le(core::ptr::read_unaligned(src as *const u16)),
            ),
            4 => write_volatile(
                (SDIO1_BASE + DATA) as *mut u32,
                u32::from_le(core::ptr::read_unaligned(src as *const u32)),
            ),
            8 => write_volatile(
                (SDIO1_BASE + DATA) as *mut u64,
                u64::from_le(core::ptr::read_unaligned(src as *const u64)),
            ),
            _ => unreachable!(),
        }
    }

    fn wait_card_ready(&mut self) -> Result<(), SdError> {
        for _ in 0..1_000_000 {
            let response = send_cmd(
                13,
                self.rca << 16,
                CMD_RESP_EXPECT | CMD_CHECK_RESP_CRC | CMD_WAIT_PRVDATA,
            )?[0];
            let ready_for_data = response & (1 << 8) != 0;
            let current_state = (response >> 9) & 0xf;
            if ready_for_data && current_state == 4 {
                return Ok(());
            }
            spin_loop();
        }
        Err(SdError::Timeout)
    }

    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), SdError> {
        if !self.initialized {
            return Err(SdError::Timeout);
        }

        wait_cmd_idle()?;
        reset_fifo()?;
        write_reg(RINTSTS, u32::MAX);
        write_reg(BLKSIZ, 512);
        write_reg(BYTCNT, 512);

        let argument = if self.high_capacity {
            sector as u32
        } else {
            (sector * 512) as u32
        };

        // DW-MMC CMD bit 10:
        // 0 = read
        // 1 = write
        write_reg(CMDARG, argument);
        write_reg(
            CMD,
            CMD_START
                | CMD_USE_HOLD_REG
                | 17
                | CMD_RESP_EXPECT
                | CMD_CHECK_RESP_CRC
                | CMD_DATA_EXPECTED
                | CMD_WAIT_PRVDATA,
        );

        let mut bytes_read = 0usize;
        let mut command_done = false;

        for _ in 0..10_000_000 {
            let status = read_reg(STATUS);
            let fifo_count =
                ((status >> STATUS_FIFO_COUNT_SHIFT) & STATUS_FIFO_COUNT_MASK) as usize;

            let entries_needed = (512 - bytes_read) / self.fifo_width;
            let available = core::cmp::min(fifo_count, entries_needed);

            for _ in 0..available {
                unsafe {
                    let dst = buf.as_mut_ptr().add(bytes_read);
                    match self.fifo_width {
                        2 => core::ptr::write_unaligned(
                            dst as *mut u16,
                            read_volatile((SDIO1_BASE + DATA) as *const u16).to_le(),
                        ),
                        4 => core::ptr::write_unaligned(
                            dst as *mut u32,
                            read_volatile((SDIO1_BASE + DATA) as *const u32).to_le(),
                        ),
                        8 => core::ptr::write_unaligned(
                            dst as *mut u64,
                            read_volatile((SDIO1_BASE + DATA) as *const u64).to_le(),
                        ),
                        _ => unreachable!(),
                    }
                }

                bytes_read += self.fifo_width;
            }

            // Data can enter the FIFO before CMD_DONE is observed.  Drain it
            // first on every iteration so a full FIFO cannot turn into FRUN.
            let interrupts = read_reg(RINTSTS);

            if interrupts & CMD_ERROR_MASK != 0 {
                write_reg(RINTSTS, interrupts);
                return Err(SdError::Command {
                    index: 17,
                    status: interrupts,
                });
            }

            if interrupts & INT_CMD_DONE != 0 {
                command_done = true;
                write_reg(RINTSTS, INT_CMD_DONE);
            }

            if interrupts & DATA_ERROR_MASK != 0 {
                println!(
                    "[sd] data error: sector={}, bytes={}, rintsts={:#x}, status={:#x}, fifo={}, tcbcnt={}, tbbcnt={}",
                    sector,
                    bytes_read,
                    interrupts,
                    read_reg(STATUS),
                    (read_reg(STATUS) >> STATUS_FIFO_COUNT_SHIFT) & STATUS_FIFO_COUNT_MASK,
                    read_reg(TCBCNT),
                    read_reg(TBBCNT),
                );
                write_reg(RINTSTS, interrupts);

                return Err(SdError::Data(interrupts));
            }

            let fifo_empty =
                ((read_reg(STATUS) >> STATUS_FIFO_COUNT_SHIFT) & STATUS_FIFO_COUNT_MASK) == 0;

            if command_done && bytes_read == 512 && fifo_empty {
                if interrupts & INT_DATA_OVER != 0 {
                    write_reg(RINTSTS, INT_DATA_OVER | INT_RXDR);
                    return Ok(());
                }

                // Some revisions report HTO rather than DATA_OVER after a
                // completed PIO transfer.  It is only safe once the FIFO was
                // actually drained and both hardware counters reached 512.
                if interrupts & INT_HTO != 0 && read_reg(TCBCNT) == 512 && read_reg(TBBCNT) == 512 {
                    write_reg(RINTSTS, INT_HTO | INT_RXDR);
                    return Ok(());
                }
            }

            if interrupts & INT_HTO != 0 {
                println!(
                    "[sd] host timeout: sector={}, bytes={}, status={:#x}, fifo={}, tcbcnt={}, tbbcnt={}",
                    sector,
                    bytes_read,
                    read_reg(STATUS),
                    (read_reg(STATUS) >> STATUS_FIFO_COUNT_SHIFT) & STATUS_FIFO_COUNT_MASK,
                    read_reg(TCBCNT),
                    read_reg(TBBCNT),
                );
                write_reg(RINTSTS, interrupts);
                return Err(SdError::Data(interrupts));
            }

            core::hint::spin_loop();
        }
        println!(
        "[sd] read timeout: sector={}, bytes={}, rintsts={:#x}, status={:#x}, tcbcnt={}, tbbcnt={}",
        sector,
        bytes_read,
        read_reg(RINTSTS),
        read_reg(STATUS),
        read_reg(TCBCNT),
        read_reg(TBBCNT),
    );

        Err(SdError::Timeout)
    }

    pub fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), SdError> {
        if !self.initialized {
            return Err(SdError::Timeout);
        }

        self.wait_card_ready()?;
        wait_cmd_idle()?;
        reset_fifo()?;
        write_reg(RINTSTS, u32::MAX);
        write_reg(BLKSIZ, 512);
        write_reg(BYTCNT, 512);

        let argument = if self.high_capacity {
            sector as u32
        } else {
            (sector * 512) as u32
        };

        write_reg(CMDARG, argument);
        write_reg(
            CMD,
            CMD_START
                | CMD_USE_HOLD_REG
                | 24
                | CMD_RESP_EXPECT
                | CMD_CHECK_RESP_CRC
                | CMD_DATA_EXPECTED
                | CMD_WRITE
                | CMD_WAIT_PRVDATA,
        );

        let mut bytes_written = 0usize;
        let mut command_done = false;

        for _ in 0..10_000_000 {
            let interrupts = read_reg(RINTSTS);

            if interrupts & CMD_ERROR_MASK != 0 {
                write_reg(RINTSTS, interrupts);
                return Err(SdError::Command {
                    index: 24,
                    status: interrupts,
                });
            }

            if interrupts & INT_CMD_DONE != 0 {
                command_done = true;
                write_reg(RINTSTS, INT_CMD_DONE);
            }

            if interrupts & DATA_ERROR_MASK != 0 || interrupts & INT_HTO != 0 {
                println!(
                    "[sd] write error: sector={}, bytes={}, rintsts={:#x}, status={:#x}, tcbcnt={}, tbbcnt={}",
                    sector,
                    bytes_written,
                    interrupts,
                    read_reg(STATUS),
                    read_reg(TCBCNT),
                    read_reg(TBBCNT),
                );
                write_reg(RINTSTS, interrupts);
                return Err(SdError::Data(interrupts));
            }

            let fifo_count =
                ((read_reg(STATUS) >> STATUS_FIFO_COUNT_SHIFT) & STATUS_FIFO_COUNT_MASK) as usize;
            let fifo_space = self.fifo_depth.saturating_sub(fifo_count);
            let entries_left = (512 - bytes_written) / self.fifo_width;
            let entries = core::cmp::min(fifo_space, entries_left);

            for _ in 0..entries {
                unsafe {
                    self.write_fifo_entry(buf.as_ptr().add(bytes_written));
                }
                bytes_written += self.fifo_width;
            }

            let interrupts = read_reg(RINTSTS);
            if command_done && bytes_written == 512 && interrupts & INT_DATA_OVER != 0 {
                write_reg(RINTSTS, INT_DATA_OVER);
                self.wait_card_ready()?;
                return Ok(());
            }

            spin_loop();
        }

        println!(
            "[sd] write timeout: sector={}, bytes={}, rintsts={:#x}, status={:#x}, tcbcnt={}, tbbcnt={}",
            sector,
            bytes_written,
            read_reg(RINTSTS),
            read_reg(STATUS),
            read_reg(TCBCNT),
            read_reg(TBBCNT),
        );
        Err(SdError::Timeout)
    }
}
fn partition_entries(mbr: &[u8; 512]) -> impl Iterator<Item = (usize, u8, u32, u32)> + '_ {
    (0..4).filter_map(|index| {
        let offset = 446 + index * 16;
        let entry = &mbr[offset..offset + 16];
        let partition_type = entry[4];
        let start_lba = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        let sector_count = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);

        (start_lba != 0 && sector_count != 0).then_some((
            index,
            partition_type,
            start_lba,
            sector_count,
        ))
    })
}

fn find_ext4_partition_lba(card: &mut SdCard, mbr: &[u8; 512]) -> Option<u32> {
    if mbr[510] == 0x55 && mbr[511] == 0xaa {
        for (index, partition_type, start_lba, sector_count) in partition_entries(mbr) {
            println!(
                "[sd] partition {}: type={:#04x}, start={}, sectors={}",
                index, partition_type, start_lba, sector_count,
            );

            let mut superblock_sector = [0u8; 512];
            if card
                .read_sector(start_lba as u64 + 2, &mut superblock_sector)
                .is_err()
            {
                continue;
            }
            let magic = u16::from_le_bytes([superblock_sector[0x38], superblock_sector[0x39]]);
            println!("[sd] partition {} ext4 magic={:#06x}", index, magic);
            if magic == 0xef53 {
                return Some(start_lba);
            }
        }
    } else {
        println!("[sd] no MBR signature, checking whole-device ext4");
    }

    let mut superblock_sector = [0u8; 512];
    card.read_sector(2, &mut superblock_sector).ok()?;
    let magic = u16::from_le_bytes([superblock_sector[0x38], superblock_sector[0x39]]);
    println!("[sd] whole-device ext4 magic={:#06x}", magic);
    if magic == 0xef53 {
        Some(0)
    } else {
        None
    }
}
pub fn test_read_sector() {
    let mut card = SdCard::new();

    card.init().expect("[sd] initialization failed");

    let mut buf = [0u8; 512];

    card.read_sector(0, &mut buf)
        .expect("[sd] failed to read sector 0");

    println!("[sd] sector 0:");

    for row in 0..4 {
        for col in 0..16 {
            print!("{:02x} ", buf[row * 16 + col]);
        }
    }

    println!("[sd] MBR signature: {:02x} {:02x}", buf[510], buf[511],);
}
pub fn first_partition_lba(mbr: &[u8; 512]) -> Option<u32> {
    if mbr[510] != 0x55 || mbr[511] != 0xaa {
        return None;
    }

    let entry = &mbr[446..462];

    Some(u32::from_le_bytes([
        entry[8], entry[9], entry[10], entry[11],
    ]))
}
pub fn test_sd_and_ext4() {
    let mut card = SdCard::new();

    card.init().expect("[sd] initialization failed");

    let mut mbr = [0u8; 512];

    card.read_sector(0, &mut mbr)
        .expect("[sd] failed to read MBR");

    println!("[sd] MBR signature: {:02x} {:02x}", mbr[510], mbr[511],);

    let partition_lba =
        find_ext4_partition_lba(&mut card, &mbr).expect("[sd] no ext4 partition found");

    println!(
        "[sd] ext4 candidate partition starts at LBA {}",
        partition_lba,
    );

    // ext4 超级块位于分区起点之后 1024 字节，
    // 即第 2 个 512-byte sector。
    let mut superblock_sector = [0u8; 512];

    card.read_sector(partition_lba as u64 + 2, &mut superblock_sector)
        .expect("[sd] failed to read ext4 superblock sector");

    // ext4 magic 位于超级块内部偏移 0x38。
    let magic = u16::from_le_bytes([superblock_sector[0x38], superblock_sector[0x39]]);

    println!(
        "[sd] ext4 magic at partition {} = {:#06x}",
        partition_lba, magic,
    );

    assert_eq!(magic, 0xef53, "[sd] selected partition is not ext4");
}
use spin::Mutex;

pub struct SdBlockDevice {
    card: Mutex<SdCard>,
    partition_start_sector: u64,
}
impl SdBlockDevice {
    pub fn new() -> Self {
        let mut card = SdCard::new();

        card.init().expect("[sd] initialization failed");

        let mut mbr = [0u8; 512];

        card.read_sector(0, &mut mbr)
            .expect("[sd] failed to read partition table");

        let partition_start_sector =
            find_ext4_partition_lba(&mut card, &mbr).expect("[sd] ext4 partition not found") as u64;

        let mut sb_sector = [0u8; 512];

        card.read_sector(partition_start_sector + 2, &mut sb_sector)
            .expect("[sd] failed to read ext4 superblock");

        let magic = u16::from_le_bytes([sb_sector[0x38], sb_sector[0x39]]);

        assert_eq!(magic, 0xef53, "[sd] chosen partition is not ext4");

        println!(
            "[sd] ext4 partition ready: start_lba={}",
            partition_start_sector,
        );

        Self {
            card: Mutex::new(card),
            partition_start_sector,
        }
    }
}
const SECTOR_SIZE: usize = 512;
const SECTORS_PER_BLOCK: usize = BLOCK_SZ / SECTOR_SIZE;
impl BlockDevice for SdBlockDevice {
    fn raw_read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len(), BLOCK_SZ, "SD raw block read requires 4096 bytes");

        let first_sector = self.partition_start_sector + block_id as u64 * SECTORS_PER_BLOCK as u64;

        let mut card = self.card.lock();

        for index in 0..SECTORS_PER_BLOCK {
            let begin = index * SECTOR_SIZE;
            let end = begin + SECTOR_SIZE;

            let sector_buf: &mut [u8; SECTOR_SIZE] = (&mut buf[begin..end]).try_into().unwrap();

            card.read_sector(first_sector + index as u64, sector_buf)
                .unwrap_or_else(|error| {
                    panic!(
                        "[sd] block read failed: block={}, sector={}, error={:?}",
                        block_id,
                        first_sector + index as u64,
                        error,
                    )
                });
        }
    }

    fn raw_write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(
            buf.len(),
            BLOCK_SZ,
            "SD raw block write requires 4096 bytes"
        );

        let first_sector = self.partition_start_sector + block_id as u64 * SECTORS_PER_BLOCK as u64;
        let mut card = self.card.lock();

        for index in 0..SECTORS_PER_BLOCK {
            let begin = index * SECTOR_SIZE;
            let end = begin + SECTOR_SIZE;
            let sector_buf: &[u8; SECTOR_SIZE] = (&buf[begin..end]).try_into().unwrap();

            card.write_sector(first_sector + index as u64, sector_buf)
                .unwrap_or_else(|error| {
                    panic!(
                        "[sd] block write failed: block={}, sector={}, error={:?}",
                        block_id,
                        first_sector + index as u64,
                        error,
                    )
                });
        }
    }

    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert!(buf.len() <= BLOCK_SZ);

        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());

        let cache = cache.lock();
        let data: &[u8; BLOCK_SZ] = cache.get_ref(0);

        buf.copy_from_slice(&data[..buf.len()]);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert!(buf.len() <= BLOCK_SZ);

        let cache = get_block_cache(block_id, BLOCK_DEVICE.clone());
        cache.lock().modify(0, |data: &mut [u8; BLOCK_SZ]| {
            data[..buf.len()].copy_from_slice(buf);
        });
    }
}
fn update_card_clock() -> Result<(), SdError> {
    wait_cmd_idle()?;
    write_reg(RINTSTS, u32::MAX);
    write_reg(CMD, CMD_START | CMD_UPDATE_CLOCK | CMD_WAIT_PRVDATA);

    for _ in 0..1_000_000 {
        if read_reg(CMD) & CMD_START == 0 {
            let status = read_reg(RINTSTS);

            if status & INT_HLE != 0 {
                write_reg(RINTSTS, INT_HLE);
                return Err(SdError::Command { index: 0, status });
            }

            return Ok(());
        }

        core::hint::spin_loop();
    }

    Err(SdError::Timeout)
}
fn set_card_clock_divider(divider: u32) -> Result<(), SdError> {
    // 先关闭卡时钟。
    write_reg(CLKENA, 0);
    update_card_clock()?;

    // 使用时钟源 0。
    write_reg(CLKSRC, 0);
    write_reg(CLKDIV, divider);
    update_card_clock()?;

    // 再打开卡时钟。
    write_reg(CLKENA, 1);
    update_card_clock()?;

    Ok(())
}
