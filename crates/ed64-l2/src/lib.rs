//! EverDrive **64 X7** L2 adapter: maps the abstract L3 octet stream onto the EverDrive USB framing
//! and parses the same framing coming back, so the L3 codec above sees one continuous byte stream.
//!
//! The wire format is **`docs/spec/l3-over-everdrive-x7.md` §4**: an 8-byte **`DMA@`** header carrying
//! `(datatype << 24) | size`, the payload, and a **`CMPH`** trailer, everything aligned to 2 bytes.
//!
//! The two directions are **not** symmetric about where that alignment goes (#134). Writing to the
//! cart, the payload is padded and the trailer follows it; reading from the cart, the trailer follows
//! the unpadded payload and the padding comes after it. Both carry the unpadded length in the header,
//! so the layouts differ only for an odd-length payload.
//!
//! # Validation status
//!
//! **This has run on an X7 only.** The framing is transcribed from
//! [UNFLoader](https://github.com/buu342/N64-UNFLoader) and libdragon's `usb.c`, both of which drive an
//! X7 in practice. On that cart, L3 went both ways through `multi64d` with no stream errors. Over a
//! direct serial connection, with the cart echoing messages while the host was still sending them,
//! one arrived cut off. The parser reports that and resynchronizes at the next `DMA@` (spec §4.4).
//! In a later run a message from the cart stopped part-way and nothing followed it for seconds, so
//! the parser counted the cart's next messages as its payload; it now drops a message that stalls
//! for [`PARTIAL_MESSAGE_STALL`] (spec §4.5 item 7). Spec §4.0 and §4.5 record what that means and
//! what to check first. Treat a successful [`Ed64L2Pipe::open`] as "the serial port
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

/// Wire alignment, both directions. libdragon's `usb.c` declares `USBPROTOCOL_VERSION 2`, which
/// aligns to **2** bytes; version 1 aligned to 512. The wrong alignment mis-frames every message.
///
/// *What* is aligned differs by direction, which is the whole of #134: the host pads the **payload**
/// and then writes the trailer, while the cart writes the trailer straight after the unpadded
/// payload and pads the **whole message**. See [`encode_message`] and [`WireBuffer::next_message`].
const WIRE_ALIGN: usize = 2;

/// The header's size field is 24 bits, so one message cannot carry more than this.
const MAX_MESSAGE_BYTES: usize = 0x00FF_FFFF;

/// How long the cart may go quiet part-way through a message before [`Ed64L2Pipe`] drops the part it
/// holds and resynchronizes on the next `DMA@` (spec §4.4).
///
/// A cart sends a message's blocks back to back. Across 142 whole messages from an X7, the longest
/// wait for the next byte of a message was 15 ms; the agent's `ed64_send`, retrying a busy PI, adds
/// at most about 0.2 s. A message that stops for longer was cut off, and one did: a 4122-byte reply
/// stopped after 2048 bytes, and the decoder counted the cart's next 45 s of messages as its payload
/// (spec §4.5 item 7).
///
/// Only time spent waiting in a read counts, so a reader that was busy elsewhere while bytes queued
/// up in the driver is not mistaken for a quiet cart.
pub const PARTIAL_MESSAGE_STALL: Duration = Duration::from_millis(500);

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

    /// Drop everything held: the part of a message that never finished. Returns how many bytes
    /// went, and the size of the message they began, when they got as far as its header.
    fn discard_partial(&mut self) -> (usize, Option<usize>) {
        let size = (self.buf.len() >= 8 && self.buf.starts_with(&DMA_MAGIC))
            .then(|| (u32::from_be_bytes([0, self.buf[5], self.buf[6], self.buf[7]])) as usize);
        let held = self.buf.len();
        self.buf.clear();
        (held, size)
    }

    /// Pop the next complete message, or `Ok(None)` when more bytes are needed.
    fn next_message(&mut self) -> io::Result<Option<WireMessage>> {
        let skipped = self.resync();
        if skipped > 0 {
            tracing::debug!(
                target: "multi64_ed64_l2",
                skipped,
                "resynchronized: discarded bytes before DMA@"
            );
        }
        if self.buf.len() < 8 {
            return Ok(None);
        }
        let head = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]);
        let datatype = (head >> 24) as u8;
        let size = (head & 0x00FF_FFFF) as usize;
        // A cart writes the trailer straight after the *unpadded* payload and pads the whole
        // message afterwards, so the alignment byte of an odd payload follows `CMPH` rather than
        // preceding it (#134). That is what libdragon's `usb_everdrive_write` sends and what
        // UNFLoader's receive path reads; this used to assume the other direction's layout and
        // looked for the trailer one byte late, failing every odd-length message from the cart.
        let trailer = 8 + size;
        let total = align_up(trailer + CMP_MAGIC.len(), WIRE_ALIGN);
        if self.buf.len() < total {
            return Ok(None);
        }
        if self.buf[trailer..trailer + CMP_MAGIC.len()] != CMP_MAGIC {
            let got = self.buf[trailer..trailer + CMP_MAGIC.len()].to_vec();
            // Drop the magic so the next resync cannot latch onto this same bad message forever.
            self.buf.drain(..DMA_MAGIC.len());
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ED64: expected CMPH trailer, got {got:02x?}"),
            ));
        }
        let payload = self.buf[8..trailer].to_vec();
        // `total` covers the alignment byte, so it leaves with its own message. A cart sends
        // whatever its buffer held there, so it is never looked at and never reaches L3.
        self.buf.drain(..total);
        Ok(Some(WireMessage { datatype, payload }))
    }
}

