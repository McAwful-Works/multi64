//! Tell the supported carts apart on a serial port, for Multi64's and Xfer64's **Auto** settings.
//!
//! Two tiers, meant to be used in this order:
//!
//! 1. [`usb_is_sc64`] judges a USB serial device from its descriptors. It writes nothing, so it is
//!    safe on any port and still works while another process (usually `multi64d`) holds the port.
//! 2. [`probe_port`] sends each cart's identity request and waits for an answer: SummerCart64
//!    `IDENTIFIER_GET`, then the EverDrive-64 PRO's edlink handshake, then the X-series `usb64`
//!    `cmd` + `t` test. Whatever is on the other end receives those bytes, and a port another
//!    process holds cannot be opened, so it reports nothing there.
//!
//! The PRO goes before the X-series because its handshake is the stronger identity check, and
//! edlink itself sends it to every port it scans. Only an SC64 has a descriptor worth matching: the
//! X7's FT245R is a stock FTDI part, and the PRO's USB descriptors are not documented.
//!
//! The EverDrive probes follow `docs/spec/ed64-pro-usb-host.md` §4 and
//! `docs/spec/l3-over-everdrive-x7.md` §8, and have **never been run against a cart**.

use multi64_sc64_link::{cmd, cmd_packet, ResponseBuffer};
use serialport::{ClearBuffer, SerialPort, UsbPortInfo};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// A cart a probe identified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectedCart {
    /// SummerCart64: the only cart proven on hardware.
    Sc64,
    /// EverDrive-64 PRO (experimental).
    Ed64Pro,
    /// EverDrive-64 X-series, e.g. the X7 (experimental).
    Ed64,
}

impl DetectedCart {
    /// The cart's short name, as `multi64d --cart` and Xfer64's detected kind spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            DetectedCart::Sc64 => "sc64",
            DetectedCart::Ed64Pro => "ed64pro",
            DetectedCart::Ed64 => "ed64",
        }
    }
}

/// FTDI FT232H, the USB bridge on a SummerCart64.
///
/// A stock FTDI part that also turns up in unrelated adapters, so the VID/PID is only half of the
/// match; see [`usb_is_sc64`].
pub const SC64_USB_VID: u16 = 0x0403;
pub const SC64_USB_PID: u16 = 0x6014;
/// The cart programs its FTDI serial number as `SC64…` and its product string as `SC64`.
const SC64_USB_TAG: &str = "SC64";

/// Whether a USB serial device is a SummerCart64, judged from its descriptors alone.
///
/// Deliberately writes nothing: until proven otherwise the port belongs to some other device,
/// opening it can reset that device (many adapters toggle DTR on open), and the cart's own port
/// may already be held by `multi64d`.
///
/// Windows reports the FTDI serial with the interface letter appended (`SC64XXXXXXA`) and the
/// driver's description (`USB Serial Port`) as the product, so there the serial prefix is what
/// matches; other platforms read the product string from the descriptor.
pub fn usb_is_sc64(usb: &UsbPortInfo) -> bool {
    let tagged = |s: &Option<String>| {
        s.as_deref()
            .and_then(|s| s.get(..SC64_USB_TAG.len()))
            .is_some_and(|head| head.eq_ignore_ascii_case(SC64_USB_TAG))
    };
    usb.vid == SC64_USB_VID
        && usb.pid == SC64_USB_PID
        && (tagged(&usb.serial_number) || tagged(&usb.product))
}

/// How long a port gets to answer `IDENTIFIER_GET`. A SummerCart64 answers within milliseconds;
/// this bounds what every other device costs a scan.
const SC64_IDENTIFY_TIMEOUT: Duration = Duration::from_millis(1000);
/// How long a port gets to answer the X-series `usb64` test.
const ED64_TEST_TIMEOUT: Duration = Duration::from_millis(2000);
/// Read timeout for a single `read` while waiting on either of the above.
const READ_SLICE: Duration = Duration::from_millis(100);

/// Identify the cart on `port` by asking it, or `None` if nothing answered as a cart.
///
/// Writes to the port; see the [crate docs](crate). A port that cannot be opened (absent, or held
/// by another process) is `None`. Worst case, a port with no cart costs about three seconds.
pub fn probe_port(port: &str) -> Option<DetectedCart> {
    if probe_sc64(port) {
        return Some(DetectedCart::Sc64);
    }
    if probe_ed64pro(port) {
        return Some(DetectedCart::Ed64Pro);
    }
    if probe_ed64(port) {
        return Some(DetectedCart::Ed64);
    }
    None
}

fn open(port: &str, baud: u32) -> Option<Box<dyn SerialPort>> {
    serialport::new(port, baud).timeout(READ_SLICE).open().ok()
}

/// SummerCart64 `IDENTIFIER_GET`, answered with an identifier starting `SC`.
fn probe_sc64(port: &str) -> bool {
    let Some(mut p) = open(port, 115_200) else {
        return false;
    };
    let _ = p.clear(ClearBuffer::Input);
    let request = cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]);
    if p.write_all(&request).is_err() || p.flush().is_err() {
        return false;
    }
    let mut responses = ResponseBuffer::default();
    let mut scratch = [0u8; 256];
    let deadline = Instant::now() + SC64_IDENTIFY_TIMEOUT;
    while Instant::now() < deadline {
        match p.read(&mut scratch) {
            Ok(0) => {}
            Ok(n) => responses.push_bytes(&scratch[..n]),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => return false,
        }
        while let Some(r) = responses.next_cmp() {
            if r.cmd_id == cmd::IDENTIFIER_GET {
                return r.ok && is_sc64_identifier(&r.data);
            }
        }
    }
    false
}

