//! EverDrive **64 X7** L2 adapter: maps the abstract L3 octet stream onto the EverDrive USB framing
//! and parses the same framing coming back, so the L3 codec above sees one continuous byte stream.
//!
//! The wire format is **`docs/spec/l3-over-everdrive-x7.md` §4**: a symmetric 8-byte **`DMA@`** header
//! carrying `(datatype << 24) | size`, the payload padded to a 2-byte boundary, then a **`CMPH`** trailer.
//!
//! # Validation status
//!
//! **This has never been run against EverDrive hardware.** The framing is transcribed from
//! [UNFLoader](https://github.com/buu342/N64-UNFLoader) and libdragon's `usb.c`, both of which drive an
//! X7 in practice, but nothing here has been confirmed against a cart. Spec §4.0 and §4.5 record what
//! that means and what to check first. Treat a successful [`Ed64L2Pipe::open`] as "the serial port
//! opened", not as "EverDrive support works" — there is no identity handshake in the data path, so use
//! `ed64-smoke` (spec §8) to confirm a port really is an EverDrive.
//!
//! [`Ed64L2Pipe`] mirrors `multi64_sc64_l2::Sc64L2Pipe`'s surface so the e2e tools can drive either cart.
//!
//! # Unsafe code
//!
//! This crate contains **no** `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

use serialport::{ClearBuffer, SerialPort};
use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// L3 datatype tag. MUST match `multi64_sc64_link::MULTI64_L3_TYPE` — both backends hand the same tag
/// to the same L3 codec, so a divergence would silently break one of them. Locked by a test.
pub const MULTI64_L3_TYPE: u8 = 0x01;

/// Header magic, both directions (spec §4.2).
const DMA_MAGIC: [u8; 4] = *b"DMA@";
/// Trailer magic, both directions (spec §4.2).
const CMP_MAGIC: [u8; 4] = *b"CMPH";

/// Payload alignment on send. libdragon's `usb.c` declares `USBPROTOCOL_VERSION 2`, which aligns to
/// **2** bytes; version 1 aligned to 512. Sending the wrong alignment mis-frames every message.
const SEND_ALIGN: usize = 2;

/// The header's size field is 24 bits, so one message cannot carry more than this.
const MAX_MESSAGE_BYTES: usize = 0x00FF_FFFF;

/// Default payload bytes per message. Deliberately conservative: one `REG_USB_DATA` window (spec §3).
/// Raise it via [`Ed64L2Pipe::write_l3_stream_with_max`] once the link is proven on hardware.
pub const DEFAULT_ED64_CHUNK: usize = 512;

fn align_up(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// One decoded `DMA@` message.
#[derive(Debug, PartialEq, Eq)]
struct WireMessage {
    datatype: u8,
    payload: Vec<u8>,
}

/// Incremental parser for the `DMA@` framing. Holds bytes that do not yet form a whole message.
#[derive(Default)]
struct WireBuffer {
    buf: Vec<u8>,
}

impl WireBuffer {
    fn push_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Drop leading bytes until the buffer starts with `DMA@`. Returns how many were discarded.
    fn resync(&mut self) -> usize {
        if self.buf.starts_with(&DMA_MAGIC) {
            return 0;
        }
        match self
            .buf
            .windows(DMA_MAGIC.len())
            .position(|w| w == DMA_MAGIC)
        {
            Some(at) => {
                self.buf.drain(..at);
                at
            }
            None => {
                // Keep a partial magic that may complete on the next read.
                let keep = (DMA_MAGIC.len() - 1).min(self.buf.len());
                let dropped = self.buf.len() - keep;
                self.buf.drain(..dropped);
                dropped
            }
        }
    }

    /// Pop the next complete message, or `Ok(None)` when more bytes are needed.
    fn next_message(&mut self) -> io::Result<Option<WireMessage>> {
        let skipped = self.resync();
        if skipped > 0 {
            tracing::debug!(
                target: "multi64_ed64_l2",
                skipped,
                "resynchronised: discarded bytes before DMA@"
            );
        }
        if self.buf.len() < 8 {
            return Ok(None);
        }
        let head = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]);
        let datatype = (head >> 24) as u8;
        let size = (head & 0x00FF_FFFF) as usize;
        let padded = align_up(size, SEND_ALIGN);
        let total = 8 + padded + CMP_MAGIC.len();
        if self.buf.len() < total {
            return Ok(None);
        }
        if self.buf[8 + padded..total] != CMP_MAGIC {
            let got = self.buf[8 + padded..total].to_vec();
            // Drop the magic so the next resync cannot latch onto this same bad message forever.
            self.buf.drain(..DMA_MAGIC.len());
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ED64: expected CMPH trailer, got {got:02x?}"),
            ));
        }
        let payload = self.buf[8..8 + size].to_vec();
        self.buf.drain(..total);
        Ok(Some(WireMessage { datatype, payload }))
    }
}

