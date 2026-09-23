//! PC-side helpers for the **SummerCart64** USB serial protocol: **`CMD`**, **`CMP`**, and **`PKT`** framing.
//!
//! Provides [`WireBuffer`] for incremental parsing, [`usb_write_l3_stream`] to chunk raw L3 bytes into `USB_WRITE`
//! packets, and constants used by **`multi64-sc64-l2`**. Vendor layout is described in SummerCart64
//! **`docs/03_usb_interface.md`**; multi64’s L3 tagging is in **`docs/spec/l3-over-sc64.md`**.
//!
//! # Unsafe code
//!
//! This crate contains **no** `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

mod wire;

pub use wire::{PktPacket, WireBuffer, WireEvent};

/// Command IDs used by smoke tests and tooling.
pub mod cmd {
    pub const IDENTIFIER_GET: u8 = b'v';
    pub const VERSION_GET: u8 = b'V';
    /// PC → SC64: `USB_WRITE` (see SummerCart64 `docs/03_usb_interface.md`).
    pub const USB_WRITE: u8 = b'U';
    /// Read cart SDRAM/flash buffer from PC (`MEMORY_READ`).
    pub const MEMORY_READ: u8 = b'm';
    /// Write cart memory from PC (`MEMORY_WRITE`).
    pub const MEMORY_WRITE: u8 = b'M';
    /// SD card special op (`SD_CARD_OP`): init/deinit/status/…
    pub const SD_CARD_OP: u8 = b'i';
    /// Read SD sectors into cart memory (`SD_READ`).
    pub const SD_READ: u8 = b's';
    /// Write SD sectors from cart memory (`SD_WRITE`).
    pub const SD_WRITE: u8 = b'S';
}

/// multi64 L3 stream datatype tag (`docs/spec/l3-over-sc64.md`).
pub const MULTI64_L3_TYPE: u8 = 0x01;

/// Default max `USB_WRITE` payload bytes per chunk (matches L3 default scale).
pub const DEFAULT_USB_WRITE_CHUNK: usize = 8192;

/// Largest `data` a `CMP`/`ERR` response can carry: a `MEMORY_READ` spanning the cart's whole
/// 128 MiB address space (vendor `docs/01_memory_map.md`). Every other response is at most 8 bytes.
pub const MAX_CMP_DATA_LEN: usize = 0x0800_0000;

/// Largest `data` a `PKT` can carry: `U` (DATA) is a datatype byte, a 24-bit length and that many
/// bytes (vendor `docs/03_usb_interface.md`). Every other packet id carries less.
pub const MAX_PKT_DATA_LEN: usize = 4 + 0x00FF_FFFF;

/// Data length of the vendor response header at the start of `buf` (at least 8 bytes), or `None`
/// if `buf` does not start with `CMP`/`ERR`/`PKT` or the length exceeds what that tag can carry.
/// An impossible length means the tag was found inside other data, so callers resynchronize.
fn header_data_len(buf: &[u8]) -> Option<usize> {
    let cap = match &buf[0..3] {
        b"CMP" | b"ERR" => MAX_CMP_DATA_LEN,
        b"PKT" => MAX_PKT_DATA_LEN,
        _ => return None,
    };
    let len = u32::from_be_bytes(buf[4..8].try_into().ok()?) as usize;
    (len <= cap).then_some(len)
}

/// Build one `USB_WRITE` `CMD` carrying a chunk of raw L3 octets (`arg0` lower 8 bits = [MULTI64_L3_TYPE]).
pub fn usb_write_l3_chunk(chunk: &[u8]) -> Vec<u8> {
    let arg0 = u32::from(MULTI64_L3_TYPE);
    let arg1 = chunk.len() as u32;
    cmd_packet(cmd::USB_WRITE, arg0, arg1, chunk)
}

/// Split `data` into `USB_WRITE`-sized chunks and encode each as a full `CMD` packet.
pub fn usb_write_l3_stream(data: &[u8], max_chunk: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
    data.chunks(max_chunk).map(usb_write_l3_chunk)
}

/// Parse `PKT` id `U` **DATA** inner payload (`datatype` + 24-bit BE length + bytes).
/// Returns L3 octets when `datatype == MULTI64_L3_TYPE`.
pub fn pkt_u_multi64_l3_payload(pkt_u_data: &[u8]) -> Option<&[u8]> {
    if pkt_u_data.len() < 4 {
        return None;
    }
    if pkt_u_data[0] != MULTI64_L3_TYPE {
        return None;
    }
    let len =
        u32::from(pkt_u_data[1]) << 16 | u32::from(pkt_u_data[2]) << 8 | u32::from(pkt_u_data[3]);
    let end = 4usize.saturating_add(len as usize);
    if pkt_u_data.len() < end {
        return None;
    }
    Some(&pkt_u_data[4..end])
}