/// The full edlink connection sequence at 921600 baud; succeeds only for an EverDrive-64 PRO.
fn probe_ed64pro(port: &str) -> bool {
    multi64_ed64pro_link::Ed64Pro::open(port).is_ok()
}

/// The X-series `usb64` test: `cmd` + `t` in a 16-byte packet, answered with `cmdk` or `cmdr`.
fn probe_ed64(port: &str) -> bool {
    let Some(mut p) = open(port, 115_200) else {
        return false;
    };
    let _ = p.clear(ClearBuffer::Input);
    let mut request = [0u8; 16];
    request[..3].copy_from_slice(b"cmd");
    request[3] = b't';
    if p.write_all(&request).is_err() || p.flush().is_err() {
        return false;
    }
    let mut reply = Vec::new();
    let mut scratch = [0u8; 64];
    let deadline = Instant::now() + ED64_TEST_TIMEOUT;
    while Instant::now() < deadline && reply.len() < 512 {
        match p.read(&mut scratch) {
            Ok(0) => std::thread::sleep(Duration::from_millis(1)),
            Ok(n) => {
                reply.extend_from_slice(&scratch[..n]);
                if is_ed64_test_reply(&reply) {
                    return true;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => return false,
        }
    }
    false
}

/// Whether an `IDENTIFIER_GET` payload names a SummerCart64 (`SCv2`, …).
fn is_sc64_identifier(data: &[u8]) -> bool {
    data.starts_with(b"SC")
}

/// Whether bytes read after the `usb64` test are its answer: the fourth byte is `k` or `r`.
fn is_ed64_test_reply(reply: &[u8]) -> bool {
    reply.len() >= 4 && matches!(reply[3], b'k' | b'r')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(vid: u16, pid: u16, serial: Option<&str>, product: Option<&str>) -> UsbPortInfo {
        UsbPortInfo {
            vid,
            pid,
            serial_number: serial.map(str::to_string),
            manufacturer: None,
            product: product.map(str::to_string),
        }
    }

    /// Windows: `FTDIBUS\VID_0403+PID_6014+SC64XXXXXXA\0000`, product is the driver's description.
    #[test]
    fn matches_an_sc64_as_windows_reports_it() {
        assert!(usb_is_sc64(&usb(
            0x0403,
            0x6014,
            Some("SC64XXXXXXA"),
            Some("USB Serial Port")
        )));
    }

    /// Linux and macOS read the descriptor's product string; the serial may not come through.
    #[test]
    fn matches_on_the_product_string_too() {
        assert!(usb_is_sc64(&usb(0x0403, 0x6014, None, Some("SC64"))));
    }

    /// The cart's VID/PID is a stock FTDI part; without the SC64 tag it is just some adapter.
    #[test]
    fn a_stock_ftdi_part_without_the_tag_is_not_a_cart() {
        assert!(!usb_is_sc64(&usb(
            0x0403,
            0x6014,
            Some("FT9ABCDEA"),
            Some("USB Serial Port")
        )));
    }

    #[test]
    fn the_tag_without_the_ftdi_ids_is_not_a_cart() {
        assert!(!usb_is_sc64(&usb(
            0x0403,
            0x6001,
            Some("SC64XXXXXXA"),
            None
        )));
        assert!(!usb_is_sc64(&usb(0x1a86, 0x6014, None, Some("SC64"))));
    }

    #[test]
    fn the_tag_match_ignores_case_and_survives_short_or_non_ascii_strings() {
        assert!(usb_is_sc64(&usb(0x0403, 0x6014, Some("sc64xxxxxxa"), None)));
        for odd in ["", "SC", "SCβ4", "βSC64"] {
            assert!(
                !usb_is_sc64(&usb(0x0403, 0x6014, Some(odd), Some(odd))),
                "{odd:?} must not match (or panic on a char boundary)"
            );
        }
    }

    #[test]
    fn identity_replies_are_recognised() {
        assert!(is_sc64_identifier(b"SCv2"));
        assert!(!is_sc64_identifier(b"S"));
        assert!(!is_sc64_identifier(b"XXv2"));
        assert!(is_ed64_test_reply(b"cmdk"));
        assert!(is_ed64_test_reply(b"cmdr...."));
        assert!(!is_ed64_test_reply(b"cmd"));
        assert!(!is_ed64_test_reply(b"cmdx"));
    }

    /// A port that cannot be opened is no cart, and costs nothing.
    #[test]
    fn a_missing_port_is_not_a_cart() {
        let started = Instant::now();
        assert_eq!(probe_port("multi64-cart-probe-no-such-port"), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn names_match_the_daemon_spelling() {
        assert_eq!(DetectedCart::Sc64.as_str(), "sc64");
        assert_eq!(DetectedCart::Ed64Pro.as_str(), "ed64pro");
        assert_eq!(DetectedCart::Ed64.as_str(), "ed64");
    }
}
