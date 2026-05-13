//! EverDrive-64 SD access over USB serial using the same addressing model as Krikzz tooling:
//! - **edlink** (PRO/CORE): Gen3 `++` link at **921600** baud, **EPO FCI** reads (`DeviceIO_V2.MemRD` in vendor
//!   sources). Default sector-0 base is **`ADDR_FCI_SYS` (0x1000_0000)** from `DEV_ED64/DeviceIO.cs` — the same
//!   numeric window as X-series `usb64` [`multi64_ed64_link::ROM_BASE_ADDRESS`].
//! - **X-series `usb64`**: legacy **`RomRead`** (`R`) at `rom_linear_base + LBA * 512` when the cart does not
//!   answer the edlink handshake.

use crate::link::SdCardTransport;
use multi64_ed64_link::{
    looks_like_disk_sector0, Ed64Link, EdlinkLink, ADDR_FCI_SYS, PROTOCOL_ID_ED64, SECTOR_BYTES,
};
use std::io;

use crate::link::SD_CARD_BUFFER_MAX_BYTES;

enum Ed64Io {
    Usb64(Ed64Link),
    Edlink(EdlinkLink),
}

/// Cart-side sector reader: **edlink EPO / FCI** when available, else **`usb64` `RomRead`**.
pub struct Ed64RomLinear {
    io: Ed64Io,
    rom_linear_base: u32,
}

impl Ed64RomLinear {
    /// `rom_linear_base`: `None` selects **edlink** [`ADDR_FCI_SYS`] when the cart speaks Gen3 ED64; for **usb64**
    /// only carts it must be `Some` (use Settings scan or a known address).
    pub fn open(port_name: &str, baud_legacy: u32, rom_linear_base: Option<u32>) -> io::Result<Self> {
        if let Ok(mut el) = EdlinkLink::try_open(port_name) {
            if el.protocol_id() == PROTOCOL_ID_ED64 {
                el.set_timeout(std::time::Duration::from_millis(500))?;
                let _ = el.clear_buffers();
                let base = rom_linear_base.unwrap_or(ADDR_FCI_SYS);
                let mut sector = [0u8; SECTOR_BYTES];
                el.fci_read(base, &mut sector)?;
                if !looks_like_disk_sector0(&sector) {
                    return Err(io::Error::other(
                        "ED64 (edlink): first 512-byte FCI block at the chosen base does not look like disk sector 0. \
Try Settings → Scan for SD base or set the FCI base manually.",
                    ));
                }
                return Ok(Self {
                    io: Ed64Io::Edlink(el),
                    rom_linear_base: base,
                });
            }
        }

        let base = rom_linear_base.ok_or_else(|| {
            io::Error::other(
                "EverDrive X-series (usb64): no linear ROM base is configured. \
This cart uses the legacy usb64 protocol (not edlink). Open Settings → EverDrive (advanced), then Scan for SD base or enter an address. \
Or use an EverDrive-64 that speaks the edlink Gen3 protocol (default FCI base applies).",
            )
        })?;

        let mut link = Ed64Link::open(port_name, baud_legacy)
            .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
        link.set_timeout(std::time::Duration::from_millis(500))?;
        let _ = link.clear_buffers();
        link.test_connection()?;
        let data = link.rom_read(base, SECTOR_BYTES)?;
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
            io: Ed64Io::Usb64(link),
            rom_linear_base: base,
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
            match &mut self.io {
                Ed64Io::Usb64(link) => {
                    let data = link.rom_read(addr_u32, chunk)?;
                    if data.len() != chunk {
                        return Err(io::Error::other("ED64 RomRead length mismatch"));
                    }
                    buf[offset..offset + chunk].copy_from_slice(&data);
                }
                Ed64Io::Edlink(el) => {
                    el.fci_read(addr_u32, &mut buf[offset..offset + chunk])?;
                }
            }
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
        match &mut self.io {
            Ed64Io::Usb64(l) => l.flush_serial(),
            Ed64Io::Edlink(l) => l.flush_serial(),
        }
    }
}