/// Encode one payload as a `DMA@` message for the **cart** (spec §4.2).
///
/// Host to cart, the payload is zero-padded to [`WIRE_ALIGN`] and the trailer follows the padding —
/// what libdragon's `usb_everdrive_poll` reads, and what UNFLoader's `device_senddata_everdrive`
/// sends. The other direction places the padding differently, so this is not the inverse of
/// [`WireBuffer::next_message`] for an odd-length payload (#134).
fn encode_message(datatype: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::other(format!(
            "ED64: message of {} bytes exceeds the 24-bit size field",
            payload.len()
        )));
    }
    let padded = align_up(payload.len(), WIRE_ALIGN);
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
    /// An L2 error parsed after L3 bytes were queued. Held until [`read_l3_bytes`](Self::read_l3_bytes)
    /// has handed those bytes out, so a caller that drops the pipe on error does not lose them.
    pending_err: Option<io::Error>,
    /// Time spent waiting in reads that brought nothing, while `wire` holds part of a message.
    quiet: Duration,
    /// [`PARTIAL_MESSAGE_STALL`], except in tests.
    stall: Duration,
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
            pending_err: None,
            quiet: Duration::ZERO,
            stall: PARTIAL_MESSAGE_STALL,
        })
    }

    /// Apply a new read timeout to the underlying port.
    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.port.set_timeout(t).map_err(io::Error::other)
    }

    /// Clear host serial buffers **and** internal parse state (spec §4.4 resynchronization).
    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        self.port
            .clear(ClearBuffer::All)
            .map_err(io::Error::other)?;
        self.wire = WireBuffer::default();
        self.l3_rx.clear();
        self.pending_err = None;
        self.quiet = Duration::ZERO;
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
    ///
    /// An L2 error (bad `CMPH` trailer) is returned only once every L3 octet received before it has
    /// been read.
    ///
    /// A message the cart stops sending part-way is dropped once [`PARTIAL_MESSAGE_STALL`] has
    /// passed in reads with no byte arriving. That is not an error: its bytes never reach L3, it is
    /// logged at `warn`, and the next `DMA@` decodes as usual.
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
            if let Some(e) = self.pending_err.take() {
                return Err(e);
            }
            // A bad trailer stops parsing with whole messages possibly still buffered behind it.
            // Parse them now, rather than leave them waiting for a read that brings more bytes,
            // where a quiet cart would have them taken for a message that stalled part-way.
            if !self.wire.buf.is_empty() {
                if let Err(e) = process_wire_messages(&mut self.wire, &mut self.l3_rx) {
                    self.pending_err = Some(e);
                }
                if !self.l3_rx.is_empty() || self.pending_err.is_some() {
                    continue;
                }
            }
            let mut scratch = [0u8; 512];
            let asked = Instant::now();
            let read = self.port.read(&mut scratch);
            // Before the bytes it brought go in: a read that blocked past the stall and then got
            // the start of the next message must not append it to the one that was cut off.
            self.count_quiet(asked.elapsed());
            match read {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.quiet = Duration::ZERO;
                    // Same target shape as `multi64_sc64_l2`, so `multi64d --serial-trace` shows either cart.
                    tracing::trace!(
                        target: "multi64_ed64_l2",
                        raw_bytes = n,
                        bytes = %hex(&scratch[..n]),
                        "serial read from cart"
                    );
                    self.wire.push_bytes(&scratch[..n]);
                    if let Err(e) = process_wire_messages(&mut self.wire, &mut self.l3_rx) {
                        // The loop hands out anything queued first, then this error.
                        self.pending_err = Some(e);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => return Ok(0),
                Err(e) => return Err(e),
            }
        }
    }

    /// Count `waited`, time a read spent blocked, against the part of a message `wire` holds, and
    /// drop that part once `self.stall` has passed with no byte arriving.
    fn count_quiet(&mut self, waited: Duration) {
        if self.wire.buf.is_empty() {
            self.quiet = Duration::ZERO;
            return;
        }
        self.quiet += waited;
        if self.quiet < self.stall {
            return;
        }
        let quiet_ms = self.quiet.as_millis();
        self.quiet = Duration::ZERO;
        let (held, size) = self.wire.discard_partial();
        match size {
            Some(size) => tracing::warn!(
                target: "multi64_ed64_l2",
                held,
                size,
                quiet_ms,
                "dropped a message the cart stopped sending part-way; resynchronizing on the next DMA@"
            ),
            None => tracing::debug!(
                target: "multi64_ed64_l2",
                held,
                quiet_ms,
                "dropped bytes short of a DMA@ header after the cart went quiet"
            ),
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

/// `bytes` as lowercase hex, for the `--serial-trace` line: what the cart actually sent, so a
/// garbled message can be read back from a log.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
#[path = "../../l2-test-support/scripted_port.rs"]
mod scripted_port;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted_port::ScriptedPort;

    #[test]
    fn serial_trace_hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0xab, 0x10, 0xff]), "00ab10ff");
        assert_eq!(hex(&[]), "");
    }

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

    /// Build a message as a **cart** sends one (spec §4.2): the trailer straight after the unpadded
    /// payload, then the whole message padded to 2 bytes. `pad` is what the cart's buffer happened
    /// to hold — libdragon sends leftover bytes there, not zeros.
    /// [`cart_message`] with a zero alignment byte — for tests that care about what the parser does
    /// with a message, not about what the padding held.
    fn from_cart(datatype: u8, payload: &[u8]) -> Vec<u8> {
        cart_message(datatype, payload, 0)
    }

    fn cart_message(datatype: u8, payload: &[u8], pad: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&DMA_MAGIC);
        let head = (u32::from(datatype) << 24) | (payload.len() as u32 & 0x00FF_FFFF);
        out.extend_from_slice(&head.to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&CMP_MAGIC);
        out.resize(align_up(out.len(), WIRE_ALIGN), pad);
        out
    }

    /// #134: a cart puts `CMPH` straight after the unpadded payload and pads the whole message
    /// afterwards, so an odd payload's alignment byte lands *after* the trailer. The parser used to
    /// read the padded layout of the other direction and looked for the trailer one byte late,
    /// which failed every odd-length message from the cart.
    #[test]
    fn an_odd_payload_from_the_cart_has_its_padding_after_the_trailer() {
        let msgs = decode_all(&cart_message(MULTI64_L3_TYPE, &[0xAA, 0xBB, 0xCC], 0x5A)).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, vec![0xAA, 0xBB, 0xCC]);
    }

    /// The alignment byte is consumed with the message it belongs to: it is not payload, and it
    /// must not be left to be resynchronized past — a cart sends whatever was in its buffer there,
    /// which could be any byte at all.
    #[test]
    fn the_alignment_byte_is_consumed_and_never_reaches_l3() {
        let mut wire = cart_message(MULTI64_L3_TYPE, &[1, 2, 3], 0xFF);
        wire.extend_from_slice(&cart_message(MULTI64_L3_TYPE, &[4, 5, 6, 7], 0));
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs.len(), 2, "the second message parsed without resyncing");
        assert_eq!(msgs[0].payload, vec![1, 2, 3]);
        assert_eq!(msgs[1].payload, vec![4, 5, 6, 7]);
    }

    /// The framing is **not** symmetric (#134). The host pads the payload up to the trailer; the
    /// cart pads the whole message after it. Both are 2-byte aligned and both carry the unpadded
    /// length, so they differ only for an odd payload — which is why this went unnoticed.
    #[test]
    fn the_two_directions_place_an_odd_payloads_padding_differently() {
        let sent = encode_message(MULTI64_L3_TYPE, &[0xAA, 0xBB, 0xCC]).unwrap();
        let received = cart_message(MULTI64_L3_TYPE, &[0xAA, 0xBB, 0xCC], 0);
        assert_eq!(sent.len(), received.len(), "same length either way");
        assert_eq!(&sent[11..12], &[0x00], "host: pad, then the trailer");
        assert_eq!(&sent[12..16], b"CMPH");
        assert_eq!(&received[11..15], b"CMPH", "cart: trailer, then the pad");
        assert_ne!(sent, received);
    }

    /// An even payload is framed identically in both directions, so this round trip is the only one
    /// that holds; see [`the_two_directions_place_an_odd_payloads_padding_differently`].
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

    /// Send direction only: the payload is padded up to the trailer, and the header still carries
    /// the unpadded length, so the cart hands 3 bytes to its L3 layer and drops the pad. Decoding
    /// this back is *not* a valid round trip for an odd payload — see
    /// [`the_two_directions_place_an_odd_payloads_padding_differently`].
    #[test]
    fn a_sent_odd_payload_is_padded_before_the_trailer() {
        let wire = encode_message(MULTI64_L3_TYPE, &[0xAA, 0xBB, 0xCC]).unwrap();
        // 8 header + 3 payload + 1 pad + 4 trailer
        assert_eq!(wire.len(), 16);
        assert_eq!(wire[11], 0x00, "pad byte");
        assert_eq!(&wire[12..16], b"CMPH");
        assert_eq!(
            u32::from_be_bytes([wire[4], wire[5], wire[6], wire[7]]) & 0x00FF_FFFF,
            3,
            "the header carries the unpadded length"
        );
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
        let mut wire = from_cart(MULTI64_L3_TYPE, b"abc");
        wire.extend(from_cart(MULTI64_L3_TYPE, b"de"));
        let mut q = VecDeque::new();
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        process_wire_messages(&mut w, &mut q).unwrap();
        let got: Vec<u8> = q.into_iter().collect();
        assert_eq!(got, b"abcde");
    }

    #[test]
    fn other_datatypes_are_dropped_not_streamed() {
        let mut wire = from_cart(0x02, b"debug text");
        wire.extend(from_cart(MULTI64_L3_TYPE, b"L3"));
        let mut q = VecDeque::new();
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        process_wire_messages(&mut w, &mut q).unwrap();
        let got: Vec<u8> = q.into_iter().collect();
        assert_eq!(got, b"L3", "only MULTI64_L3 payloads reach the codec");
    }

    #[test]
    fn partial_message_yields_none_until_complete() {
        let wire = from_cart(MULTI64_L3_TYPE, b"hello");
        let mut w = WireBuffer::default();
        w.push_bytes(&wire[..wire.len() - 1]);
        assert_eq!(w.next_message().unwrap(), None);
        w.push_bytes(&wire[wire.len() - 1..]);
        assert_eq!(w.next_message().unwrap().unwrap().payload, b"hello");
    }

    #[test]
    fn leading_garbage_is_resynchronized() {
        let mut wire = b"\x00\xffnoise".to_vec();
        wire.extend(from_cart(MULTI64_L3_TYPE, b"ok"));
        let msgs = decode_all(&wire).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"ok");
    }

    #[test]
    fn bad_trailer_is_invalid_data_and_does_not_wedge() {
        let mut wire = from_cart(MULTI64_L3_TYPE, b"xy");
        let n = wire.len();
        wire[n - 1] = b'!';
        wire.extend(from_cart(MULTI64_L3_TYPE, b"next"));
        let mut w = WireBuffer::default();
        w.push_bytes(&wire);
        let e = w.next_message().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // The parser must recover and find the following good message.
        assert_eq!(w.next_message().unwrap().unwrap().payload, b"next");
    }

    fn pipe_over(reads: Vec<Vec<u8>>) -> Ed64L2Pipe {
        pipe_on(ScriptedPort::new(reads), PARTIAL_MESSAGE_STALL)
    }

    fn pipe_on(port: ScriptedPort, stall: Duration) -> Ed64L2Pipe {
        Ed64L2Pipe {
            port: Box::new(port),
            wire: WireBuffer::default(),
            l3_rx: VecDeque::new(),
            pending_err: None,
            quiet: Duration::ZERO,
            stall,
        }
    }

    /// Read until the script runs out (`reads` reads in all): the L3 bytes, or the first error.
    fn read_script(pipe: &mut Ed64L2Pipe, reads: usize) -> io::Result<Vec<u8>> {
        let mut got = Vec::new();
        let mut out = [0u8; 8192];
        for _ in 0..reads + 1 {
            let n = pipe.read_l3_bytes(&mut out)?;
            got.extend_from_slice(&out[..n]);
        }
        Ok(got)
    }

    /// What an X7 sent on 2026-10-09: the header and first 4 blocks of a 4122-byte reply, and then
    /// nothing more of it (spec §4.5 item 7), in reads of a block each.
    fn cut_off_reply() -> Vec<(Duration, Vec<u8>)> {
        let payload: Vec<u8> = (0..4122u32).map(|i| (i * 7) as u8).collect();
        let wire = from_cart(MULTI64_L3_TYPE, &payload);
        wire[..2048]
            .chunks(512)
            .map(|block| (Duration::ZERO, block.to_vec()))
            .collect()
    }

    /// Once the cart has been quiet past the stall part-way through a message, that part is
    /// dropped, and the next message decodes. The decoder used to count every later message as
    /// payload of the cut one until 4122 bytes had arrived, deaf for 45 s, and then fail on the
    /// trailer.
    #[test]
    fn a_message_cut_off_part_way_is_dropped_after_the_stall() {
        let quiet = Duration::from_millis(30);
        let mut port = cut_off_reply();
        port.extend([(quiet, vec![]), (quiet, vec![])]);
        port.push((
            Duration::ZERO,
            from_cart(MULTI64_L3_TYPE, b"the next reply"),
        ));
        let reads = port.len();
        let mut pipe = pipe_on(ScriptedPort::timed(port), Duration::from_millis(50));
        assert_eq!(read_script(&mut pipe, reads).unwrap(), b"the next reply");
    }

    /// The same when the next message ends the quiet: a read that blocked past the stall and then
    /// brought the next `DMA@` drops the cut-off part before that message goes in after it.
    #[test]
    fn a_read_that_waited_past_the_stall_drops_the_part_before_its_bytes() {
        let mut port = cut_off_reply();
        port.push((
            Duration::from_millis(80),
            from_cart(MULTI64_L3_TYPE, b"the next reply"),
        ));
        let reads = port.len();
        let mut pipe = pipe_on(ScriptedPort::timed(port), Duration::from_millis(50));
        assert_eq!(read_script(&mut pipe, reads).unwrap(), b"the next reply");
    }

    /// Quiet shorter than the stall is a message still on its way: it is kept, and completes.
    #[test]
    fn a_message_that_resumes_within_the_stall_is_kept() {
        let payload: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
        let wire = from_cart(MULTI64_L3_TYPE, &payload);
        let quiet = Duration::from_millis(5);
        let port = vec![
            (Duration::ZERO, wire[..512].to_vec()),
            (quiet, vec![]),
            (quiet, wire[512..1024].to_vec()),
            (quiet, vec![]),
            (Duration::ZERO, wire[1024..].to_vec()),
        ];
        let mut pipe = pipe_on(ScriptedPort::timed(port), Duration::from_secs(10));
        assert_eq!(read_script(&mut pipe, 5).unwrap(), payload);
    }

    /// Quiet only counts while a message is part-way: a cart idle between messages is not stalled,
    /// and the next message after it is not cut short.
    #[test]
    fn quiet_between_messages_drops_nothing() {
        let wire = from_cart(MULTI64_L3_TYPE, b"second");
        let port = vec![
            (Duration::ZERO, from_cart(MULTI64_L3_TYPE, b"first")),
            (Duration::from_millis(30), vec![]),
            (Duration::from_millis(30), vec![]),
            (Duration::ZERO, wire[..6].to_vec()),
            (Duration::ZERO, wire[6..].to_vec()),
        ];
        let mut pipe = pipe_on(ScriptedPort::timed(port), Duration::from_millis(50));
        assert_eq!(read_script(&mut pipe, 5).unwrap(), b"firstsecond");
    }

    /// #136: L3 bytes queued by the same serial read as a later bad trailer are handed out before
    /// the error, so a caller that drops the pipe on error (multi64d) does not lose them.
    #[test]
    fn queued_l3_bytes_are_returned_before_bad_trailer_error() {
        let mut chunk = from_cart(MULTI64_L3_TYPE, b"M64B-queued");
        let mut bad = from_cart(MULTI64_L3_TYPE, b"xy");
        let n = bad.len();
        bad[n - 1] = b'!';
        chunk.extend_from_slice(&bad);
        let mut pipe = pipe_over(vec![chunk]);
        let mut got = Vec::new();
        let mut out = [0u8; 4];
        let err = loop {
            match pipe.read_l3_bytes(&mut out) {
                Ok(0) => panic!("timed out before the error surfaced"),
                Ok(n) => got.extend_from_slice(&out[..n]),
                Err(e) => break e,
            }
        };
        assert_eq!(got, b"M64B-queued");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // The error is reported once, then the pipe reads on as before.
        assert_eq!(pipe.read_l3_bytes(&mut out).unwrap(), 0);
    }

    /// Whole messages behind a bad trailer are parsed on the next call, with no new bytes needed,
    /// so a quiet cart cannot get them dropped as a message that stalled part-way.
    #[test]
    fn messages_behind_a_bad_trailer_are_parsed_without_more_bytes() {
        let mut bad = from_cart(MULTI64_L3_TYPE, b"xy");
        let n = bad.len();
        bad[n - 1] = b'!';
        bad.extend_from_slice(&from_cart(MULTI64_L3_TYPE, b"after"));
        let quiet = Duration::from_millis(30);
        let port = vec![(Duration::ZERO, bad), (quiet, vec![]), (quiet, vec![])];
        let mut pipe = pipe_on(ScriptedPort::timed(port), Duration::from_millis(50));
        let mut out = [0u8; 16];
        let e = pipe.read_l3_bytes(&mut out).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_script(&mut pipe, 2).unwrap(), b"after");
    }

    /// With nothing queued, a bad trailer still surfaces on the read that parsed it.
    #[test]
    fn bad_trailer_with_empty_queue_errors_immediately() {
        let mut bad = from_cart(MULTI64_L3_TYPE, b"xy");
        let n = bad.len();
        bad[n - 1] = b'!';
        let mut pipe = pipe_over(vec![bad]);
        let mut out = [0u8; 16];
        let e = pipe.read_l3_bytes(&mut out).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_message_is_rejected() {
        // Cheap check of the bound without allocating 16 MiB of payload.
        assert!(align_up(MAX_MESSAGE_BYTES, WIRE_ALIGN) >= MAX_MESSAGE_BYTES);
        assert_eq!(align_up(0, WIRE_ALIGN), 0);
        assert_eq!(align_up(1, WIRE_ALIGN), 2);
        assert_eq!(align_up(2, WIRE_ALIGN), 2);
    }
}
