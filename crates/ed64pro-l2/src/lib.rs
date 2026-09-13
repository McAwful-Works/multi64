//! EverDrive-64 **PRO** L2 adapter: carries the L3 octet stream over Krikzz's edlink Gen3 link.
//!
//! The wire contract is **`docs/spec/l3-over-everdrive-pro.md`**:
//!
//! - **Host → ROM:** L3 octets are written to the cart FIFO (`FifoWR`, `ed64-pro-usb-host.md` §8),
//!   which the ROM drains through its `FIFODATA` register.
//! - **ROM → host:** the ROM sends with `ed_usb_wr`, and the bytes arrive raw on the serial stream
//!   (`ed64-pro-usb-host.md` §9).
//!
//! Nothing is added in either direction. L3 finds its own frame boundaries (`MAGIC` +
//! `PAYLOAD_LEN`), and an L2 adapter must not interpret it (`l2-link-adapter.md` §2).
//!
//! # Flow control
//!
//! The ROM's FIFO holds **2048 bytes**, and nothing tells the host how much the ROM has drained:
//! Krikzz's own sample simply says not to send more until the previous data is read. So writes
//! are split into [`DEFAULT_FIFO_CHUNK`]-byte FIFO writes spaced at least [`DEFAULT_CHUNK_GAP`]
//! apart — two N64 frames, for a ROM that drains once per frame. That is a guess at a safe rate,
//! not a guarantee; spec §5 lists it as the first thing to measure.
//!
//! # Validation status
//!
//! **This has never been run against an EverDrive-64 PRO**, and unlike the X7 mapping there is no
//! reference implementation to transcribe: neither libdragon nor UNFLoader supports the PRO. The
//! mapping is this repository's design on top of Krikzz's MIT-licensed edlink and ed64-pro-pub
//! sources. [`Ed64ProL2Pipe::open`] does check identity — the edlink handshake validates the
//! status key, protocol ID and device ID — so an open pipe is at least talking to a PRO.
//!
//! [`Ed64ProL2Pipe`] mirrors `multi64_sc64_l2::Sc64L2Pipe`'s surface so `multi64d` can drive any
//! cart through one enum.

#![forbid(unsafe_code)]

use multi64_ed64pro_link::{Ed64Pro, Error, Transport};
use serialport::SerialPort;
use std::io;
use std::time::{Duration, Instant};

/// Bytes the ROM's FIFO holds (ed64-pro-pub `edio/appmain.c`: "FIFO size is only 2048 bytes").
pub const ROM_FIFO_CAPACITY: usize = 2048;

/// Largest single FIFO write. Half the FIFO, so one chunk still fits when the ROM has fallen a
/// little behind.
pub const DEFAULT_FIFO_CHUNK: usize = 1024;

// One chunk must always fit the ROM's FIFO. Checked at compile time.
const _: () = assert!(DEFAULT_FIFO_CHUNK <= ROM_FIFO_CAPACITY);

/// Minimum spacing between FIFO writes: two frames at 60 Hz, for a ROM that drains once a frame.
pub const DEFAULT_CHUNK_GAP: Duration = Duration::from_millis(34);

/// How often [`Ed64ProL2Pipe::read_l3_bytes`] looks for ROM output while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Read timeout until [`Ed64ProL2Pipe::set_timeout`] says otherwise.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(100);

fn io_error(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::other(other),
    }
}

/// Host-side pipe: L3 octets in and out of an EverDrive-64 PRO over its USB serial link.
pub struct Ed64ProL2Pipe<T: Transport = Box<dyn SerialPort>> {
    dev: Ed64Pro<T>,
    read_timeout: Duration,
    chunk_gap: Duration,
    /// Earliest time the next FIFO write may go out. Kept across calls, so two writes in quick
    /// succession are spaced like the chunks of one.
    next_write_at: Option<Instant>,
}

impl Ed64ProL2Pipe {
    /// Open the port at the PRO's fixed 921600 baud and run the edlink handshake.
    ///
    /// Fails unless the device answers as an EverDrive-64 PRO.
    pub fn open(port_name: &str) -> io::Result<Self> {
        Self::from_device(Ed64Pro::open(port_name).map_err(io_error)?)
    }
}

impl<T: Transport> Ed64ProL2Pipe<T> {
    /// Wrap a device that has already completed the handshake. Discards any input already
    /// waiting: it predates this stream.
    pub fn from_device(mut dev: Ed64Pro<T>) -> io::Result<Self> {
        dev.discard_input().map_err(io_error)?;
        Ok(Self {
            dev,
            read_timeout: DEFAULT_READ_TIMEOUT,
            chunk_gap: DEFAULT_CHUNK_GAP,
            next_write_at: None,
        })
    }

    /// Change the spacing between FIFO writes (tests, or tuning once a cart has been measured).
    pub fn with_chunk_gap(mut self, gap: Duration) -> Self {
        self.chunk_gap = gap;
        self
    }

