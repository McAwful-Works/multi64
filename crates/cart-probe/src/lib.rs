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
//! **On an FT245R (FTDI `0403:6001`) the X-series test goes first** (#140). The other probes put 86
//! bytes into the device's receive FIFO before the X7's 16-byte test, and clearing the port only
//! clears the PC's side. If the X7 firmware reads commands in 16-byte blocks, 86 bytes leave the
//! test straddling a block boundary and a real X7 is never found. On that chip the X7 test is
//! sent into an empty FIFO; every other port keeps the order above. This is a precaution: the X7
//! firmware's framing has never been observed, and the X7 probe has never run against a cart.
//!
//! The EverDrive probes follow `docs/spec/ed64-pro-usb-host.md` §4 and
//! `docs/spec/l3-over-everdrive-x7.md` §8, and have **never been run against a cart**.

use multi64_ed64pro_link::{Ed64Pro, Transport, BAUD};
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

/// FTDI FT245R, the USB bridge on an EverDrive-64 X7.
///
/// A stock FTDI part used by countless ordinary USB serial adapters, so it identifies nothing on its
/// own. It only decides which probe a port gets first; see [`probe_order`].
pub const ED64_X7_USB_VID: u16 = 0x0403;
pub const ED64_X7_USB_PID: u16 = 0x6001;

/// Whether a USB serial device uses the X7's USB bridge chip. Not a cart match: see
/// [`ED64_X7_USB_PID`].
pub fn usb_is_x7_bridge(usb: &UsbPortInfo) -> bool {
    usb.vid == ED64_X7_USB_VID && usb.pid == ED64_X7_USB_PID
}

/// Which carts to ask for, and in what order, on a port whose USB bridge is (or is not) the X7's
/// FT245R. Every cart is still asked; only the order changes. See the [crate docs](crate).
pub fn probe_order(x7_bridge: bool) -> [DetectedCart; 3] {
    if x7_bridge {
        [
            DetectedCart::Ed64,
            DetectedCart::Sc64,
            DetectedCart::Ed64Pro,
        ]
    } else {
        [
            DetectedCart::Sc64,
            DetectedCart::Ed64Pro,
            DetectedCart::Ed64,
        ]
    }
}

/// Whether `port` is enumerated as a USB serial device on the X7's bridge chip. A port that is not
/// enumerated, or whose enumeration fails, is treated as not — which keeps the usual order.
fn port_is_x7_bridge(port: &str) -> bool {
    serialport::available_ports()
        .map(|ports| {
            ports.iter().any(|p| {
                p.port_name.eq_ignore_ascii_case(port)
                    && matches!(&p.port_type, serialport::SerialPortType::UsbPort(u) if usb_is_x7_bridge(u))
            })
        })
        .unwrap_or(false)
}

/// How long a port gets to answer `IDENTIFIER_GET`. A SummerCart64 answers within milliseconds;
/// this bounds what every other device costs a scan.
const SC64_IDENTIFY_TIMEOUT: Duration = Duration::from_millis(1000);
/// How long a port gets to answer the X-series `usb64` test.
const ED64_TEST_TIMEOUT: Duration = Duration::from_millis(2000);
/// Read timeout for a single `read` while waiting on either of the above.
const READ_SLICE: Duration = Duration::from_millis(100);
/// The port timeout [`Ed64Pro::open`] opens with. Its handshake sets the same value before its first
/// write, so this only keeps the probe's port set up exactly as the link's own.
const ED64PRO_OPEN_TIMEOUT: Duration = Duration::from_millis(200);

/// Identify the cart on `port` by asking it, or `None` if nothing answered as a cart.
///
/// Writes to the port; see the [crate docs](crate). A port that cannot be opened (absent, or held
/// by another process) is `None`. Worst case, a port with no cart costs about three seconds.
pub fn probe_port(port: &str) -> Option<DetectedCart> {
    probe_port_cancellable(port, || false)
}

/// [`probe_port`], giving up as soon as `cancelled` returns true: `None`, whatever is on the port.
///
/// `cancelled` is checked before each probe and at every read while waiting for an answer, so a
/// cancel takes effect within one read timeout rather than after the rest of the port's probes:
/// 100 ms for the SummerCart64 and X-series probes, 200 ms during the PRO's edlink handshake.
pub fn probe_port_cancellable(port: &str, cancelled: impl Fn() -> bool) -> Option<DetectedCart> {
    if cancelled() {
        return None;
    }
    let order = probe_order(port_is_x7_bridge(port));
    first_answering(order, &cancelled, |cart| match cart {
        DetectedCart::Sc64 => probe_sc64(port, &cancelled),
        DetectedCart::Ed64Pro => probe_ed64pro(port, &cancelled),
        DetectedCart::Ed64 => probe_ed64(port, &cancelled),
    })
}

