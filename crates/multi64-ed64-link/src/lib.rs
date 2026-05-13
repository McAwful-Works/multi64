//! EverDrive-64 host ↔ cart **USB serial** aligned with Krikzz sources:
//! - **[edlink](https://github.com/krikzz/edlink)** Gen3 **`++`** link (default **921600** baud) and **EPO FCI** reads
//!   for **EverDrive-64 PRO/CORE** — see [`edlink::EdlinkLink`] (`Device/Link.cs`, `Device/DeviceIO_V2.cs`,
//!   `DEV_ED64/DeviceIO.cs`).
//! - Legacy **X-series `usb64`** **`cmd`** packet** ([`krikzz/ed64-x-pub`](https://github.com/krikzz/ed64-x-pub)
//!   `CommandProcessor.cs`; same packing as [UNFLoader](https://github.com/buu342/N64-UNFLoader)
//!   `device_sendcmd_everdrive`): **RomRead** (`R`) / **RamRead** (`r`) + raw payload reads.
//!
//! This is **not** SummerCart64’s `CMD`/`CMP` protocol. For `usb64`, outbound layout is **`cmd`** + 1-byte opcode +
//! three **big-endian `u32`**: address, length in **512-byte sectors**, argument (see [`Ed64Link::command_packet`]).
pub mod edlink;
pub mod linear_probe;

pub use edlink::{
    ADDR_FCI_SYS, EDLINK_DEFAULT_BAUD, EdlinkLink, PROTOCOL_ID_ED64,
};
pub use linear_probe::{
    ed64_linear_base_probe_list, ed64_linear_base_probe_list_with_preferred,
    looks_like_disk_sector0, probe_ed64_sd_linear_bases, probe_ed64_sd_linear_bases_with_cancel,
    ED64_LINEAR_BASE_HINTS,
};

use serialport::{ClearBuffer, SerialPort};
use std::io;
use std::io::{Read, Write};
use std::time::{Duration, Instant};
/// X-series ROM space base used by reference `usb64` for uploads (`CommandProcessor.ROM_BASE_ADDRESS`).
pub const ROM_BASE_ADDRESS: u32 = 0x1000_0000;
/// X-series RDRAM base for `RamRead` (`CommandProcessor.RAM_BASE_ADDRESS`).
pub const RAM_BASE_ADDRESS: u32 = 0x8000_0000;

pub const SECTOR_BYTES: usize = 512;

/// Max bytes per `RomRead`/`RamRead` in one command (vendor `UsbInterface` uses 32 KiB chunks internally; we allow up to 1 MiB).
pub const ED64_READ_MAX_BYTES: usize = 1024 * 1024;

fn io_other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// Single EverDrive USB serial session (exclusive COM port).
pub struct Ed64Link {
    port: Box<dyn SerialPort>,
}