    /// How long [`read_l3_bytes`](Self::read_l3_bytes) waits for ROM output before returning `0`.
    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.read_timeout = t;
        Ok(())
    }

    /// Drop ROM output that has arrived but not been read.
    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        self.dev.discard_input().map_err(io_error)
    }

    /// Send L3 octets to the ROM in [`DEFAULT_FIFO_CHUNK`]-byte FIFO writes.
    ///
    /// Blocks for the chunk spacing, so a large write takes about
    /// `len / DEFAULT_FIFO_CHUNK * DEFAULT_CHUNK_GAP`.
    pub fn write_l3_stream(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write_l3_stream_with_max(buf, DEFAULT_FIFO_CHUNK)
    }

    /// Same as [`write_l3_stream`](Self::write_l3_stream) with an explicit chunk size, capped at
    /// [`ROM_FIFO_CAPACITY`].
    pub fn write_l3_stream_with_max(&mut self, buf: &[u8], max_chunk: usize) -> io::Result<()> {
        let max_chunk = max_chunk.clamp(1, ROM_FIFO_CAPACITY);
        for chunk in buf.chunks(max_chunk) {
            if let Some(at) = self.next_write_at {
                let now = Instant::now();
                if at > now {
                    std::thread::sleep(at - now);
                }
            }
            self.dev.fifo_write(chunk).map_err(io_error)?;
            self.next_write_at = Some(Instant::now() + self.chunk_gap);
        }
        Ok(())
    }

    /// Read ROM output. Returns `0` when nothing arrives within the timeout, matching
    /// `Sc64L2Pipe`.
    pub fn read_l3_bytes(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let start = Instant::now();
        loop {
            let n = self.dev.usb_read(out).map_err(io_error)?;
            if n > 0 {
                // Same target shape as the other pipes, so `multi64d --serial-trace` shows any cart.
                tracing::trace!(
                    target: "multi64_ed64pro_l2",
                    raw_bytes = n,
                    "serial read from cart"
                );
                return Ok(n);
            }
            if start.elapsed() >= self.read_timeout {
                return Ok(0);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Read until `out` is filled, polling until `deadline` elapses (matches the other pipes).
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
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            off += n;
        }
        Ok(())
    }

    pub fn into_device(self) -> Ed64Pro<T> {
        self.dev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use multi64_ed64pro_link::fake::FakeEd64Pro;

    fn pipe() -> Ed64ProL2Pipe<FakeEd64Pro> {
        let dev = Ed64Pro::connect(FakeEd64Pro::new()).expect("handshake");
        Ed64ProL2Pipe::from_device(dev)
            .expect("pipe")
            .with_chunk_gap(Duration::ZERO)
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 13 + 7) as u8).collect()
    }

    #[test]
    fn writes_reach_the_fifo_unchanged_and_in_order() {
        let data = pattern(5000);
        let mut p = pipe();
        p.write_l3_stream(&data).unwrap();
        p.write_l3_stream(b"M64B").unwrap();
        let fake = p.into_device().into_inner();
        let mut expect = data;
        expect.extend_from_slice(b"M64B");
        assert_eq!(fake.fifo_received(), &expect[..]);
    }

    #[test]
    fn rom_output_is_returned_raw() {
        let mut p = pipe();
        p.dev.transport_mut().push_usb(b"M64B\x10\x00\x00\x01");
        let mut out = [0u8; 64];
        let n = p.read_l3_bytes(&mut out).unwrap();
        assert_eq!(&out[..n], b"M64B\x10\x00\x00\x01");
    }

    #[test]
    fn a_read_takes_no_more_than_fits() {
        let mut p = pipe();
        p.dev.transport_mut().push_usb(b"abcdef");
        let mut out = [0u8; 4];
        assert_eq!(p.read_l3_bytes(&mut out).unwrap(), 4);
        assert_eq!(&out, b"abcd");
        assert_eq!(p.read_l3_bytes(&mut out).unwrap(), 2);
        assert_eq!(&out[..2], b"ef");
    }

    #[test]
    fn an_exact_read_fills_the_buffer_or_times_out() {
        let mut p = pipe();
        p.set_timeout(Duration::ZERO).unwrap();
        p.dev.transport_mut().push_usb(b"M64B");
        let mut out = [0u8; 4];
        p.read_l3_bytes_exact(&mut out, Duration::from_millis(50))
            .unwrap();
        assert_eq!(&out, b"M64B");

        let err = p
            .read_l3_bytes_exact(&mut [0u8; 4], Duration::from_millis(5))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_silent_rom_times_out_with_zero() {
        let mut p = pipe();
        p.set_timeout(Duration::from_millis(5)).unwrap();
        let start = Instant::now();
        assert_eq!(p.read_l3_bytes(&mut [0u8; 8]).unwrap(), 0);
        assert!(start.elapsed() >= Duration::from_millis(5));
    }

    #[test]
    fn output_waiting_before_the_pipe_opens_is_discarded() {
        let mut dev = Ed64Pro::connect(FakeEd64Pro::new()).unwrap();
        dev.transport_mut().push_usb(b"stale");
        let mut p = Ed64ProL2Pipe::from_device(dev).unwrap();
        p.set_timeout(Duration::ZERO).unwrap();
        assert_eq!(p.read_l3_bytes(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn clearing_drops_pending_rom_output() {
        let mut p = pipe();
        p.dev.transport_mut().push_usb(b"old");
        p.clear_serial_buffers().unwrap();
        p.set_timeout(Duration::ZERO).unwrap();
        assert_eq!(p.read_l3_bytes(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn chunks_are_spaced_by_the_gap_even_across_calls() {
        let gap = Duration::from_millis(15);
        let mut p = pipe().with_chunk_gap(gap);
        let start = Instant::now();
        // Three chunks in one call, then a fourth in a second call: three gaps in all.
        p.write_l3_stream_with_max(&pattern(3000), 1024).unwrap();
        p.write_l3_stream_with_max(b"more", 1024).unwrap();
        assert!(start.elapsed() >= gap * 3, "{:?}", start.elapsed());
        let fake = p.into_device().into_inner();
        assert_eq!(fake.fifo_received().len(), 3004);
    }

    #[test]
    fn opening_a_missing_port_is_an_error() {
        assert!(Ed64ProL2Pipe::open("multi64-test-no-such-serial-port").is_err());
    }
}