/// Build a `CMD` packet: `CMD` + id + arg0 (BE) + arg1 (BE) + data.
pub fn cmd_packet(cmd_id: u8, arg0: u32, arg1: u32, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + data.len());
    v.extend_from_slice(b"CMD");
    v.push(cmd_id);
    v.extend_from_slice(&arg0.to_be_bytes());
    v.extend_from_slice(&arg1.to_be_bytes());
    v.extend_from_slice(data);
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmpResponse {
    pub ok: bool,
    pub cmd_id: u8,
    pub data: Vec<u8>,
}

/// Buffer for interleaved `CMP`/`ERR`/`PKT` responses from the serial stream: [`WireBuffer`] for
/// callers that only wait on command responses.
#[derive(Default)]
pub struct ResponseBuffer {
    wire: WireBuffer,
}

impl ResponseBuffer {
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        self.wire.push_bytes(chunk);
    }

    /// Pull the next `CMP` or `ERR` packet, skipping `PKT` and resynchronizing if needed.
    ///
    /// A tag whose length exceeds [`MAX_CMP_DATA_LEN`] / [`MAX_PKT_DATA_LEN`] is treated as noise.
    pub fn next_cmp(&mut self) -> Option<CmpResponse> {
        loop {
            match self.wire.next_event()? {
                WireEvent::Cmp(cmp) => return Some(cmp),
                WireEvent::Pkt(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_identifier_layout() {
        let p = cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]);
        assert_eq!(&p[0..3], b"CMD");
        assert_eq!(p[3], b'v');
        assert_eq!(p[4..8], [0, 0, 0, 0]);
        assert_eq!(p[8..12], [0, 0, 0, 0]);
        assert_eq!(p.len(), 12);
    }

    #[test]
    fn parse_cmp_identifier() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"CMP");
        buf.push(b'v');
        buf.extend_from_slice(&(4u32).to_be_bytes());
        buf.extend_from_slice(b"SCv2");
        let mut rb = ResponseBuffer::default();
        rb.push_bytes(&buf);
        let r = rb.next_cmp().unwrap();
        assert!(r.ok);
        assert_eq!(r.cmd_id, b'v');
        assert_eq!(r.data, b"SCv2");
    }

    #[test]
    fn usb_write_l3_layout() {
        let p = usb_write_l3_chunk(&[1, 2, 3]);
        assert_eq!(&p[0..3], b"CMD");
        assert_eq!(p[3], cmd::USB_WRITE);
        let arg0 = u32::from_be_bytes(p[4..8].try_into().unwrap());
        assert_eq!(arg0 & 0xFF, u32::from(MULTI64_L3_TYPE));
        let arg1 = u32::from_be_bytes(p[8..12].try_into().unwrap());
        assert_eq!(arg1, 3);
        assert_eq!(&p[12..], &[1, 2, 3]);
    }

    #[test]
    fn pkt_u_inner_round_trip() {
        let inner = [MULTI64_L3_TYPE, 0, 0, 2, 10, 11];
        let pl = pkt_u_multi64_l3_payload(&inner).unwrap();
        assert_eq!(pl, &[10, 11]);
    }

    #[test]
    fn buffer_skips_pkt() {
        let mut b = ResponseBuffer::default();
        let pkt_len = 4u32.to_be_bytes();
        let mut blob = Vec::new();
        blob.extend_from_slice(b"PKT");
        blob.push(b'U');
        blob.extend_from_slice(&pkt_len);
        blob.extend_from_slice(&[1, 2, 3, 4]);
        blob.extend_from_slice(b"CMP");
        blob.push(b'v');
        blob.extend_from_slice(&(4u32).to_be_bytes());
        blob.extend_from_slice(b"SCv2");
        b.push_bytes(&blob);
        let r = b.next_cmp().unwrap();
        assert_eq!(r.cmd_id, b'v');
        assert_eq!(r.data, b"SCv2");
    }

    /// #135: `ResponseBuffer` must skip a `CMP` or `PKT` header whose length is impossible.
    #[test]
    fn buffer_resyncs_past_oversized_lengths() {
        for (tag, len) in [(b"CMP", 0x9000_0000u32), (b"PKT", 0x2000_0000u32)] {
            let mut blob = tag.to_vec();
            blob.push(b'v');
            blob.extend_from_slice(&len.to_be_bytes());
            blob.extend_from_slice(b"CMP");
            blob.push(b'V');
            blob.extend_from_slice(&(4u32).to_be_bytes());
            blob.extend_from_slice(b"SCv2");
            let mut b = ResponseBuffer::default();
            b.push_bytes(&blob);
            let r = b.next_cmp().expect("valid CMP after the bad header");
            assert_eq!(r.cmd_id, b'V');
            assert_eq!(r.data, b"SCv2");
        }
    }
}
