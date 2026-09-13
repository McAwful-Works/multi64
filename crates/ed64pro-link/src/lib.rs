//! Host side of the **EverDrive-64 PRO** USB link: Krikzz's edlink **Gen3** protocol.
//!
//! The PRO is a different device from the X7. It does not speak `usb64` or the `DMA@` framing that
//! `multi64-ed64-link` and `multi64-ed64-l2` implement; it speaks edlink, whose cart
//! microcontroller serves file-system, memory and FIFO commands directly. The wire contract lives in
//! `docs/spec/ed64-pro-usb-host.md`.
//!
//! # Validation status
//!
//! **This has never been run against an EverDrive-64 PRO.** Every byte is transcribed from
//! Krikzz's own sources, pinned in the spec:
//!
//! - [krikzz/edlink](https://github.com/krikzz/edlink) — the PC utility (`Device/Link.cs`,
//!   `Device/DeviceIO_V2.cs`, `DEV_ED64/DeviceIO.cs`);
//! - [krikzz/ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) — the console-side library
//!   (`edio/everdrive.c`), the only Gen3 reference for the directory commands.
//!
//! A successful [`Ed64Pro::connect`] is more than an opened port — the handshake checks the status
//! key, protocol ID and device ID — but it is still not evidence that transfers work. Treat all of
//! this as experimental until someone runs it on hardware; the spec lists what to check first.
//!
//! Both upstream sources are MIT-licensed; see this crate's README for attribution.

#![forbid(unsafe_code)]

mod device;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod transport;
pub mod wire;

pub use device::{Ed64Pro, Error, Identity, Result};
pub use transport::Transport;
pub use wire::{dir_option, open_mode, Endpoint, FileInfo, BAUD, DEVICE_ID_ED64_PRO, PROTOCOL_ID};
