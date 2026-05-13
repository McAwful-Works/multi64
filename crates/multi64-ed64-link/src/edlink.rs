//! Krikzz **[edlink](https://github.com/krikzz/edlink)** Gen3 serial link (`Device/Link.cs`, `Device/DeviceIO_V2.cs`,
//! `DEV_ED64/DeviceIO.cs`): `++`-framed commands, default **921600** baud, **EPO** `MemRD` as FCI reads.
//!
//! This matches the vendor tool path for **EverDrive-64 PRO/CORE** (protocol id **0x07**). Legacy **X-series `usb64`**
//! (`cmd` + `RomRead`) remains in [`crate::Ed64Link`].

use serialport::{ClearBuffer, SerialPort};
use std::io;
use std::io::Write;
use std::time::Duration;

/// Default serial speed from edlink `Link.OpenConnection` (`921600`).
pub const EDLINK_DEFAULT_BAUD: u32 = 921_600;

/// `Edlink.DEV_ED64.DeviceIO.PROTOCOL_ID` — Gen3 device family for N64 EverDrive in vendor sources.
pub const PROTOCOL_ID_ED64: u8 = 0x07;

/// `Edlink.DEV_ED64.DeviceIO` — `ADDR_FCI_SYS` (FCI “system” window). Same numeric value as X-series `usb64`
/// [`crate::ROM_BASE_ADDRESS`]; vendor maps **MemRD** (EPO **FCI**) over this address space.
pub const ADDR_FCI_SYS: u32 = 0x1000_0000;

const STATUS_KEY: u8 = 0x5A;
const CMD_STATUS: u8 = 0x10;
const CMD_STATUS2: u8 = 0x40;
const PROTOCOL_ID_MEGA: u8 = 0x05;
const PROTOCOL_ID_N8: u8 = 0x06;

const CMD_EPO: u8 = 0x81;
const EPO_SCMD_XFER: u8 = 0x10;

#[repr(u8)]
#[derive(Clone, Copy)]
enum EpoType {
    Link = 0x10,
    Fci = 0x13,
}

/// Gen3 EverDrive link (edlink `Link` + `DeviceIO_V2` EPO read path).
pub struct EdlinkLink {
    port: Box<dyn SerialPort>,
    protocol_id: u8,
    device_id: u8,
}

impl EdlinkLink {
    /// Open port at [`EDLINK_DEFAULT_BAUD`], send vendor cold-start flush, run Gen3 status handshake.
    pub fn try_open(port_name: &str) -> io::Result<Self> {
        Self::try_open_with_baud(port_name, EDLINK_DEFAULT_BAUD)
    }

    pub fn try_open_with_baud(port_name: &str, baud: u32) -> io::Result<Self> {
        let mut port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|e| io::Error::other(e.to_string()))?;
        port.write_all(&[0u8; 64 + 2])?;
        port.flush()?;
        flush_rx_best_effort(&mut *port)?;

        let (protocol_id, device_id) = get_device_config(&mut *port)?;

        // Match edlink `OpenConnection`: relax timeouts after handshake.
        port.set_timeout(Duration::from_millis(2000))?;