/// Ask each cart in `order` until one answers, checking `cancelled` before each.
fn first_answering(
    order: [DetectedCart; 3],
    cancelled: &impl Fn() -> bool,
    mut asks: impl FnMut(DetectedCart) -> bool,
) -> Option<DetectedCart> {
    for cart in order {
        if cancelled() {
            return None;
        }
        if asks(cart) {
            return Some(cart);
        }
    }
    None
}

fn open(port: &str, baud: u32) -> Option<Box<dyn SerialPort>> {
    serialport::new(port, baud).timeout(READ_SLICE).open().ok()
}

/// SummerCart64 `IDENTIFIER_GET`, answered with an identifier starting `SC`.
fn probe_sc64(port: &str, cancelled: &impl Fn() -> bool) -> bool {
    let Some(mut p) = open(port, 115_200) else {
        return false;
    };
    let _ = p.clear(ClearBuffer::Input);
    let request = cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]);
    if p.write_all(&request).is_err() || p.flush().is_err() {
        return false;
    }
    await_sc64_identifier(&mut p, SC64_IDENTIFY_TIMEOUT, cancelled)
}

/// Read until `IDENTIFIER_GET` is answered, `timeout` passes or `cancelled` returns true.
fn await_sc64_identifier(
    p: &mut impl Read,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
) -> bool {
    let mut responses = ResponseBuffer::default();
    let mut scratch = [0u8; 256];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && !cancelled() {
        match p.read(&mut scratch) {
            // Some drivers return no bytes at once instead of waiting out the read timeout; don't
            // spin a core until the deadline.
            Ok(0) => std::thread::sleep(Duration::from_millis(1)),
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
fn probe_ed64pro(port: &str, cancelled: &impl Fn() -> bool) -> bool {
    let Ok(p) = serialport::new(port, BAUD)
        .timeout(ED64PRO_OPEN_TIMEOUT)
        .open()
    else {
        return false;
    };
    ed64pro_answers(p, cancelled)
}

/// Whether the edlink handshake on `io` identifies a PRO, giving up at the handshake's next read
/// once `cancelled` returns true. Uncancelled, the handshake sends and reads exactly what
/// [`Ed64Pro::open`] does.
fn ed64pro_answers<T: Transport>(io: T, cancelled: &impl Fn() -> bool) -> bool {
    Ed64Pro::connect(Cancellable { io, cancelled }).is_ok()
}

/// A [`Transport`] whose reads fail once `cancelled` returns true, so a handshake in progress stops
/// at its next read instead of running to the end.
struct Cancellable<'a, T, C> {
    io: T,
    cancelled: &'a C,
}

impl<T: Read, C: Fn() -> bool> Read for Cancellable<'_, T, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            // Not `Interrupted`: `read_exact` retries that.
            return Err(io::Error::other("cart probe cancelled"));
        }
        self.io.read(buf)
    }
}

impl<T: Write, C> Write for Cancellable<'_, T, C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.io.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.io.flush()
    }
}

impl<T: Transport, C: Fn() -> bool> Transport for Cancellable<'_, T, C> {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.io.set_timeout(timeout)
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.io.clear_input()
    }

    fn bytes_to_read(&mut self) -> io::Result<u32> {
        self.io.bytes_to_read()
    }
}

/// The X-series `usb64` test: `cmd` + `t` in a 16-byte packet, answered with `cmdk` or `cmdr`.
fn probe_ed64(port: &str, cancelled: &impl Fn() -> bool) -> bool {
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
    await_ed64_test_reply(&mut p, ED64_TEST_TIMEOUT, cancelled)
}

