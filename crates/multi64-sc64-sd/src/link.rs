//! Low-level SC64 USB `CMD`/`CMP` exchange for SD + memory.

use multi64_sc64_link::{cmd, cmd_packet, CmpResponse, ResponseBuffer};
use serialport::{ClearBuffer, SerialPort};
use std::io;
use std::time::{Duration, Instant};

/// SDRAM staging for `SD_READ` / `SD_WRITE` + `MEMORY_READ` / `MEMORY_WRITE`.
/// Matches **sc64deployer** `SD_CARD_BUFFER_ADDRESS` / `SD_CARD_BUFFER_LENGTH` (not the older `0x0500_0000` single-sector scratch).
pub const SD_CARD_BUFFER_ADDR: u32 = 0x03FE_0000;
/// Max bytes per SD batch (128 KiB = 256 × 512-byte sectors per deployer).
pub const SD_CARD_BUFFER_MAX_BYTES: usize = 128 * 1024;

const SECTOR_BYTES: usize = 512;

fn memory_io_timeout(len: usize) -> Duration {
    if len <= 4096 {
        Duration::from_secs(5)
    } else {
        Duration::from_secs(30)
    }
}

fn sd_rw_timeout(sector_count: u32) -> Duration {
    Duration::from_secs(8 + (sector_count as u64).saturating_mul(2) / 25)
        .min(Duration::from_secs(90))
}

/// SummerCart64 `sd_error_t` (first `u32` BE in `SD_CARD_OP` ERR payload) → short text for the UI.
fn sd_card_op_user_message(code: u32) -> &'static str {
    match code {
        1 => "No microSD card detected. Insert a card fully into the SC64 slot, then try again.",
        2 => "The SD card is not ready yet. Wait a moment, or remove and reinsert the card.",
        3 => "The cartridge could not complete that SD request. Try again or reconnect USB.",
        4 => "SD access went out of range. The card may be damaged or use a layout this app cannot read.",
        5 => "That SD operation is not allowed right now. Wait for other activity to finish and try again.",
        30 => "The SD card is locked because the N64 may still be using it. Turn the N64 power all the way off (not only reset), then try from the PC again. Powering only the cart from USB can leave the slot locked until the console is fully off.",
        _ => "The SD card reported an error. Try reinserting the card, or power-cycle the N64 and PC.",
    }
}

/// When we get ERR without a decodable `sd_error_t` payload.
fn cmp_err_fallback(cmd_id: u8, _r: &CmpResponse) -> String {
    match cmd_id {
        b'v' => "This serial port did not respond like a SummerCart64. Check the USB cable and COM port selection."
            .to_string(),
        b'i' => "The SD card slot returned an unexpected response. Try reinserting the microSD card or reconnecting USB."
            .to_string(),
        b's' => "Reading from the SD card failed. The card may be loose, busy, or still locked by the console."
            .to_string(),
        b'S' => "Writing to the SD card failed. The card may be write-protected, full, or busy."
            .to_string(),
        b'm' => "Reading from cartridge memory failed. Try again or reconnect USB.".to_string(),
        b'M' => "Writing to cartridge memory failed. Try again or reconnect USB.".to_string(),
        _ => "Communication with the device failed. Try reconnecting USB or choosing another COM port."
            .to_string(),
    }
}

fn cmp_err(_name: &str, cmd_id: u8, r: &CmpResponse) -> io::Error {
    let detail = if cmd_id == b'i' && r.data.len() >= 4 {
        sd_card_op_user_message(u32::from_be_bytes(r.data[0..4].try_into().unwrap())).to_string()
    } else {
        cmp_err_fallback(cmd_id, r)
    };
    io::Error::other(detail)
}

/// USB serial session to an SC64 (exclusive COM port).
pub struct Sc64Link {
    port: Box<dyn SerialPort>,
    rb: ResponseBuffer,
}

