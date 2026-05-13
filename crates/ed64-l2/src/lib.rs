//! EverDrive **64 X7** L2 adapter — **stub**.
//!
//! The goal is the same as [`multi64_sc64_l2`]: expose an ordered **L3 octet stream** to the host.
//! The normative USB mapping is not implemented yet; see **`docs/spec/l3-over-everdrive-x7.md`**
//! and **`crates/ed64-l2/README.md`** (repository paths).
//!
//! [`Ed64L2Pipe`] mirrors **`Sc64L2Pipe`** (`multi64-sc64-l2`)'s method surface so **`ed64-echo-test`** and
//! **`ed64-l3-framing-e2e`** can link against this crate; [`Ed64L2Pipe::open`] returns
//! [`io::ErrorKind::Unsupported`] until the mapping is implemented.
//!
//! # Unsafe code
//!
//! This crate contains **no** `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

use std::io;
use std::time::Duration;

fn not_implemented() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "EverDrive X7 L2 is not implemented yet. See docs/spec/l3-over-everdrive-x7.md \
         and crates/ed64-l2/README.md for contributor notes.",
    )
}

/// Placeholder for a future ED64 L2 stream handle (same conceptual role as `multi64_sc64_l2::Sc64L2Pipe`).
pub struct Ed64L2Pipe {
    _p: (),
}

impl Ed64L2Pipe {
    /// Opens the USB serial device for EverDrive X7 L3 streaming.
    ///
    /// **Not implemented.** Returns [`io::ErrorKind::Unsupported`] until the mapping in
    /// **`docs/spec/l3-over-everdrive-x7.md`** is implemented in this crate.
    pub fn open(_device: &str, _baud: u32) -> io::Result<Self> {
        Err(not_implemented())
    }

    /// Test-only instance for API surface checks (not a real USB link).
    #[cfg(test)]
    fn stub() -> Self {
        Ed64L2Pipe { _p: () }
    }

    /// Apply a new read timeout to the underlying port (stub: always unsupported).
    pub fn set_timeout(&mut self, _t: Duration) -> io::Result<()> {
        Err(not_implemented())
    }

    /// Clear host serial buffers and internal L2 parse state (stub: always unsupported).
    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        Err(not_implemented())
    }

    /// Send raw L3 octets to the N64 (stub: always unsupported).
    pub fn write_l3_stream(&mut self, _buf: &[u8]) -> io::Result<()> {
        Err(not_implemented())
    }

    /// Read L3 octets from the N64 (stub: always unsupported).
    pub fn read_l3_bytes(&mut self, _out: &mut [u8]) -> io::Result<usize> {
        Err(not_implemented())
    }

    /// Read until `out` is filled, polling until `deadline` elapses (stub: always unsupported).
    pub fn read_l3_bytes_exact(&mut self, _out: &mut [u8], _deadline: Duration) -> io::Result<()> {
        Err(not_implemented())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn open_returns_unsupported() {
        match Ed64L2Pipe::open("COM1", 115_200) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported"),
        }
    }

    #[test]
    fn stub_surface_returns_unsupported() {
        let mut p = Ed64L2Pipe::stub();
        assert_eq!(
            p.set_timeout(Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            p.clear_serial_buffers().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            p.write_l3_stream(&[1]).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let mut b = [0u8; 1];
        assert_eq!(
            p.read_l3_bytes(&mut b).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            p.read_l3_bytes_exact(&mut b, Duration::from_millis(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
