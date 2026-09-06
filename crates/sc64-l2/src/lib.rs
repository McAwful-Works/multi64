//! SummerCart64 **L2** adapter: maps the abstract L3 octet stream to host **`USB_WRITE`** commands and parses **`PKT`** id **`U`** (**DATA**) into the same stream.
//!
//! Use [`Sc64L2Pipe`] for blocking I/O over a serial port. The wire format is defined in **`docs/spec/l3-over-sc64.md`**.
//!
//! # Unsafe code
//!
//! This crate contains **no** `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

use multi64_sc64_link::{
    pkt_u_multi64_l3_payload, usb_write_l3_stream, WireBuffer, WireEvent, DEFAULT_USB_WRITE_CHUNK,
};
use serialport::ClearBuffer;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

/// Feed parsed wire events into an L3 byte queue (`PKT` `U` with multi64 datatype only).
pub fn process_wire_events(wire: &mut WireBuffer, l3_rx: &mut VecDeque<u8>) -> io::Result<()> {
    while let Some(ev) = wire.next_event() {
        match ev {
            WireEvent::Cmp(_) => {}
            WireEvent::Pkt(p) if p.id == b'U' => {
                if let Some(l3) = pkt_u_multi64_l3_payload(&p.data) {
                    l3_rx.extend(l3);
                } else if !p.data.is_empty() {
                    let head = &p.data[..p.data.len().min(8)];
                    tracing::debug!(
                        target: "multi64_sc64_l2",
                        pkt_len = p.data.len(),
                        ?head,
                        "PKT U inner not MULTI64_L3 (ignored)"
                    );
                }
            }
            WireEvent::Pkt(p) if p.id == b'G' => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "SC64 DATA_FLUSHED (PC USB_WRITE not acknowledged by N64 in time)",
                ));
            }
            WireEvent::Pkt(_) => {}
        }
    }
    Ok(())
}

/// Host-side pipe: TX via `USB_WRITE` chunks, RX via `PKT` id `U` with multi64 L3 inner format.
pub struct Sc64L2Pipe {
    port: Box<dyn serialport::SerialPort>,
    wire: WireBuffer,
    l3_rx: VecDeque<u8>,
}

impl Sc64L2Pipe {
    /// Open the serial device (USB-CDC). `baud` is required by the API; SC64 often ignores it.
    pub fn open(port_name: &str, baud: u32) -> serialport::Result<Self> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(100))
            .open()?;
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

    /// Clear host serial buffers and internal L2 parse state (use before a test if the link had noise).
    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        self.port
            .clear(ClearBuffer::All)
            .map_err(io::Error::other)?;
        self.wire = WireBuffer::default();
        self.l3_rx.clear();
        Ok(())
    }

    /// Send raw L3 octets to the N64 using `USB_WRITE` fragmentation (`docs/spec/l3-over-sc64.md`).
    pub fn write_l3_stream(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write_l3_stream_with_max(buf, DEFAULT_USB_WRITE_CHUNK)
    }

    /// Same as [`write_l3_stream`](Self::write_l3_stream) with an explicit chunk size (tests / tuning).
    pub fn write_l3_stream_with_max(&mut self, buf: &[u8], max_chunk: usize) -> io::Result<()> {
        for pkt in usb_write_l3_stream(buf, max_chunk.max(1)) {
            self.port.write_all(&pkt)?;
        }
        self.port.flush()?;
        Ok(())
    }

    /// Read L3 octets **from the N64** (async `PKT` `U` with `MULTI64_L3` datatype). Returns `0` on timeout if the queue is empty.
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
                    if n > 0 {
                        tracing::trace!(
                            target: "multi64_sc64_l2",
                            raw_bytes = n,
                            "serial read from cart"
                        );
                    }
                    self.wire.push_bytes(&scratch[..n]);
                    process_wire_events(&mut self.wire, &mut self.l3_rx)?;
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
    use multi64_sc64_link::MULTI64_L3_TYPE;

    fn encode_fake_pkt_u(l3: &[u8]) -> Vec<u8> {
        let mut inner = Vec::with_capacity(4 + l3.len());
        inner.push(MULTI64_L3_TYPE);
        let len = l3.len() as u32;
        inner.push(((len >> 16) & 0xFF) as u8);
        inner.push(((len >> 8) & 0xFF) as u8);
        inner.push((len & 0xFF) as u8);
        inner.extend_from_slice(l3);
        let mut pkt = Vec::new();
        pkt.extend_from_slice(b"PKT");
        pkt.push(b'U');
        pkt.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        pkt.extend_from_slice(&inner);
        pkt
    }

    #[test]
    fn process_wire_pkt_u_to_l3_queue() {
        let mut wire = WireBuffer::default();
        let mut q = VecDeque::new();
        wire.push_bytes(&encode_fake_pkt_u(&[0x4D, 0x36, 0x34, 0x42]));
        process_wire_events(&mut wire, &mut q).unwrap();
        assert_eq!(q.len(), 4);
        let collected: Vec<u8> = q.into_iter().collect();
        assert_eq!(collected, vec![0x4D, 0x36, 0x34, 0x42]);
    }

    #[test]
    fn data_flushed_is_error() {
        let mut wire = WireBuffer::default();
        let mut q = VecDeque::new();
        let mut pkt = Vec::new();
        pkt.extend_from_slice(b"PKT");
        pkt.push(b'G');
        pkt.extend_from_slice(&0u32.to_be_bytes());
        wire.push_bytes(&pkt);
        let e = process_wire_events(&mut wire, &mut q).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
    }
}