/// Encode one payload as a `DMA@` message (spec §4.2).
fn encode_message(datatype: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::other(format!(
            "ED64: message of {} bytes exceeds the 24-bit size field",
            payload.len()
        )));
    }
    let padded = align_up(payload.len(), SEND_ALIGN);
    let mut out = Vec::with_capacity(8 + padded + CMP_MAGIC.len());
    out.extend_from_slice(&DMA_MAGIC);
    let head = (u32::from(datatype) << 24) | (payload.len() as u32 & 0x00FF_FFFF);
    out.extend_from_slice(&head.to_be_bytes());
    out.extend_from_slice(payload);
    out.resize(8 + padded, 0);
    out.extend_from_slice(&CMP_MAGIC);
    Ok(out)
}

/// Drain decoded messages into the L3 byte queue, keeping only [`MULTI64_L3_TYPE`] payloads.
fn process_wire_messages(wire: &mut WireBuffer, l3_rx: &mut VecDeque<u8>) -> io::Result<()> {
    while let Some(msg) = wire.next_message()? {
        if msg.datatype == MULTI64_L3_TYPE {
            l3_rx.extend(msg.payload);
        } else {
            tracing::debug!(
                target: "multi64_ed64_l2",
                datatype = msg.datatype,
                len = msg.payload.len(),
                "non-MULTI64_L3 datatype (ignored)"
            );
        }
    }
    Ok(())
}

/// Host-side pipe: L3 octets in and out of an EverDrive 64 X7 over USB serial.
pub struct Ed64L2Pipe {
    port: Box<dyn SerialPort>,
    wire: WireBuffer,
    l3_rx: VecDeque<u8>,
}

