//! Byte-level encoding for edlink Gen3, as the EverDrive-64 PRO uses it.
//!
//! Constants are transcribed from `krikzz/edlink` (`Device/Link.cs`, `Device/DeviceIO_V2.cs`,
//! `DEV_ED64/DeviceIO.cs`) and cross-checked against `krikzz/ed64-pro-pub` (`edio/everdrive.c`).
//! Where the two disagree, `docs/spec/ed64-pro-usb-host.md` says which one this follows and why.

/// Serial rate edlink opens every Gen3 cart at (`Link.OpenConnection`).
pub const BAUD: u32 = 921_600;

/// First byte of every status reply.
pub const STATUS_KEY: u8 = 0x5A;
/// Status key of the older Mega / N8 firmware, which is not an N64 cart.
pub(crate) const STATUS_KEY_LEGACY: u8 = 0xA5;

/// Protocol ID of the N64 PRO family (`DEV_ED64/DeviceIO.PROTOCOL_ID`).
pub const PROTOCOL_ID: u8 = 0x07;
/// Device ID of the EverDrive-64 PRO (`DEV_ED64/DeviceIO.DEV_ID_ED64_PRO`, `DEVID_ED64PRO`).
pub const DEVICE_ID_ED64_PRO: u8 = 0x27;
/// Protocol IDs of the Mega EverDrive PRO and EverDrive N8 PRO; a reply carrying either is a Gen2
/// cart for another console.
pub(crate) const PROTOCOL_ID_MEGA: u8 = 0x05;
pub(crate) const PROTOCOL_ID_N8: u8 = 0x06;

pub(crate) const CMD_STATUS: u8 = 0x10;
pub(crate) const CMD_NRESP: u8 = 0x13;
/// Only Gen2 firmware answers this; edlink sends it before `CMD_STATUS` to tell generations apart.
pub(crate) const CMD_STATUS2: u8 = 0x40;
pub(crate) const CMD_FS: u8 = 0x80;
pub(crate) const CMD_EPO: u8 = 0x81;
pub(crate) const EPO_SCMD_XFER: u8 = 0x10;

/// `CMD_FS` subcommands used here (`FS_SCMD_*`). Numbering agrees in both sources.
pub(crate) mod fs {
    pub const INIT: u8 = 0x10;
    pub const DIR_LOAD: u8 = 0x13;
    pub const DIR_SIZE: u8 = 0x14;
    pub const DIR_GET: u8 = 0x16;
    pub const FILE_OPEN: u8 = 0x17;
    pub const FILE_CLOSE: u8 = 0x18;
    pub const FILE_SEEK: u8 = 0x19;
    pub const FILE_INFO: u8 = 0x1A;
    pub const DIR_MAKE: u8 = 0x1C;
    pub const DELETE: u8 = 0x1D;
    pub const AVAILABLE: u8 = 0x1F;
    pub const DIR_TEST: u8 = 0x22;
    pub const FILE_TEST: u8 = 0x23;
}

/// Payload bytes per acknowledged block (`Link.ack_block_size`, `SIZE_ACK_BLOCK`).
pub const ACK_BLOCK: usize = 1024;
/// Largest single serial write edlink makes (`Link.TxData`).
pub(crate) const MAX_WRITE_BLOCK: usize = 4096;
/// Zero bytes edlink sends right after opening the port, before the handshake.
pub(crate) const WAKE_ZEROS: usize = 64 + 2;

/// Cart-memory address of the MCU FIFO a running ROM reads (`ADDR_FCI_SYS + 0x10000`).
pub const FIFO_ADDR: u32 = 0x1001_0000;

/// FAT attribute bit marking a directory (`AT_DIR`).
pub const ATTR_DIR: u8 = 0x10;

/// `FA_*` flags for [`crate::Ed64Pro::file_open`]; the FatFs `f_open` modes plus `MAKE_PATH`.
pub mod open_mode {
    pub const READ: u8 = 0x01;
    pub const WRITE: u8 = 0x02;
    pub const OPEN_EXISTING: u8 = 0x00;
    pub const CREATE_NEW: u8 = 0x04;
    pub const CREATE_ALWAYS: u8 = 0x08;
    pub const OPEN_ALWAYS: u8 = 0x10;
    pub const OPEN_APPEND: u8 = 0x30;
    /// Create missing parent directories (`FS_MAKEPATH`).
    pub const MAKE_PATH: u8 = 0x80;
}

/// `DIR_OPT_*` flags for [`crate::Ed64Pro::dir_list`].
pub mod dir_option {
    pub const SORTED: u8 = 0x01;
    pub const HIDE_SYSTEM: u8 = 0x02;
}

/// Endpoint types of an `EPO` transfer.
///
/// Only the four both sources agree on are listed. Above `0x13` they diverge — edlink numbers
/// `EFU = 0x15`, `USB = 0x16`; `ed64-pro-pub` numbers `RAM = 0x15`, `NOP = 0x16`, `DBG = 0x17`,
/// `USB = 0x18` — and the host does not need any of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Endpoint {
    /// The USB link, unacknowledged.
    Link = 0x10,
    /// The USB link in acknowledged 1024-byte blocks.
    LinkAck = 0x11,
    /// The currently open file.
    File = 0x12,
    /// Cart memory (`FCI` address space).
    Memory = 0x13,
}

