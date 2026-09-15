//! Commands to an EverDrive-64 PRO: handshake, status, file system, cart memory and the FIFO.

use crate::transport::Transport;
use crate::wire::{
    self, cmd_frame, epo_xfer, file_info_head, fs, scmd_frame, string_field, Endpoint, FileInfo,
    ACK_BLOCK, CMD_EPO, CMD_FS, CMD_NRESP, CMD_STATUS, CMD_STATUS2, DEVICE_ID_ED64_PRO,
    EPO_SCMD_XFER, FIFO_ADDR, MAX_WRITE_BLOCK, PROTOCOL_ID, PROTOCOL_ID_MEGA, PROTOCOL_ID_N8,
    STATUS_KEY, STATUS_KEY_LEGACY, WAKE_ZEROS,
};
use std::fmt;
use std::io;
use std::time::Duration;

/// Read/write timeout while probing a port (`Link.OpenConnection`).
const OPEN_TIMEOUT: Duration = Duration::from_millis(200);
/// Timeout once the cart has identified itself (`Link.OpenConnection`).
const OPERATION_TIMEOUT: Duration = Duration::from_millis(2000);
/// Longest name requested per directory record; the ed64-pro-pub sample uses a 1024-byte buffer.
const MAX_NAME_LEN: u16 = 1023;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The first status byte was neither [`STATUS_KEY`] nor a legacy key.
    UnexpectedStatusKey(u8),
    /// Older Mega EverDrive PRO / N8 PRO firmware answered: a cart for another console.
    OtherConsole {
        protocol_id: u8,
    },
    /// A Gen3 cart answered, but not the N64 protocol.
    WrongProtocol(u8),
    /// The N64 protocol answered, but not an EverDrive-64 PRO.
    WrongDevice(u8),
    /// The cart reported a non-zero status; `detail` is its `CMD_NRESP` elaboration when fetched.
    Operation {
        status: u8,
        detail: Option<u8>,
    },
    /// A write block was not acknowledged with `0`.
    BadAck(u8),
    /// A path does not fit the protocol's `u16` length field.
    PathTooLong(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "EverDrive-64 PRO link I/O: {e}"),
            Error::UnexpectedStatusKey(k) => write!(f, "unexpected status key 0x{k:02X}"),
            Error::OtherConsole { protocol_id } => write!(
                f,
                "an EverDrive for another console answered (protocol 0x{protocol_id:02X})"
            ),
            Error::WrongProtocol(p) => write!(
                f,
                "cart speaks protocol 0x{p:02X}, not the N64 protocol 0x{PROTOCOL_ID:02X}"
            ),
            Error::WrongDevice(d) => write!(
                f,
                "device 0x{d:02X} is not an EverDrive-64 PRO (0x{DEVICE_ID_ED64_PRO:02X})"
            ),
            Error::Operation {
                status,
                detail: Some(d),
            } => {
                write!(f, "cart operation error {status:02X}.{d:02X}")
            }
            Error::Operation {
                status,
                detail: None,
            } => write!(f, "cart operation error {status:02X}"),
            Error::BadAck(a) => write!(f, "write block not acknowledged (got 0x{a:02X})"),
            Error::PathTooLong(n) => write!(f, "path of {n} bytes exceeds the 65535-byte limit"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// What a status reply identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub protocol_id: u8,
    pub device_id: u8,
    /// Result of the previous command: `0` on success.
    pub status: u8,
}

/// An EverDrive-64 PRO that has answered the handshake.
pub struct Ed64Pro<T: Transport> {
    io: T,
}

impl Ed64Pro<Box<dyn serialport::SerialPort>> {
    /// Open `port_name` at [`wire::BAUD`] and run the handshake.
    pub fn open(port_name: &str) -> Result<Self> {
        let port = serialport::new(port_name, wire::BAUD)
            .timeout(OPEN_TIMEOUT)
            .open()
            .map_err(|e| Error::Io(io::Error::other(e)))?;
        Self::connect(port)
    }
}