        let mut s = Self {
            port,
            protocol_id,
            device_id,
        };
        s.get_id(0)?;
        Ok(s)
    }

    pub fn protocol_id(&self) -> u8 {
        self.protocol_id
    }

    pub fn device_id(&self) -> u8 {
        self.device_id
    }

    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.port.set_timeout(t).map_err(io::Error::other)
    }

    pub fn flush_serial(&mut self) -> io::Result<()> {
        self.port.flush()
    }

    pub fn clear_buffers(&mut self) -> io::Result<()> {
        let _ = self.port.clear(ClearBuffer::All);
        Ok(())
    }

    /// Read `buf.len()` bytes from cart **FCI** at `addr` via EPO XFER (edlink `DeviceIO_V2.MemRD` / `EpoRD`).
    pub fn fci_read(&mut self, addr: u32, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.tx_cmd_scmd(CMD_EPO, EPO_SCMD_XFER)?;
        self.tx32_be(addr)?;
        self.tx32_be(0)?;
        self.tx32_be(buf.len() as u32)?;
        self.port.write_all(&[EpoType::Fci as u8, EpoType::Link as u8])?;
        self.port.write_all(&0u16.to_be_bytes())?;
        self.port.write_all(&[0u8])?; // ack
        self.port.flush()?;

        read_exact_port(&mut *self.port, buf)?;
        self.check_status()
    }

    fn tx_cmd(&mut self, cmd: u8) -> io::Result<()> {
        let pkt = [b'+', b'+' ^ 0xff, cmd, cmd ^ 0xff];
        self.port.write_all(&pkt)?;
        self.port.flush()?;
        Ok(())
    }

    fn tx_cmd_scmd(&mut self, cmd: u8, scmd: u8) -> io::Result<()> {
        let pkt = [b'+', b'+' ^ 0xff, cmd, cmd ^ 0xff, scmd];
        self.port.write_all(&pkt)?;
        self.port.flush()?;
        Ok(())
    }

    fn tx32_be(&mut self, v: u32) -> io::Result<()> {
        self.port.write_all(&v.to_be_bytes())?;
        Ok(())
    }

    /// Vendor `Link.GetID` / `DeviceIO.GetStatus`: returns status byte (`id[3]`, `0` = OK).
    fn get_id(&mut self, timeout_ms: u32) -> io::Result<[u8; 4]> {
        self.tx_cmd(CMD_STATUS)?;
        if timeout_ms != 0 {
            let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms as u64);
            while self.port.bytes_to_read()? < 2 {
                if std::time::Instant::now() > deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "edlink: status read timeout (waiting for 2 bytes)",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut id = [0u8; 4];
        read_exact_port(&mut *self.port, &mut id)?;
        if id[0] != STATUS_KEY || id[1] != self.protocol_id {
            return Err(io::Error::other(format!(
                "edlink: unexpected status frame {:02x?}",
                id
            )));
        }
        if id[2] != self.device_id {
            return Err(io::Error::other(format!(
                "edlink: device id mismatch (expected 0x{:02x}, got 0x{:02x})",
                self.device_id, id[2]
            )));
        }
        Ok(id)
    }

    fn check_status(&mut self) -> io::Result<()> {
        let st = self.get_id(8000)?[3];
        if st != 0 {
            return Err(io::Error::other(format!(
                "edlink: device reported operation status 0x{st:02x}"
            )));
        }
        Ok(())
    }
}

fn flush_rx_best_effort(port: &mut dyn SerialPort) -> io::Result<()> {
    let mut scratch = [0u8; 512];
    for _ in 0..512 {
        let n = port.bytes_to_read()?;
        if n == 0 {
            break;
        }
        let take = (n as usize).min(scratch.len());
        match port.read(&mut scratch[..take]) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn read_exact_port(port: &mut dyn SerialPort, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match port.read(buf) {
            Ok(0) => {
                return Err(io::Error::other(
                    "edlink: unexpected EOF while reading payload",
                ));
            }
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "edlink: timed out reading payload",
                ));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn get_device_config(port: &mut dyn SerialPort) -> io::Result<(u8, u8)> {
    tx_cmd_port(port, CMD_STATUS2)?;
    tx_cmd_port(port, CMD_STATUS)?;
    let mut id = [0u8; 4];
    read_exact_port(port, &mut id[..2])?;
    if id[0] != STATUS_KEY {
        return Err(io::Error::other(format!(
            "edlink: unexpected status key 0x{:02x} (expected 0x5A)",
            id[0]
        )));
    }
    read_exact_port(port, &mut id[2..4])?;
    if id[1] == PROTOCOL_ID_MEGA || id[1] == PROTOCOL_ID_N8 {
        let mut drop = [0u8; 2];
        read_exact_port(port, &mut drop)?;
        return Err(io::Error::other(
            "edlink: MEGA/N8 protocol on this port (not EverDrive-64)",
        ));
    }
    Ok((id[1], id[2]))
}

fn tx_cmd_port(port: &mut dyn SerialPort, cmd: u8) -> io::Result<()> {
    let pkt = [b'+', b'+' ^ 0xff, cmd, cmd ^ 0xff];
    port.write_all(&pkt)?;
    port.flush()?;
    Ok(())
}
