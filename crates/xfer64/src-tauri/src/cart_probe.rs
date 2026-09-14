//! Tells the carts apart when the user selects **Auto** in Settings, over [`multi64_cart_probe`]:
//!
//! 1. a SummerCart64 by its USB descriptors, which writes nothing and still works while `multi64d`
//!    holds the port;
//! 2. otherwise the wire probes, in order: SummerCart64 `IDENTIFIER_GET`, the **EverDrive-64 PRO**'s
//!    edlink handshake ([`docs/spec/ed64-pro-usb-host.md`](../../../docs/spec/ed64-pro-usb-host.md)
//!    §4), then the X-series `usb64` `cmd` + `t` test
//!    ([`docs/spec/l3-over-everdrive-x7.md`](../../../docs/spec/l3-over-everdrive-x7.md) §8).

use multi64_cart_probe::DetectedCart;

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

impl From<Option<DetectedCart>> for DetectedCartKind {
    fn from(found: Option<DetectedCart>) -> Self {
        match found {
            Some(DetectedCart::Sc64) => DetectedCartKind::Sc64,
            Some(DetectedCart::Ed64Pro) => DetectedCartKind::Ed64Pro,
            Some(DetectedCart::Ed64) => DetectedCartKind::Ed64Beta,
            None => DetectedCartKind::Unknown,
        }
    }
}

/// Whether `port` enumerates as a SummerCart64, judged from its USB descriptors alone.
pub fn port_is_sc64_by_usb(port: &str) -> bool {
    serialport::available_ports()
        .map(|ports| {
            ports.iter().any(|p| {
                p.port_name.eq_ignore_ascii_case(port)
                    && matches!(
                        &p.port_type,
                        serialport::SerialPortType::UsbPort(usb) if multi64_cart_probe::usb_is_sc64(usb)
                    )
            })
        })
        .unwrap_or(false)
}

/// The SC64 descriptor match, then the wire probes.
///
/// The descriptor match comes first so an SC64 is found without opening its port: while
/// `multi64d` holds it, opening fails and the wire probes would report nothing.
pub fn probe_serial_cart(port: &str) -> DetectedCartKind {
    if port_is_sc64_by_usb(port) {
        return DetectedCartKind::Sc64;
    }
    multi64_cart_probe::probe_port(port).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_carts_keep_xfer64s_names() {
        let kind = |found| DetectedCartKind::from(found).as_str();
        assert_eq!(kind(Some(DetectedCart::Sc64)), "sc64");
        assert_eq!(kind(Some(DetectedCart::Ed64Pro)), "ed64pro");
        assert_eq!(kind(Some(DetectedCart::Ed64)), "ed64");
        assert_eq!(kind(None), "unknown");
    }
}