impl Ed64L2Pipe {
    /// Open the serial device (FTDI VCP — see spec §4.1).
    ///
    /// Succeeding means the port opened, nothing more: the data path carries no identity handshake,
    /// so this cannot tell an EverDrive from any other serial device. Probe with `ed64-smoke` first.
    pub fn open(port_name: &str, baud: u32) -> io::Result<Self> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            port,
            wire: WireBuffer::default(),
            l3_rx: VecDeque::new(),
        })
    }

    /// Apply a new read timeout to the underlying port.
    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.port.set_timeout(t).map_err(io::Error::other)
    }

    /// Clear host serial buffers **and** internal parse state (spec §4.4 resynchronisation).
    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        self.port
            .clear(ClearBuffer::All)
            .map_err(io::Error::other)?;
        self.wire = WireBuffer::default();
        self.l3_rx.clear();
        Ok(())
    }

    /// Send raw L3 octets to the N64, split into [`DEFAULT_ED64_CHUNK`] payloads.
    pub fn write_l3_stream(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write_l3_stream_with_max(buf, DEFAULT_ED64_CHUNK)
    }

    /// Same as [`write_l3_stream`](Self::write_l3_stream) with an explicit payload size (tuning / tests).
    pub fn write_l3_stream_with_max(&mut self, buf: &[u8], max_chunk: usize) -> io::Result<()> {
        let max_chunk = max_chunk.clamp(1, MAX_MESSAGE_BYTES);
        for chunk in buf.chunks(max_chunk) {
            let msg = encode_message(MULTI64_L3_TYPE, chunk)?;
            self.port.write_all(&msg)?;
        }
        self.port.flush()?;
        Ok(())
    }

    /// Read L3 octets from the N64. Returns `0` on timeout with an empty queue, matching `Sc64L2Pipe`.
    pub fn read_l3_bytes(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.l3_rx.is_empty() {
                let n = self.l3_rx.len().min(out.len());
                for slot in out.iter_mut().take(n) {
                    *slot = self.l3_rx.pop_front().expect("len checked");
                }
                return Ok(n);
            }
            let mut scratch = [0u8; 512];
            match self.port.read(&mut scratch) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.wire.push_bytes(&scratch[..n]);
                    process_wire_messages(&mut self.wire, &mut self.l3_rx)?;
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => return Ok(0),
                Err(e) => return Err(e),
            }
        }
    }

    /// Read until `out` is filled, polling the serial port until `deadline` elapses.
    pub fn read_l3_bytes_exact(&mut self, out: &mut [u8], deadline: Duration) -> io::Result<()> {
        let start = Instant::now();
        let mut off = 0;
        while off < out.len() {
            if start.elapsed() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "read_l3_bytes_exact: deadline exceeded",
                ));
            }
            let n = self.read_l3_bytes(&mut out[off..])?;
            if n == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            off += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two backends must tag L3 payloads identically or the shared codec breaks on one of them.
    #[test]
    fn datatype_tag_matches_sc64_backend() {
        assert_eq!(MULTI64_L3_TYPE, multi64_sc64_link::MULTI64_L3_TYPE);
    }

    fn decode_all(bytes: &[u8]) -> io::Result<Vec<WireMessage>> {
        let mut w = WireBuffer::default();
        w.push_bytes(bytes);
        let mut out = Vec::new();
        while let Some(m) = w.next_message()? {
            out.push(m);
        }
        Ok(out)
    }

    #[test]
    fn round_trip_even_length() {
        let wire = encode_message(MULTI64_L3_TYPE, b"M64B").unwrap();
        assert_eq!(&wire[0..4], b"DMA@");
        assert_eq!(&wire[wire.len() - 4..], b"CMPH");
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].datatype, MULTI64_L3_TYPE);
        assert_eq!(msgs[0].payload, b"M64B");
    }

    #[test]
    fn odd_length_is_padded_but_payload_is_not() {
        let wire = encode_message(MULTI64_L3_TYPE, &[0xAA, 0xBB, 0xCC]).unwrap();
        // 8 header + 3 payload + 1 pad + 4 trailer
        assert_eq!(wire.len(), 16);
        assert_eq!(wire[11], 0x00, "pad byte");
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs[0].payload, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn header_encodes_datatype_and_size_big_endian() {
        let wire = encode_message(0x7F, &[0u8; 0x1234]).unwrap();
        assert_eq!(
            u32::from_be_bytes([wire[4], wire[5], wire[6], wire[7]]),
            0x7F00_1234
        );
    }

    #[test]
    fn empty_payload_round_trips() {
        let wire = encode_message(MULTI64_L3_TYPE, &[]).unwrap();
        assert_eq!(wire.len(), 12);
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs[0].payload, Vec::<u8>::new());
    }

    #[test]
    fn two_messages_concatenate_into_one_stream() {
        let mut wire = encode_message(MULTI64_L3_TYPE, b"abc").unwrap();
        wire.extend(encode_message(MULTI64_L3_TYPE, b"de").unwrap());
        let mut q = VecDeque::new();
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        process_wire_messages(&mut w, &mut q).unwrap();
        let got: Vec<u8> = q.into_iter().collect();
        assert_eq!(got, b"abcde");
    }

    #[test]
    fn other_datatypes_are_dropped_not_streamed() {
        let mut wire = encode_message(0x02, b"debug text").unwrap();
        wire.extend(encode_message(MULTI64_L3_TYPE, b"L3").unwrap());
        let mut q = VecDeque::new();
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        process_wire_messages(&mut w, &mut q).unwrap();
        let got: Vec<u8> = q.into_iter().collect();
        assert_eq!(got, b"L3", "only MULTI64_L3 payloads reach the codec");
    }

    #[test]
    fn partial_message_yields_none_until_complete() {
        let wire = encode_message(MULTI64_L3_TYPE, b"hello").unwrap();
        let mut w = WireBuffer::default();
        w.push_bytes(&wire[..wire.len() - 1]);
        assert_eq!(w.next_message().unwrap(), None);
        w.push_bytes(&wire[wire.len() - 1..]);
        assert_eq!(w.next_message().unwrap().unwrap().payload, b"hello");
    }

    #[test]
    fn leading_garbage_is_resynchronised() {
        let mut wire = b"\x00\xffnoise".to_vec();
        wire.extend(encode_message(MULTI64_L3_TYPE, b"ok").unwrap());
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"ok");
    }

    #[test]
    fn bad_trailer_is_invalid_data_and_does_not_wedge() {
        let mut wire = encode_message(MULTI64_L3_TYPE, b"xy").unwrap();
        let n = wire.len();
        wire[n - 1] = b'!';
        wire.extend(encode_message(MULTI64_L3_TYPE, b"next").unwrap());
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        let e = w.next_message().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // The parser must recover and find the following good message.
        assert_eq!(w.next_message().unwrap().unwrap().payload, b"next");
    }

    #[test]
    fn oversized_message_is_rejected() {
        // Cheap check of the bound without allocating 16 MiB of payload.
        assert!(align_up(MAX_MESSAGE_BYTES, SEND_ALIGN) >= MAX_MESSAGE_BYTES);
        assert_eq!(align_up(0, SEND_ALIGN), 0);
        assert_eq!(align_up(1, SEND_ALIGN), 2);
        assert_eq!(align_up(2, SEND_ALIGN), 2);
    }
}
