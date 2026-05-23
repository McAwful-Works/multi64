//! EverDrive-64 SD access over USB serial using **experimental linear `RomRead`** at
//! `rom_linear_base + LBA * 512` (Krikzz X-series **`usb64`**).

use crate::link::SdCardTransport;
use crate::link::SD_CARD_BUFFER_MAX_BYTES;
use multi64_ed64_link::{looks_like_disk_sector0, Ed64Link, SECTOR_BYTES};
use std::io;

/// Cart-side sector reader via **`usb64` `RomRead`** at a configured linear base.
pub struct Ed64RomLinear {
    link: Ed64Link,
    rom_linear_base: u32,
}

impl Ed64RomLinear {
    pub fn open(port_name: &str, baud: u32, rom_linear_base: u32) -> io::Result<Self> {
        let mut link = Ed64Link::open(port_name, baud)
            .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
        link.set_timeout(std::time::Duration::from_millis(500))?;
        let _ = link.clear_buffers();
        link.test_connection()?;
        let data = link.rom_read(rom_linear_base, SECTOR_BYTES)?;
        if data.len() != SECTOR_BYTES {
            return Err(io::Error::other(
                "ED64: RomRead of sector 0 returned unexpected length; check linear ROM address.",
            ));
        }
        let mut sector = [0u8; SECTOR_BYTES];
        sector.copy_from_slice(&data);
        if !looks_like_disk_sector0(&sector) {
            return Err(io::Error::other(
                "ED64: saved linear ROM address no longer matches a disk-like sector 0. \
Use Settings → Scan for SD base or enter a new address (see docs/spec/ed64-sd-usb-host.md).",
            ));
        }
        Ok(Self {
            link,
            rom_linear_base,
        })
    }

    pub fn rom_linear_base(&self) -> u32 {
        self.rom_linear_base
    }
}

impl SdCardTransport for Ed64RomLinear {
    fn read_sd_sectors(&mut self, start_lba: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() || buf.len() % SECTOR_BYTES != 0 {
            return Err(io::Error::other(
                "ED64 read length must be a non-zero multiple of 512",
            ));
        }
        if buf.len() > SD_CARD_BUFFER_MAX_BYTES {
            return Err(io::Error::other(format!(
                "ED64 read length {} exceeds max batch {}",
                buf.len(),
                SD_CARD_BUFFER_MAX_BYTES
            )));
        }
        if start_lba > u32::MAX as u64 {
            return Err(io::Error::other("sector LBA out of range"));
        }
        let mut offset = 0usize;
        while offset < buf.len() {
            let chunk = (buf.len() - offset).min(SD_CARD_BUFFER_MAX_BYTES);
            let lba = start_lba + (offset / SECTOR_BYTES) as u64;
            let addr = lba
                .checked_mul(SECTOR_BYTES as u64)
                .and_then(|b| b.checked_add(self.rom_linear_base as u64))
                .ok_or_else(|| io::Error::other("ED64 address overflow"))?;
            let addr_u32 = u32::try_from(addr).map_err(|_| {
                io::Error::other("ED64 address does not fit in 32 bits (reduce LBA or base)")
            })?;
            let data = self.link.rom_read(addr_u32, chunk)?;
            if data.len() != chunk {
                return Err(io::Error::other("ED64 RomRead length mismatch"));
            }
            buf[offset..offset + chunk].copy_from_slice(&data);
            offset += chunk;
        }
        Ok(())
    }

    fn write_sd_sectors(&mut self, _start_lba: u64, _buf: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive SD mapping is read-only from the PC in this build.",
        ))
    }

    fn flush_serial(&mut self) -> io::Result<()> {
        self.link.flush_serial()
    }
}