/// Read until the `usb64` test is answered, `timeout` passes or `cancelled` returns true.
fn await_ed64_test_reply(
    p: &mut impl Read,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
) -> bool {
    let mut reply = Vec::new();
    let mut scratch = [0u8; 64];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && reply.len() < 512 && !cancelled() {
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

    /// A port that never answers: every read times out at once, as a real one would after
    /// `READ_SLICE`. Counts the reads.
    struct Silent {
        reads: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Read for Silent {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    /// Exit used to wait out the whole of a port's probes (#147 follow-up): a cancel now stops the
    /// wait at the next read, well before the timeout.
    #[test]
    fn a_cancel_stops_waiting_for_an_answer_at_the_next_read() {
        type Await = fn(&mut Silent, Duration, &dyn Fn() -> bool) -> bool;
        let waits: [(&str, Await); 2] = [
            ("sc64", |p, t, c| await_sc64_identifier(p, t, &c)),
            ("ed64", |p, t, c| await_ed64_test_reply(p, t, &c)),
        ];
        for (name, wait) in waits {
            let reads = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut port = Silent {
                reads: reads.clone(),
            };
            let seen = reads.clone();
            let started = Instant::now();
            let answered = wait(&mut port, Duration::from_secs(30), &|| seen.get() >= 3);
            assert!(!answered, "{name}");
            assert_eq!(reads.get(), 3, "{name}: no read after the cancel");
            assert!(started.elapsed() < Duration::from_secs(5), "{name}");
        }
    }

    /// A fake PRO that trickles its replies one byte per read, so the handshake takes many reads.
    /// Counts them.
    struct Trickle {
        cart: multi64_ed64pro_link::fake::FakeEd64Pro,
        reads: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            let one = buf.len().min(1);
            self.cart.read(&mut buf[..one])
        }
    }

    impl Write for Trickle {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.cart.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.cart.flush()
        }
    }

    impl Transport for Trickle {
        fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
            self.cart.set_timeout(t)
        }
        fn clear_input(&mut self) -> io::Result<()> {
            self.cart.clear_input()
        }
        fn bytes_to_read(&mut self) -> io::Result<u32> {
            self.cart.bytes_to_read()
        }
    }

    fn trickle() -> (Trickle, std::rc::Rc<std::cell::Cell<usize>>) {
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        let port = Trickle {
            cart: multi64_ed64pro_link::fake::FakeEd64Pro::new(),
            reads: reads.clone(),
        };
        (port, reads)
    }

    #[test]
    fn an_uncancelled_pro_handshake_still_identifies_the_pro() {
        let (port, reads) = trickle();
        assert!(ed64pro_answers(port, &|| false));
        assert!(reads.get() >= 8, "the whole handshake ran: {}", reads.get());
    }

    /// #167: the PRO's edlink handshake used to run to the end once started, whatever the cancel
    /// flag said; it now stops at its next read.
    #[test]
    fn a_cancel_stops_the_pro_handshake_at_its_next_read() {
        let (port, reads) = trickle();
        let seen = reads.clone();
        assert!(!ed64pro_answers(port, &|| seen.get() >= 3));
        assert_eq!(reads.get(), 3, "no read after the cancel");
    }

    #[test]
    fn a_cancelled_probe_opens_nothing() {
        assert_eq!(
            probe_port_cancellable("multi64-cart-probe-no-such-port", || true),
            None
        );
    }

    /// #140: on the X7's FT245R, the X-series test goes out before any other cart's bytes.
    #[test]
    fn the_x7_test_goes_first_only_on_the_x7_bridge_chip() {
        assert_eq!(
            probe_order(true),
            [
                DetectedCart::Ed64,
                DetectedCart::Sc64,
                DetectedCart::Ed64Pro
            ]
        );
        assert_eq!(
            probe_order(false),
            [
                DetectedCart::Sc64,
                DetectedCart::Ed64Pro,
                DetectedCart::Ed64
            ],
            "every other port keeps the order it had"
        );
    }

    #[test]
    fn the_x7_bridge_is_the_ft245r_and_not_the_sc64s_ft232h() {
        assert!(usb_is_x7_bridge(&usb(0x0403, 0x6001, None, None)));
        assert!(!usb_is_x7_bridge(&usb(
            0x0403,
            0x6014,
            Some("SC64XXXXXXA"),
            None
        )));
        assert!(!usb_is_x7_bridge(&usb(0x1a86, 0x6001, None, None)));
    }

    /// The order is what the probe actually follows: the first cart asked is the first in the
    /// order, and an answer stops the rest from being asked.
    #[test]
    fn probing_asks_carts_in_order_and_stops_at_an_answer() {
        for x7 in [true, false] {
            let order = probe_order(x7);
            let mut asked = Vec::new();
            let found = first_answering(order, &|| false, |cart| {
                asked.push(cart);
                cart == order[1]
            });
            assert_eq!(found, Some(order[1]));
            assert_eq!(asked, order[..2], "x7 bridge: {x7}");
        }
    }

    #[test]
    fn a_cancel_between_probes_asks_no_further_cart() {
        let mut asked = Vec::new();
        let calls = std::cell::Cell::new(0);
        let found = first_answering(
            probe_order(true),
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 1
            },
            |cart| {
                asked.push(cart);
                false
            },
        );
        assert_eq!(found, None);
        assert_eq!(asked, [DetectedCart::Ed64], "only the first cart was asked");
    }

    #[test]
    fn names_match_the_daemon_spelling() {
        assert_eq!(DetectedCart::Sc64.as_str(), "sc64");
        assert_eq!(DetectedCart::Ed64Pro.as_str(), "ed64pro");
        assert_eq!(DetectedCart::Ed64.as_str(), "ed64");
    }
}