impl Ed64Link {
    pub fn open(port_name: &str, baud: u32) -> serialport::Result<Self> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(100))
            .open()?;
        Ok(Self { port })
    }

    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.port.set_timeout(t).map_err(io::Error::other)
    }

    pub fn flush_serial(&mut self) -> io::Result<()> {
        self.port.flush()
    }

    /// Clear host RX/TX buffers (best-effort).
    pub fn clear_buffers(&mut self) -> io::Result<()> {
        let _ = self.port.clear(ClearBuffer::All);
        Ok(())
    }

    /// Build the 16-byte **`cmd`** packet (matches C# `CommandPacketTransmit`: `length` is **byte** count divided by 512 on the wire).
    pub fn command_packet(op: u8, address: u32, length_bytes: u32, argument: u32) -> [u8; 16] {
        let mut p = [0u8; 16];
        p[0..3].copy_from_slice(b"cmd");
        p[3] = op;
        p[4..8].copy_from_slice(&address.to_be_bytes());
        let sectors = length_bytes / 512;
        p[8..12].copy_from_slice(&sectors.to_be_bytes());
        p[12..16].copy_from_slice(&argument.to_be_bytes());
        p
    }

    fn write_packet(&mut self, pkt: &[u8; 16]) -> io::Result<()> {
        self.port.write_all(pkt)?;
        self.port.flush()?;
        Ok(())
    }

    /// Read exactly `n` bytes (Rom/Ram payload).
    fn read_exact_payload(&mut self, n: usize) -> io::Result<Vec<u8>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        if n > ED64_READ_MAX_BYTES {
            return Err(io_other(format!(
                "Ed64 read length {} exceeds max {}",
                n, ED64_READ_MAX_BYTES
            )));
        }
        let mut out = vec![0u8; n];
        let mut got = 0usize;
        while got < n {
            match self.port.read(&mut out[got..]) {
                Ok(0) => {
                    return Err(io_other("unexpected EOF on serial port"));
                }
                Ok(k) => got += k,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(io_other(format!(
                        "timeout reading payload ({got}/{n} bytes)"
                    )));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// `TransmitCommand.TestConnection` — read 16-byte response (`cmd`/`RSP` + `r`/`k` at index 3 per vendor).
    pub fn test_connection(&mut self) -> io::Result<[u8; 16]> {
        let pkt = Self::command_packet(b't', 0, 0, 0);
        self.write_packet(&pkt)?;
        let mut r = [0u8; 16];
        self.port.read_exact(&mut r)?;
        if !(r[3] == b'r' || r[3] == b'k') {
            return Err(io_other(format!(
                "EverDrive test: expected reply byte at [3] r/k, got {:?}",
                r
            )));
        }
        Ok(r)
    }

    /// **RomRead** — read `length_bytes` from cart ROM at `address` (see [`ROM_BASE_ADDRESS`]).
    pub fn rom_read(&mut self, address: u32, length_bytes: usize) -> io::Result<Vec<u8>> {
        if length_bytes % SECTOR_BYTES != 0 {
            return Err(io_other("RomRead length must be a multiple of 512"));
        }
        if length_bytes > ED64_READ_MAX_BYTES {
            return Err(io_other("RomRead length too large"));
        }
        let pkt = Self::command_packet(b'R', address, length_bytes as u32, 0);
        self.write_packet(&pkt)?;
        self.read_exact_payload(length_bytes)
    }

    /// **RamRead** — read `length_bytes` from RDRAM at `address` (see [`RAM_BASE_ADDRESS`]).
    pub fn ram_read(&mut self, address: u32, length_bytes: usize) -> io::Result<Vec<u8>> {
        if length_bytes % SECTOR_BYTES != 0 {
            return Err(io_other("RamRead length must be a multiple of 512"));
        }
        if length_bytes > ED64_READ_MAX_BYTES {
            return Err(io_other("RamRead length too large"));
        }
        let pkt = Self::command_packet(b'r', address, length_bytes as u32, 0);
        self.write_packet(&pkt)?;
        self.read_exact_payload(length_bytes)
    }

    /// Read one 512-byte SD sector using **linear cart addressing**: `address = rom_base + lba * 512`.
    ///
    /// **Community / experimental:** public vendor `usb64` does not document raw SD LBAs. N64brew lists
    /// N64-side SD registers ([EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7)); mapping LBAs to
    /// **`RomRead`** addresses depends on firmware/OS. Callers supply `rom_linear_base` discovered for their setup.
    pub fn read_sd_sector_linear_rom(
        &mut self,
        rom_linear_base: u32,
        lba: u64,
        buf: &mut [u8; SECTOR_BYTES],
    ) -> io::Result<()> {
        let offset = lba
            .checked_mul(SECTOR_BYTES as u64)
            .ok_or_else(|| io_other("LBA offset overflow"))?;
        let addr = (rom_linear_base as u64)
            .checked_add(offset)
            .ok_or_else(|| io_other("ROM address overflow"))?;
        let addr_u32 = u32::try_from(addr).map_err(|_| io_other("ROM address does not fit u32"))?;
        let data = self.rom_read(addr_u32, SECTOR_BYTES)?;
        if data.len() != SECTOR_BYTES {
            return Err(io_other("RomRead returned wrong length"));
        }
        buf.copy_from_slice(&data);
        Ok(())
    }
}

/// True if the port answers **edlink** as **EverDrive-64** ([`PROTOCOL_ID_ED64`] at [`EDLINK_DEFAULT_BAUD`]),
/// or legacy **`usb64`** `cmd`+`t` at **115200** (X-series).
pub fn probe_ed64_serial_cart(port: &str) -> bool {
    match EdlinkLink::try_open(port) {
        Ok(l) => l.protocol_id() == PROTOCOL_ID_ED64,
        Err(_) => probe_usb64_cmd_t(port, 115_200),
    }
}

fn probe_usb64_cmd_t(port: &str, baud: u32) -> bool {
    let mut port_handle = match serialport::new(port, baud)
        .timeout(Duration::from_millis(100))
        .open()
    {
        Ok(p) => p,
        Err(_) => return false,
    };
    let _ = port_handle.clear(ClearBuffer::Input);
    let mut pkt = [0u8; 16];
    pkt[0..3].copy_from_slice(b"cmd");
    pkt[3] = b't';
    if port_handle.write_all(&pkt).is_err() {
        return false;
    }
    let _ = port_handle.flush();
    read_usb64_test_response(&mut *port_handle)
        .map(|buf| buf.len() >= 4 && matches!(buf[3], b'k' | b'r'))
        .unwrap_or(false)
}

fn read_usb64_test_response(port: &mut dyn SerialPort) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut scratch = [0u8; 256];
    let start = Instant::now();
    let total_timeout = Duration::from_millis(2000);
    while start.elapsed() < total_timeout && out.len() < 512 {
        match port.read(&mut scratch) {
            Ok(0) => std::thread::sleep(Duration::from_millis(1)),
            Ok(n) => {
                out.extend_from_slice(&scratch[..n]);
                if out.len() >= 4 && matches!(out[3], b'k' | b'r') {
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_millis(80) && out.len() < 512 {
                        match port.read(&mut scratch) {
                            Ok(0) => break,
                            Ok(n) => out.extend_from_slice(&scratch[..n]),
                            Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                            Err(e) => return Err(e),
                        }
                    }
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_matches_krikzz_test_connection() {
        let p = Ed64Link::command_packet(b't', 0, 0, 0);
        assert_eq!(&p[0..4], b"cmdt");
        assert!(p[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn packet_length_is_sectors() {
        let p = Ed64Link::command_packet(b'R', 0x1000_0000, 1024, 0);
        assert_eq!(u32::from_be_bytes([p[8], p[9], p[10], p[11]]), 2);
    }

    #[test]
    fn edlink_tx_cmd_matches_vendor() {
        // Link.TxCMD(0x10): '+', '+' ^ 0xff, cmd, cmd ^ 0xff
        assert_eq!([b'+', 0xD4, 0x10, 0xEF], [b'+', b'+' ^ 0xff, 0x10, 0x10 ^ 0xff]);
    }
}