/// 4-byte command frame: `'+'`, `'+' ^ 0xFF`, command, command `^ 0xFF` (`Link.TxCMD`).
pub fn cmd_frame(cmd: u8) -> [u8; 4] {
    [b'+', b'+' ^ 0xFF, cmd, cmd ^ 0xFF]
}

/// 5-byte command frame with a subcommand byte appended (`Link.TxCMD(cmd, scmd)`).
pub fn scmd_frame(cmd: u8, scmd: u8) -> [u8; 5] {
    [b'+', b'+' ^ 0xFF, cmd, cmd ^ 0xFF, scmd]
}

/// A string field: big-endian `u16` byte length, then the bytes.
///
/// edlink sends `str.Length` and the port's ASCII encoding, so the two agree for ASCII only. This
/// sends UTF-8 with its byte length, which is the only self-consistent choice for other names —
/// but how the firmware treats non-ASCII paths is unverified (spec, open questions).
pub fn string_field(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let len = u16::try_from(bytes.len()).ok()?;
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Some(out)
}

/// Body of `CMD_EPO` / `EPO_SCMD_XFER`: the 16-byte `EpoXfer` struct plus the trailing ack byte
/// (`DeviceIO_V2.EpoCmd`; field order matches `EpoXfer` in `everdrive.c`).
pub fn epo_xfer(src: Endpoint, dst: Endpoint, src_addr: u32, dst_addr: u32, len: u32) -> [u8; 17] {
    let mut out = [0u8; 17];
    out[0..4].copy_from_slice(&src_addr.to_be_bytes());
    out[4..8].copy_from_slice(&dst_addr.to_be_bytes());
    out[8..12].copy_from_slice(&len.to_be_bytes());
    out[12] = src as u8;
    out[13] = dst as u8;
    // out[14..16]: reserved u16, zero. out[16]: the ack byte edlink sends to start the transfer.
    out
}

/// One directory entry or file-info reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub size: u32,
    /// MS-DOS date field.
    pub date: u16,
    /// MS-DOS time field.
    pub time: u16,
    /// FAT attribute byte; see [`FileInfo::is_dir`].
    pub attributes: u8,
    pub name: String,
}

impl FileInfo {
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIR != 0
    }
}

/// Decode the fixed 9-byte head of a file-info record: size, date, time, attributes.
pub(crate) fn file_info_head(b: &[u8; 9]) -> (u32, u16, u16, u8) {
    (
        u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_be_bytes([b[4], b[5]]),
        u16::from_be_bytes([b[6], b[7]]),
        b[8],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_frames_match_link_txcmd() {
        assert_eq!(cmd_frame(CMD_STATUS), [0x2B, 0xD4, 0x10, 0xEF]);
        assert_eq!(cmd_frame(CMD_STATUS2), [0x2B, 0xD4, 0x40, 0xBF]);
        assert_eq!(
            scmd_frame(CMD_FS, fs::FILE_OPEN),
            [0x2B, 0xD4, 0x80, 0x7F, 0x17]
        );
    }

    #[test]
    fn string_field_is_big_endian_length_then_bytes() {
        assert_eq!(
            string_field("ed64/a.z64").unwrap(),
            [&[0x00, 0x0A][..], b"ed64/a.z64"].concat()
        );
        assert_eq!(string_field("").unwrap(), [0x00, 0x00]);
    }

    #[test]
    fn string_field_rejects_more_than_u16_bytes() {
        assert!(string_field(&"x".repeat(65_535)).is_some());
        assert!(string_field(&"x".repeat(65_536)).is_none());
    }

    #[test]
    fn epo_xfer_layout_matches_epocmd() {
        let b = epo_xfer(
            Endpoint::Memory,
            Endpoint::Link,
            0x1234_5678,
            0,
            0x0000_0400,
        );
        assert_eq!(
            b,
            [
                0x12, 0x34, 0x56, 0x78, // src_addr
                0x00, 0x00, 0x00, 0x00, // dst_addr
                0x00, 0x00, 0x04, 0x00, // len
                0x13, 0x10, // src FCI, dst LINK
                0x00, 0x00, // reserved
                0x00, // ack
            ]
        );
    }

    #[test]
    fn endpoint_numbers_are_the_ones_both_sources_agree_on() {
        assert_eq!(Endpoint::Link as u8, 0x10);
        assert_eq!(Endpoint::LinkAck as u8, 0x11);
        assert_eq!(Endpoint::File as u8, 0x12);
        assert_eq!(Endpoint::Memory as u8, 0x13);
    }

    #[test]
    fn file_info_head_decodes_big_endian_fields() {
        let head = [0x00, 0x80, 0x00, 0x00, 0x5A, 0x21, 0x60, 0x00, ATTR_DIR];
        assert_eq!(file_info_head(&head), (0x0080_0000, 0x5A21, 0x6000, 0x10));
    }
}