impl<T: Transport> Ed64Pro<T> {
    /// Run edlink's connection sequence on an already-open transport.
    ///
    /// Mirrors `Link.OpenConnection`: 66 zero bytes, discard input, then `GetDeviceConfig`
    /// (`CMD_STATUS2` + `CMD_STATUS`, 4 bytes back from a Gen3 cart) and `GetID` (`CMD_STATUS`,
    /// validated against the PRO's protocol and device IDs).
    pub fn connect(mut io: T) -> Result<Self> {
        io.set_timeout(OPEN_TIMEOUT)?;
        io.write_all(&[0u8; WAKE_ZEROS])?;
        io.flush()?;
        io.clear_input()?;

        let mut probe = Vec::with_capacity(8);
        probe.extend_from_slice(&cmd_frame(CMD_STATUS2));
        probe.extend_from_slice(&cmd_frame(CMD_STATUS));
        io.write_all(&probe)?;
        io.flush()?;

        let mut head = [0u8; 2];
        io.read_exact(&mut head)?;
        if head[0] != STATUS_KEY {
            if head[0] == STATUS_KEY_LEGACY || head[1] == STATUS_KEY_LEGACY {
                return Err(Error::OtherConsole {
                    protocol_id: head[1],
                });
            }
            return Err(Error::UnexpectedStatusKey(head[0]));
        }
        let mut tail = [0u8; 2];
        io.read_exact(&mut tail)?;
        if head[1] == PROTOCOL_ID_MEGA || head[1] == PROTOCOL_ID_N8 {
            // Gen2 answers both commands; consume the second reply so the error is clean.
            let mut rest = [0u8; 2];
            let _ = io.read_exact(&mut rest);
            return Err(Error::OtherConsole {
                protocol_id: head[1],
            });
        }

        let mut dev = Self { io };
        let id = dev.identity()?;
        if id.device_id != DEVICE_ID_ED64_PRO {
            return Err(Error::WrongDevice(id.device_id));
        }
        dev.io.set_timeout(OPERATION_TIMEOUT)?;
        tracing::debug!(
            target: "multi64_ed64pro_link",
            protocol = id.protocol_id,
            device = id.device_id,
            "EverDrive-64 PRO handshake complete"
        );
        Ok(dev)
    }

    /// Send `CMD_STATUS` and validate the key and protocol (`DeviceIO.GetStatus`).
    pub fn identity(&mut self) -> Result<Identity> {
        self.send(&cmd_frame(CMD_STATUS))?;
        let mut id = [0u8; 4];
        self.io.read_exact(&mut id)?;
        if id[0] != STATUS_KEY {
            return Err(Error::UnexpectedStatusKey(id[0]));
        }
        if id[1] != PROTOCOL_ID {
            return Err(Error::WrongProtocol(id[1]));
        }
        Ok(Identity {
            protocol_id: id[1],
            device_id: id[2],
            status: id[3],
        })
    }

    /// Turn a non-zero status into an error, fetching its `CMD_NRESP` detail (`CheckStatus`).
    fn check_status(&mut self) -> Result<()> {
        let status = self.identity()?.status;
        if status == 0 {
            return Ok(());
        }
        let mut req = Vec::with_capacity(5);
        req.extend_from_slice(&cmd_frame(CMD_NRESP));
        req.push(status);
        self.send(&req)?;
        let mut detail = [0u8; 1];
        self.io.read_exact(&mut detail)?;
        Err(Error::Operation {
            status,
            detail: Some(detail[0]),
        })
    }

    /// Initialise the SD file system (`FS_SCMD_INIT`).
    pub fn fs_init(&mut self) -> Result<()> {
        self.send(&scmd_frame(CMD_FS, fs::INIT))?;
        self.check_status()
    }

