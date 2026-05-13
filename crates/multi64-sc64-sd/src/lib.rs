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
//! # EverDrive-64 (optional `ed64` feature)
//!
//! Uses Krikzz **edlink** Gen3 **EPO / FCI** reads when the cart reports protocol `0x07`, else legacy **`usb64` `RomRead`**
//! at `base + LBA·512` (optional base; edlink defaults to `0x10000000` per vendor `DEV_ED64/DeviceIO.cs`). See
//! `Ed64RomLinear` / `Ed64SdSession` in this crate and workspace `docs/spec/ed64-sd-usb-host.md`.
//!
//! # Unified API
//!
//! [`CartSession`] dispatches to [`Sc64SdSession`] or [`Ed64SdSession`] so UIs and tools can share one code path.
//!
//! This crate does **not** use USB mass storage / a Windows drive letter.
//!
//! # Unsafe code
//!
//! `link` and `Sc64PartitionDisk` remain free of `unsafe`. `partition` uses a small `unsafe` block
//! to view `ExFatFileEntry` as bytes while locating the on-disk entry-set offset (see
//! [`partition::exfat_entry_offset_via_disk_probe`]) for exFAT delete (hadris stores
//! directory-relative offsets in iterated entries but `delete` writes as if they were absolute).

mod cart_session;
#[cfg(feature = "ed64")]
mod ed64_linear;
mod link;
mod mem_disk;
mod partition;

pub use cart_session::CartSession;
#[cfg(feature = "ed64")]
pub use ed64_linear::Ed64RomLinear;
pub use link::{Sc64Link, SdCardTransport, SD_CARD_BUFFER_ADDR, SD_CARD_BUFFER_MAX_BYTES};
#[cfg(feature = "ed64")]
pub use partition::Ed64SdSession;
pub use partition::{cart_path_parts, Sc64SdSession, SessionEntry};
