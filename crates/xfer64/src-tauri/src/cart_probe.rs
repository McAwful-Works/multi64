//! Serial probes that tell the carts apart when the user selects **Auto** in Settings, in order:
//!
//! 1. SummerCart64 `IDENTIFIER_GET`;
//! 2. the **EverDrive-64 PRO**'s edlink handshake, which checks a status key, protocol ID and device ID
//!    ([`docs/spec/ed64-pro-usb-host.md`](../../../docs/spec/ed64-pro-usb-host.md) §4);
//! 3. the X-series `usb64` `cmd` + `t` test ([`docs/spec/l3-over-everdrive-x7.md`](../../../docs/spec/l3-over-everdrive-x7.md) §8).
//!
//! The PRO goes before the X-series because its handshake is the stronger identity check — and edlink
//! itself sends that handshake to every port it scans.

use multi64_sc64_sd::Sc64Link;
use serialport::{ClearBuffer, SerialPort};
use std::io;
use std::io::Write;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetectedCartKind {
    Sc64,
    /// EverDrive-64 PRO (experimental).
    Ed64Pro,
    Ed64Beta,
    Unknown,
}

impl DetectedCartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectedCartKind::Sc64 => "sc64",
            DetectedCartKind::Ed64Pro => "ed64pro",
            DetectedCartKind::Ed64Beta => "ed64",
            DetectedCartKind::Unknown => "unknown",
        }
    }
}

/// SC64 `IDENTIFIER_GET`, then the EverDrive-64 PRO handshake, then the X-series `usb64` test.
pub fn probe_serial_cart(port: &str) -> DetectedCartKind {
    if probe_sc64_identify(port) {
        return DetectedCartKind::Sc64;
    }
    if probe_ed64pro_handshake(port) {
        return DetectedCartKind::Ed64Pro;
    }
    if probe_ed64_test_connection(port) {
        return DetectedCartKind::Ed64Beta;
    }
    DetectedCartKind::Unknown
}

fn probe_sc64_identify(port: &str) -> bool {
    let mut link = match Sc64Link::open(port, 115200) {
        Ok(l) => l,
        Err(_) => return false,
    };
    link.identify().is_ok()
}

/// The full edlink connection sequence at 921600 baud; succeeds only for an EverDrive-64 PRO.
fn probe_ed64pro_handshake(port: &str) -> bool {
    multi64_ed64pro_link::Ed64Pro::open(port).is_ok()
}

fn probe_ed64_test_connection(port: &str) -> bool {
    let mut port_handle = match serialport::new(port, 115200)
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
    match read_ed64_response(&mut *port_handle) {
        Ok(buf) => buf.len() >= 4 && matches!(buf[3], b'k' | b'r'),
        Err(_) => false,
    }
}

fn read_ed64_response(port: &mut dyn SerialPort) -> io::Result<Vec<u8>> {
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