    /// Open a file. Paths are root-relative with no `sd:` prefix, e.g. `"ed64/sysdata/config.ini"`.
    pub fn file_open(&mut self, path: &str, mode: u8) -> Result<()> {
        let mut req = scmd_frame(CMD_FS, fs::FILE_OPEN).to_vec();
        req.push(mode);
        req.extend(Self::path_field(path)?);
        self.send(&req)?;
        self.check_status()
    }

    pub fn file_close(&mut self) -> Result<()> {
        self.send(&scmd_frame(CMD_FS, fs::FILE_CLOSE))?;
        self.check_status()
    }

    /// Bytes left to read in the open file: low `u32`, then high `u32` (`FileAvailable`).
    pub fn file_available(&mut self) -> Result<u64> {
        self.send(&scmd_frame(CMD_FS, fs::AVAILABLE))?;
        let mut b = [0u8; 8];
        self.io.read_exact(&mut b)?;
        let lo = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
        let hi = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as u64;
        Ok(lo | (hi << 32))
    }

    /// Move the open file's position (`FS_SCMD_FPTR`; console-side source only).
    pub fn file_seek(&mut self, position: u32) -> Result<()> {
        let mut req = scmd_frame(CMD_FS, fs::FILE_SEEK).to_vec();
        req.extend_from_slice(&position.to_be_bytes());
        self.send(&req)?;
        self.check_status()
    }

    /// Read `buf.len()` bytes from the open file (`FileRead`: an unacknowledged `EPO` to the link).
    pub fn file_read(&mut self, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let len = Self::transfer_len(buf.len())?;
        self.epo(Endpoint::File, Endpoint::Link, 0, 0, len)?;
        self.io.read_exact(buf)?;
        self.check_status()
    }

    /// Write to the open file in acknowledged blocks (`FileWrite`).
    pub fn file_write(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let len = Self::transfer_len(data.len())?;
        self.epo(Endpoint::LinkAck, Endpoint::File, 0, 0, len)?;
        self.send_acked(data)?;
        self.check_status()
    }

    /// Size, timestamps and attributes of one path (`FS_SCMD_FINFO`; console-side source only).
    pub fn file_info(&mut self, path: &str) -> Result<FileInfo> {
        let mut req = scmd_frame(CMD_FS, fs::FILE_INFO).to_vec();
        req.extend(Self::path_field(path)?);
        self.send(&req)?;
        self.read_record()
    }

