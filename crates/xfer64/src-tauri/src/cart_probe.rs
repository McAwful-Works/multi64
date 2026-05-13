//! Serial probes to distinguish **SummerCart64** vs **EverDrive USB** (Krikzz **edlink** Gen3 ED64 or legacy **`usb64`**)
//! when the user selects **Auto** in Settings. See [`docs/spec/l3-over-everdrive-x7.md`](../../../docs/spec/l3-over-everdrive-x7.md) §8.

use multi64_ed64_link::probe_ed64_serial_cart;
use multi64_sc64_sd::Sc64Link;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetectedCartKind {
    Sc64,
    Ed64Beta,
    Unknown,
}

impl DetectedCartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectedCartKind::Sc64 => "sc64",
            DetectedCartKind::Ed64Beta => "ed64",
            DetectedCartKind::Unknown => "unknown",
        }
    }
}

/// Try SC64 `IDENTIFIER_GET` first, then EverDrive (**edlink** ED64 at 921600 or legacy **`usb64`** `cmd`+`t` at 115200).
pub fn probe_serial_cart(port: &str) -> DetectedCartKind {
    if probe_sc64_identify(port) {
        return DetectedCartKind::Sc64;
    }
    if probe_ed64_serial_cart(port) {
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