impl Sc64Link {
    pub fn open(port_name: &str, baud: u32) -> serialport::Result<Self> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(100))
            .open()?;
        Ok(Self {
            port,
            rb: ResponseBuffer::default(),
        })
    }

    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.port.set_timeout(t).map_err(io::Error::other)
    }

    /// Flush host-side serial TX (call after SD operations so the last USB frames leave the pipe).
    pub fn flush_serial(&mut self) -> io::Result<()> {
        self.port.flush()
    }

    /// Best-effort USB “reset” per vendor docs: clear buffers and toggle DTR so the link resyncs.
    pub fn usb_reset_link(&mut self) -> io::Result<()> {
        self.rb = ResponseBuffer::default();
        let _ = self.port.clear(ClearBuffer::All);
        // DTR/DSR handshake (see SummerCart64 `docs/03_usb_interface.md` — Resetting communication).
        let _ = self.port.write_data_terminal_ready(true);
        std::thread::sleep(Duration::from_millis(80));
        let _ = self.port.clear(ClearBuffer::All);
        let _ = self.port.write_data_terminal_ready(false);
        std::thread::sleep(Duration::from_millis(80));
        let _ = self.port.clear(ClearBuffer::All);
        // Drain any trailing bytes.
        let mut scratch = [0u8; 256];
        let drain_deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < drain_deadline {
            match self.port.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => self.rb.push_bytes(&scratch[..n]),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            }
        }
        self.rb = ResponseBuffer::default();
        Ok(())
    }

    /// Verify we are talking to a SummerCart64 (`IDENTIFIER_GET` → `SC…`).
    pub fn identify(&mut self) -> io::Result<()> {
        let pkt = cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]);
        let r = self.send_cmp_raw(&pkt, cmd::IDENTIFIER_GET, Duration::from_secs(5))?;
        if !r.ok {
            return Err(cmp_err("IDENTIFIER_GET", cmd::IDENTIFIER_GET, &r));
        }
        if !r.data.starts_with(b"SC") {
            return Err(io::Error::other(format!(
                "Not a SummerCart64 (identifier: {:?})",
                String::from_utf8_lossy(&r.data)
            )));
        }
        Ok(())
    }

    /// `SD_CARD_OP` **init** — `arg1 = 1`, `arg0 = 0`.
    ///
    /// **Source of truth:** SummerCart64 `sw/deployer/src/sc64/types.rs` maps
    /// `SdCardOp::Init => [0, 1]` and `SdCardOp::Deinit => [0, 0]` into the CMD `arg0`/`arg1` fields.
    /// The table in `docs/03_usb_interface.md` (listing `0` = Init, `1` = Deinit) does **not** match that
    /// wire encoding; follow the deployer, not that markdown row order.
    pub fn sd_init(&mut self) -> io::Result<()> {
        let pkt = cmd_packet(cmd::SD_CARD_OP, 0, 1, &[]);
        let r = self.send_cmp_raw(&pkt, cmd::SD_CARD_OP, Duration::from_secs(5))?;
        if !r.ok {
            return Err(cmp_err("SD_CARD_OP init", cmd::SD_CARD_OP, &r));
        }
        Ok(())
    }

    /// `SD_CARD_OP` **deinit** — `arg1 = 0`, `arg0 = 0` (see [`sd_init`](Self::sd_init)).
    pub fn sd_deinit(&mut self) -> io::Result<()> {
        let pkt = cmd_packet(cmd::SD_CARD_OP, 0, 0, &[]);
        let r = self.send_cmp_raw(&pkt, cmd::SD_CARD_OP, Duration::from_secs(3))?;
        if !r.ok {
            return Err(cmp_err("SD_CARD_OP deinit", cmd::SD_CARD_OP, &r));
        }
        Ok(())
    }

    /// Best-effort deinit (same packet as [`sd_deinit`](Self::sd_deinit)); ignores timeout/ERR.
    /// Clears a stale PC-side SD session before [`sd_init`](Self::sd_init), matching deployer patterns.
    pub fn sd_deinit_try(&mut self) {
        let pkt = cmd_packet(cmd::SD_CARD_OP, 0, 0, &[]);
        let _ = self.send_cmp_raw(&pkt, cmd::SD_CARD_OP, Duration::from_secs(2));
    }

    /// Read contiguous 512-byte sectors starting at `start_lba` into `buf` (length must be a multiple of 512, ≤ [`SD_CARD_BUFFER_MAX_BYTES`]).
    pub fn read_sd_sectors(&mut self, start_lba: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.len() % SECTOR_BYTES != 0 {
            return Err(io::Error::other("SD read length must be a multiple of 512"));
        }
        if buf.is_empty() {
            return Ok(());
        }
        if buf.len() > SD_CARD_BUFFER_MAX_BYTES {
            return Err(io::Error::other(format!(
                "SD read length {} exceeds max batch {}",
                buf.len(),
                SD_CARD_BUFFER_MAX_BYTES
            )));
        }
        if start_lba > u32::MAX as u64 {
            return Err(io::Error::other("sector LBA out of range"));
        }
        let count = (buf.len() / SECTOR_BYTES) as u32;
        let sec = start_lba as u32;
        let pkt = cmd_packet(cmd::SD_READ, SD_CARD_BUFFER_ADDR, count, &sec.to_be_bytes());
        let r = self.send_cmp_raw(&pkt, cmd::SD_READ, sd_rw_timeout(count))?;
        if !r.ok {
            return Err(cmp_err("SD_READ", cmd::SD_READ, &r));
        }
        let mem = self.memory_read(SD_CARD_BUFFER_ADDR, buf.len())?;
        if mem.len() != buf.len() {
            return Err(io::Error::other("MEMORY_READ length mismatch"));
        }
        buf.copy_from_slice(&mem);
        Ok(())
    }

    /// Read one 512-byte SD sector (LBA) into `buf`.
    pub fn read_sd_sector(
        &mut self,
        sector_lba: u64,
        buf: &mut [u8; SECTOR_BYTES],
    ) -> io::Result<()> {
        self.read_sd_sectors(sector_lba, buf.as_mut_slice())
    }

    /// Write `data` to cart SDRAM/flash at `addr` (`MEMORY_WRITE`).
    pub fn memory_write(&mut self, addr: u32, data: &[u8]) -> io::Result<()> {
        if data.len() > u32::MAX as usize {
            return Err(io::Error::other("MEMORY_WRITE payload too large"));
        }
        let pkt = cmd_packet(cmd::MEMORY_WRITE, addr, data.len() as u32, data);
        let r = self.send_cmp_raw(&pkt, cmd::MEMORY_WRITE, Duration::from_secs(30))?;
        if !r.ok {
            return Err(cmp_err("MEMORY_WRITE", cmd::MEMORY_WRITE, &r));
        }
        Ok(())
    }

    /// Write contiguous 512-byte sectors starting at `start_lba` from `buf`.
    pub fn write_sd_sectors(&mut self, start_lba: u64, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() || buf.len() % SECTOR_BYTES != 0 {
            return Err(io::Error::other(
                "SD write length must be a non-zero multiple of 512",
            ));
        }
        if buf.len() > SD_CARD_BUFFER_MAX_BYTES {
            return Err(io::Error::other(format!(
                "SD write length {} exceeds max batch {}",
                buf.len(),
                SD_CARD_BUFFER_MAX_BYTES
            )));
        }
        if start_lba > u32::MAX as u64 {
            return Err(io::Error::other("sector LBA out of range"));
        }
        let count = (buf.len() / SECTOR_BYTES) as u32;
        self.memory_write(SD_CARD_BUFFER_ADDR, buf)?;
        let sec = start_lba as u32;
        let pkt = cmd_packet(
            cmd::SD_WRITE,
            SD_CARD_BUFFER_ADDR,
            count,
            &sec.to_be_bytes(),
        );
        let r = self.send_cmp_raw(&pkt, cmd::SD_WRITE, sd_rw_timeout(count))?;
        if !r.ok {
            return Err(cmp_err("SD_WRITE", cmd::SD_WRITE, &r));
        }
        Ok(())
    }

    /// Write one 512-byte sector to the SD card (`MEMORY_WRITE` staging + `SD_WRITE`).
    pub fn write_sd_sector(&mut self, sector_lba: u64, buf: &[u8; SECTOR_BYTES]) -> io::Result<()> {
        self.write_sd_sectors(sector_lba, buf.as_slice())
    }

    fn memory_read(&mut self, addr: u32, len: usize) -> io::Result<Vec<u8>> {
        let pkt = cmd_packet(cmd::MEMORY_READ, addr, len as u32, &[]);
        let r = self.send_cmp_raw(&pkt, cmd::MEMORY_READ, memory_io_timeout(len))?;
        if !r.ok {
            return Err(cmp_err("MEMORY_READ", cmd::MEMORY_READ, &r));
        }
        if r.data.len() != len {
            return Err(io::Error::other(format!(
                "MEMORY_READ expected {} bytes, got {}",
                len,
                r.data.len()
            )));
        }
        Ok(r.data)
    }

    fn send_cmp_raw(
        &mut self,
        cmd: &[u8],
        expect_cmd: u8,
        total_timeout: Duration,
    ) -> io::Result<CmpResponse> {
        self.port.write_all(cmd)?;
        self.port.flush()?;
        let start = Instant::now();
        let mut scratch = [0u8; 512];
        while start.elapsed() < total_timeout {
            match self.port.read(&mut scratch) {
                Ok(0) => {}
                Ok(n) => self.rb.push_bytes(&scratch[..n]),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
            while let Some(r) = self.rb.next_cmp() {
                if r.cmd_id == expect_cmd {
                    return Ok(r);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "The SummerCart64 did not respond in time. Check the USB cable and COM port, then try again.",
        ))
    }
}

/// Sector-sized SD access for FAT/exFAT host mounting ([`crate::partition::SectorPartitionDisk`]).
pub trait SdCardTransport: Send {
    fn read_sd_sectors(&mut self, start_lba: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_sd_sectors(&mut self, start_lba: u64, buf: &[u8]) -> io::Result<()>;
    fn flush_serial(&mut self) -> io::Result<()>;
    /// SC64: `SD_CARD_OP` deinit + flush. EverDrive linear: flush only.
    fn release_usb_session(&mut self) -> io::Result<()> {
        self.flush_serial()
    }
}

impl SdCardTransport for Sc64Link {
    fn read_sd_sectors(&mut self, start_lba: u64, buf: &mut [u8]) -> io::Result<()> {
        Sc64Link::read_sd_sectors(self, start_lba, buf)
    }

    fn write_sd_sectors(&mut self, start_lba: u64, buf: &[u8]) -> io::Result<()> {
        Sc64Link::write_sd_sectors(self, start_lba, buf)
    }

    fn flush_serial(&mut self) -> io::Result<()> {
        Sc64Link::flush_serial(self)
    }

    fn release_usb_session(&mut self) -> io::Result<()> {
        self.sd_deinit()?;
        self.flush_serial()
    }
}
