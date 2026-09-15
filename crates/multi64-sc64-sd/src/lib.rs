//! Host-side **SD card contents** over USB serial: **FAT12/16/32** ([`fatfs`](https://docs.rs/fatfs)) and **exFAT** ([`hadris-fat`](https://docs.rs/hadris-fat); see workspace `Cargo.toml` `[patch.crates-io]`).
//!
//! # SummerCart64
//!
//! Uses the vendor USB serial protocol
//! ([`SD_CARD_OP`](https://github.com/Polprzewodnikowy/SummerCart64/blob/main/docs/03_usb_interface.md),
//! [`SD_READ`](https://github.com/Polprzewodnikowy/SummerCart64/blob/main/docs/03_usb_interface.md),
//! [`MEMORY_READ`](https://github.com/Polprzewodnikowy/SummerCart64/blob/main/docs/03_usb_interface.md)).
//! For **`SD_CARD_OP` `arg1`**, match **`sc64deployer`** (`sw/deployer/src/sc64/types.rs` `SdCardOp → [arg0,arg1]`), not the row order in the markdown “Available SD card operations” table.
//!
//! # EverDrive X-series (optional `ed64` feature)
//!
//! Experimental: issues Krikzz-style **`RomRead`** over USB serial at a configurable linear base (`base + LBA·512`). `RomRead` reads cart ROM memory, not the SD card, so this is not expected to list the card; see workspace `docs/spec/ed64-sd-usb-host.md`. With **`ed64`**, see `Ed64RomLinear` and `Ed64SdSession` in this crate.
//!
//! # EverDrive-64 PRO (optional `ed64pro` feature)
//!
//! **Experimental; never run against a cart.** File-level, not sector-level: the PRO's microcontroller owns the
//! file system and serves file commands over edlink Gen3, so [`Ed64ProSdSession`] maps each operation onto those
//! commands instead of mounting FAT. See workspace `docs/spec/ed64-pro-usb-host.md`.
//!
//! # Unified API
//!
//! [`CartSession`] dispatches to [`Sc64SdSession`] or [`Ed64SdSession`] so UIs and tools can share one code path.
//!
//! This crate does **not** use USB mass storage / a Windows drive letter.
//!
//! # Unsafe code
//!
//! None. exFAT delete, rename and size fixes find a file's entry set by scanning its folder for
//! the file's on-disk fields (see `partition::exfat_locate_entry_set`) rather than reading
//! hadris's private `entry_offset` out of `ExFatFileEntry`'s memory layout.

mod cart_session;
#[cfg(feature = "ed64")]
mod ed64_linear;
mod link;
mod mem_disk;
mod partition;

pub use cart_session::CartSession;
#[cfg(feature = "ed64pro")]
mod ed64pro;
#[cfg(feature = "ed64")]
pub use ed64_linear::Ed64RomLinear;
#[cfg(feature = "ed64pro")]
pub use ed64pro::Ed64ProSdSession;
pub use link::{Sc64Link, SdCardTransport, SD_CARD_BUFFER_ADDR, SD_CARD_BUFFER_MAX_BYTES};
#[cfg(feature = "ed64")]
pub use partition::Ed64SdSession;
pub use partition::{cart_path_parts, Sc64SdSession, SessionEntry};