    /// List a directory (`FS_SCMD_DIR_LD`, `DIR_SIZE`, then one `DIR_GET` per entry).
    ///
    /// Entries are fetched one at a time, as the ed64-pro-pub sample does: a bulk request must not
    /// leave more than 2048 unread bytes in the cart's FIFO. `""` is the SD root.
    pub fn dir_list(&mut self, path: &str, options: u8) -> Result<Vec<FileInfo>> {
        let mut req = scmd_frame(CMD_FS, fs::DIR_LOAD).to_vec();
        req.push(options);
        req.extend(Self::path_field(path)?);
        self.send(&req)?;
        self.check_status()?;

        self.send(&scmd_frame(CMD_FS, fs::DIR_SIZE))?;
        let mut size = [0u8; 2];
        self.io.read_exact(&mut size)?;
        let count = u16::from_be_bytes(size);

        let mut entries = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut req = scmd_frame(CMD_FS, fs::DIR_GET).to_vec();
            req.extend_from_slice(&index.to_be_bytes());
            req.extend_from_slice(&1u16.to_be_bytes());
            req.extend_from_slice(&MAX_NAME_LEN.to_be_bytes());
            self.send(&req)?;
            entries.push(self.read_record()?);
        }
        Ok(entries)
    }

    /// Create a directory (`FS_SCMD_DIR_MK`; console-side source only).
    pub fn dir_make(&mut self, path: &str) -> Result<()> {
        self.path_command(fs::DIR_MAKE, path)?;
        self.check_status()
    }

    /// Delete a file or empty directory (`FS_SCMD_DEL`; console-side source only).
    pub fn delete(&mut self, path: &str) -> Result<()> {
        self.path_command(fs::DELETE, path)?;
        self.check_status()
    }

    /// Whether a file exists: status `0` from `FS_SCMD_FTEST`. Console-side source only, and what a
    /// non-zero status distinguishes (absent vs. error) is unverified.
    pub fn file_exists(&mut self, path: &str) -> Result<bool> {
        self.path_command(fs::FILE_TEST, path)?;
        Ok(self.identity()?.status == 0)
    }

    /// Whether a directory exists: status `0` from `FS_SCMD_DTEST`. Same caveats as
    /// [`Ed64Pro::file_exists`].
    pub fn dir_exists(&mut self, path: &str) -> Result<bool> {
        self.path_command(fs::DIR_TEST, path)?;
        Ok(self.identity()?.status == 0)
    }

    /// Read cart memory (`MemRD`: `EPO` from `FCI`).
    pub fn mem_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let len = Self::transfer_len(buf.len())?;
        self.epo(Endpoint::Memory, Endpoint::Link, addr, 0, len)?;
        self.io.read_exact(buf)?;
        self.check_status()
    }

    /// Write cart memory (`MemWR`). Unacknowledged, and edlink reads no status afterwards.
    pub fn mem_write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let len = Self::transfer_len(data.len())?;
        self.epo(Endpoint::Link, Endpoint::Memory, 0, addr, len)?;
        self.send(data)
    }

    /// Queue bytes for a running ROM to read from its FIFO (`FifoWR`).
    pub fn fifo_write(&mut self, data: &[u8]) -> Result<()> {
        self.mem_write(FIFO_ADDR, data)
    }

    /// Read whatever a running ROM has sent with `ed_usb_wr`, up to `buf.len()` (`UsbRD`).
    ///
    /// That data arrives raw on the same serial stream as command replies, so do not interleave
    /// this with commands while a ROM is streaming.
    ///
    /// Like [`io::Read::read`], a read that fails after taking some bytes returns those bytes, and
    /// the failure, if it persists, surfaces on the next call. Only a failure before the first byte
    /// is an error, so ROM output already taken off the port is never thrown away.
    pub fn usb_read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let want = (self.io.bytes_to_read()? as usize).min(buf.len());
        let mut got = 0;
        while got < want {
            match self.io.read(&mut buf[got..want]) {
                Ok(0) if got == 0 => {
                    return Err(Error::Io(io::ErrorKind::UnexpectedEof.into()));
                }
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if got == 0 => return Err(e.into()),
                Err(e) => {
                    tracing::debug!(
                        target: "multi64_ed64pro_link",
                        error = %e,
                        kept = got,
                        wanted = want,
                        "USB read failed partway; returning the bytes already read"
                    );
                    break;
                }
            }
        }
        Ok(got)
    }

    /// Drop whatever has already arrived and not been read (edlink's `FlushPort`).
    ///
    /// For a host that is about to stream from a running ROM: anything waiting predates it.
    pub fn discard_input(&mut self) -> Result<()> {
        self.io.clear_input()?;
        Ok(())
    }

    /// The transport itself, e.g. to queue a fake cart's output in a test.
    ///
    /// Bytes written through this bypass the protocol entirely.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.io
    }

    pub fn into_inner(self) -> T {
        self.io
    }

    // -- helpers ------------------------------------------------------------------------------

    /// Write like `Link.TxData`: blocks of at most 4096 bytes, and never a single 512-byte write —
    /// edlink splits that into 256 + 256 ("512 does not work well").
    fn send(&mut self, data: &[u8]) -> Result<()> {
        let mut rest = data;
        while !rest.is_empty() {
            let mut block = rest.len().min(MAX_WRITE_BLOCK);
            if block == 512 {
                block = 256;
            }
            self.io.write_all(&rest[..block])?;
            rest = &rest[block..];
        }
        self.io.flush()?;
        Ok(())
    }

    /// `Link.TxDataACK`: before each block of up to 1024 bytes the cart sends `0`.
    fn send_acked(&mut self, data: &[u8]) -> Result<()> {
        for block in data.chunks(ACK_BLOCK) {
            let mut ack = [0u8; 1];
            self.io.read_exact(&mut ack)?;
            if ack[0] != 0 {
                return Err(Error::BadAck(ack[0]));
            }
            self.send(block)?;
        }
        Ok(())
    }

    fn epo(
        &mut self,
        src: Endpoint,
        dst: Endpoint,
        src_addr: u32,
        dst_addr: u32,
        len: u32,
    ) -> Result<()> {
        let mut req = scmd_frame(CMD_EPO, EPO_SCMD_XFER).to_vec();
        req.extend_from_slice(&epo_xfer(src, dst, src_addr, dst_addr, len));
        self.send(&req)
    }

    fn path_command(&mut self, scmd: u8, path: &str) -> Result<()> {
        let mut req = scmd_frame(CMD_FS, scmd).to_vec();
        req.extend(Self::path_field(path)?);
        self.send(&req)
    }

    /// A result byte, then (on `0`) the 9-byte head and a length-prefixed name
    /// (`ed_fs_rx_next_rec` / `ed_fs_file_info`).
    fn read_record(&mut self) -> Result<FileInfo> {
        let mut resp = [0u8; 1];
        self.io.read_exact(&mut resp)?;
        if resp[0] != 0 {
            return Err(Error::Operation {
                status: resp[0],
                detail: None,
            });
        }
        let mut head = [0u8; 9];
        self.io.read_exact(&mut head)?;
        let (size, date, time, attributes) = file_info_head(&head);
        let mut len = [0u8; 2];
        self.io.read_exact(&mut len)?;
        let mut name = vec![0u8; u16::from_be_bytes(len) as usize];
        self.io.read_exact(&mut name)?;
        Ok(FileInfo {
            size,
            date,
            time,
            attributes,
            name: String::from_utf8_lossy(&name).into_owned(),
        })
    }

    fn path_field(path: &str) -> Result<Vec<u8>> {
        string_field(path).ok_or(Error::PathTooLong(path.len()))
    }

    fn transfer_len(len: usize) -> Result<u32> {
        u32::try_from(len).map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "transfer longer than 4 GiB",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::Scripted;
    use crate::wire::{dir_option, open_mode, ATTR_DIR};

    const OK: [u8; 4] = [STATUS_KEY, PROTOCOL_ID, DEVICE_ID_ED64_PRO, 0x00];

    fn status(code: u8) -> [u8; 4] {
        [STATUS_KEY, PROTOCOL_ID, DEVICE_ID_ED64_PRO, code]
    }

    fn handshake_bytes() -> Vec<u8> {
        let mut w = vec![0u8; WAKE_ZEROS];
        w.extend_from_slice(&cmd_frame(CMD_STATUS2));
        w.extend_from_slice(&cmd_frame(CMD_STATUS));
        w.extend_from_slice(&cmd_frame(CMD_STATUS));
        w
    }

    /// A connected device whose script then continues with `after`.
    fn connected(after: &[u8]) -> Ed64Pro<Scripted> {
        let mut replies = OK.to_vec();
        replies.extend_from_slice(&OK);
        replies.extend_from_slice(after);
        Ed64Pro::connect(Scripted::replying(&replies)).expect("handshake")
    }

    /// Bytes written after the handshake.
    fn sent(dev: Ed64Pro<Scripted>) -> (Vec<u8>, Scripted) {
        let io = dev.into_inner();
        let hs = handshake_bytes().len();
        (io.written[hs..].to_vec(), io)
    }

    #[test]
    fn handshake_sends_wake_zeros_then_status2_status_status() {
        let dev = connected(&[]);
        let io = dev.into_inner();
        assert_eq!(io.written, handshake_bytes());
        assert_eq!(io.timeouts, [OPEN_TIMEOUT, OPERATION_TIMEOUT]);
        assert_eq!(io.unread(), 0);
    }

    #[test]
    fn handshake_rejects_other_devices_and_consoles() {
        let wrong_device = [STATUS_KEY, PROTOCOL_ID, 0x28, 0x00];
        let err = Ed64Pro::connect(Scripted::replying(&[OK, wrong_device].concat()))
            .err()
            .unwrap();
        assert!(matches!(err, Error::WrongDevice(0x28)), "{err}");

        let n8_gen2 = [STATUS_KEY, PROTOCOL_ID_N8, 0x17, 0x00, 0x00, 0x00];
        let err = Ed64Pro::connect(Scripted::replying(&n8_gen2))
            .err()
            .unwrap();
        assert!(
            matches!(err, Error::OtherConsole { protocol_id: 0x06 }),
            "{err}"
        );

        let legacy = [STATUS_KEY_LEGACY, 0x00];
        let err = Ed64Pro::connect(Scripted::replying(&legacy)).err().unwrap();
        assert!(matches!(err, Error::OtherConsole { .. }), "{err}");

        let noise = [0x00, 0x00];
        let err = Ed64Pro::connect(Scripted::replying(&noise)).err().unwrap();
        assert!(matches!(err, Error::UnexpectedStatusKey(0x00)), "{err}");
    }

    #[test]
    fn handshake_times_out_on_a_silent_port() {
        let err = Ed64Pro::connect(Scripted::default()).err().unwrap();
        assert!(
            matches!(err, Error::Io(ref e) if e.kind() == io::ErrorKind::TimedOut),
            "{err}"
        );
    }

    #[test]
    fn file_open_sends_mode_and_path_then_checks_status() {
        let mut dev = connected(&OK);
        dev.file_open("ed64/a.z64", open_mode::READ).unwrap();
        let (w, _) = sent(dev);
        let mut expect = scmd_frame(CMD_FS, fs::FILE_OPEN).to_vec();
        expect.push(open_mode::READ);
        expect.extend(string_field("ed64/a.z64").unwrap());
        expect.extend_from_slice(&cmd_frame(CMD_STATUS));
        assert_eq!(w, expect);
    }

    #[test]
    fn nonzero_status_fetches_the_nresp_detail() {
        let mut dev = connected(&[&status(0x42)[..], &[0x07]].concat());
        let err = dev.file_close().unwrap_err();
        assert!(
            matches!(
                err,
                Error::Operation {
                    status: 0x42,
                    detail: Some(0x07)
                }
            ),
            "{err}"
        );
        let (w, _) = sent(dev);
        let mut expect = scmd_frame(CMD_FS, fs::FILE_CLOSE).to_vec();
        expect.extend_from_slice(&cmd_frame(CMD_STATUS));
        expect.extend_from_slice(&cmd_frame(CMD_NRESP));
        expect.push(0x42);
        assert_eq!(w, expect);
    }

    #[test]
    fn file_available_combines_low_then_high_words() {
        let mut dev = connected(&[0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(dev.file_available().unwrap(), 0x1_0000_1000);
    }

    #[test]
    fn file_read_is_an_unacked_epo_then_status() {
        let mut dev = connected(&[&b"DATA"[..], &OK].concat());
        let mut buf = [0u8; 4];
        dev.file_read(&mut buf).unwrap();
        assert_eq!(&buf, b"DATA");
        let (w, _) = sent(dev);
        let mut expect = scmd_frame(CMD_EPO, EPO_SCMD_XFER).to_vec();
        expect.extend_from_slice(&epo_xfer(Endpoint::File, Endpoint::Link, 0, 0, 4));
        expect.extend_from_slice(&cmd_frame(CMD_STATUS));
        assert_eq!(w, expect);
    }

    #[test]
    fn file_write_waits_for_an_ack_before_each_1024_byte_block() {
        let data: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        // Three blocks (1024, 1024, 452), each preceded by an ack, then the status.
        let mut dev = connected(&[&[0u8, 0, 0][..], &OK].concat());
        dev.file_write(&data).unwrap();
        let (w, _) = sent(dev);
        let mut expect = scmd_frame(CMD_EPO, EPO_SCMD_XFER).to_vec();
        expect.extend_from_slice(&epo_xfer(Endpoint::LinkAck, Endpoint::File, 0, 0, 2500));
        expect.extend_from_slice(&data);
        expect.extend_from_slice(&cmd_frame(CMD_STATUS));
        assert_eq!(w, expect);
    }

    #[test]
    fn file_write_stops_on_a_bad_ack() {
        let mut dev = connected(&[0x09]);
        let err = dev.file_write(&[1, 2, 3]).unwrap_err();
        assert!(matches!(err, Error::BadAck(0x09)), "{err}");
    }

    #[test]
    fn dir_list_loads_sizes_then_fetches_one_record_at_a_time() {
        let record = |attr: u8, size: u32, name: &str| {
            let mut r = vec![0x00];
            r.extend_from_slice(&size.to_be_bytes());
            r.extend_from_slice(&0x5A21u16.to_be_bytes());
            r.extend_from_slice(&0x6000u16.to_be_bytes());
            r.push(attr);
            r.extend(string_field(name).unwrap());
            r
        };
        let mut replies = OK.to_vec(); // DIR_LD status
        replies.extend_from_slice(&2u16.to_be_bytes()); // DIR_SIZE
        replies.extend(record(ATTR_DIR, 0, "ed64"));
        replies.extend(record(0x20, 8_388_608, "sm64.z64"));
        let mut dev = connected(&replies);

        let entries = dev.dir_list("", dir_option::SORTED).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir() && entries[0].name == "ed64");
        assert!(!entries[1].is_dir() && entries[1].size == 8_388_608);

        let (w, io) = sent(dev);
        assert_eq!(io.unread(), 0);
        let mut expect = scmd_frame(CMD_FS, fs::DIR_LOAD).to_vec();
        expect.push(dir_option::SORTED);
        expect.extend(string_field("").unwrap());
        expect.extend_from_slice(&cmd_frame(CMD_STATUS));
        expect.extend_from_slice(&scmd_frame(CMD_FS, fs::DIR_SIZE));
        for i in 0..2u16 {
            expect.extend_from_slice(&scmd_frame(CMD_FS, fs::DIR_GET));
            expect.extend_from_slice(&i.to_be_bytes());
            expect.extend_from_slice(&1u16.to_be_bytes());
            expect.extend_from_slice(&MAX_NAME_LEN.to_be_bytes());
        }
        assert_eq!(w, expect);
    }

    #[test]
    fn file_info_reports_a_nonzero_result_byte() {
        let mut dev = connected(&[0x04]);
        let err = dev.file_info("missing").unwrap_err();
        assert!(
            matches!(
                err,
                Error::Operation {
                    status: 0x04,
                    detail: None
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn mem_write_is_unacked_with_no_status_read() {
        let mut dev = connected(&[]);
        dev.fifo_write(b"hi").unwrap();
        let (w, io) = sent(dev);
        let mut expect = scmd_frame(CMD_EPO, EPO_SCMD_XFER).to_vec();
        expect.extend_from_slice(&epo_xfer(Endpoint::Link, Endpoint::Memory, 0, FIFO_ADDR, 2));
        expect.extend_from_slice(b"hi");
        assert_eq!(w, expect);
        assert_eq!(io.unread(), 0);
    }

    #[test]
    fn a_512_byte_write_is_split_like_edlink() {
        let mut dev = connected(&[]);
        dev.mem_write(0, &[0xAA; 512]).unwrap();
        let io = dev.into_inner();
        // The EPO header goes out first (22 bytes), then the payload as 256 + 256, never 512.
        assert!(!io.write_sizes.contains(&512), "{:?}", io.write_sizes);
        assert!(
            io.write_sizes.ends_with(&[256, 256]),
            "{:?}",
            io.write_sizes
        );
    }

    #[test]
    fn exists_checks_read_the_status_without_nresp() {
        let mut dev = connected(&[&OK[..], &status(0x05)].concat());
        assert!(dev.file_exists("ed64/a.z64").unwrap());
        assert!(!dev.dir_exists("nope").unwrap());
    }

    /// A transport over the fake cart that hands out at most 3 bytes per read and, once armed, fails
    /// a single read after `budget` more bytes, as a port that hiccups mid-read would.
    #[cfg(feature = "fake")]
    struct Hiccup {
        inner: crate::fake::FakeEd64Pro,
        budget: Option<usize>,
    }

    #[cfg(feature = "fake")]
    impl io::Read for Hiccup {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut cap = buf.len().min(3);
            if let Some(left) = self.budget {
                if left == 0 {
                    self.budget = None;
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "hiccup"));
                }
                cap = cap.min(left);
            }
            let n = io::Read::read(&mut self.inner, &mut buf[..cap])?;
            if let Some(left) = &mut self.budget {
                *left -= n;
            }
            Ok(n)
        }
    }

    #[cfg(feature = "fake")]
    impl io::Write for Hiccup {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            io::Write::write(&mut self.inner, buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            io::Write::flush(&mut self.inner)
        }
    }

    #[cfg(feature = "fake")]
    impl Transport for Hiccup {
        fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
            self.inner.set_timeout(timeout)
        }
        fn clear_input(&mut self) -> io::Result<()> {
            self.inner.clear_input()
        }
        fn bytes_to_read(&mut self) -> io::Result<u32> {
            self.inner.bytes_to_read()
        }
    }

    /// #167: `usb_read` used `read_exact`, so a read that failed partway threw away the ROM output
    /// it had already taken off the port. It now returns what it got; the rest comes next call.
    #[cfg(feature = "fake")]
    #[test]
    fn usb_read_keeps_what_it_read_before_a_failed_read() {
        let hiccup = Hiccup {
            inner: crate::fake::FakeEd64Pro::new(),
            budget: None,
        };
        let mut dev = Ed64Pro::connect(hiccup).expect("handshake");
        dev.transport_mut().inner.push_usb(b"hello world");
        dev.transport_mut().budget = Some(5);

        let mut buf = [0u8; 16];
        let n = dev
            .usb_read(&mut buf)
            .expect("bytes already read are returned");
        assert_eq!(&buf[..n], b"hello");
        let m = dev.usb_read(&mut buf).expect("the port recovered");
        assert_eq!(&buf[..m], b" world", "nothing was lost across the failure");
    }

    /// A read that fails before taking anything still reports the error.
    #[cfg(feature = "fake")]
    #[test]
    fn usb_read_reports_a_failure_before_any_byte() {
        let hiccup = Hiccup {
            inner: crate::fake::FakeEd64Pro::new(),
            budget: None,
        };
        let mut dev = Ed64Pro::connect(hiccup).expect("handshake");
        dev.transport_mut().inner.push_usb(b"log");
        dev.transport_mut().budget = Some(0);
        let mut buf = [0u8; 16];
        let err = dev.usb_read(&mut buf).unwrap_err();
        assert!(
            matches!(err, Error::Io(ref e) if e.kind() == io::ErrorKind::BrokenPipe),
            "{err}"
        );
        assert_eq!(dev.usb_read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"log");
    }

    #[test]
    fn usb_read_takes_only_what_is_waiting() {
        let mut dev = connected(b"log");
        let mut buf = [0u8; 16];
        assert_eq!(dev.usb_read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"log");
        assert_eq!(dev.usb_read(&mut buf).unwrap(), 0);
    }
}
