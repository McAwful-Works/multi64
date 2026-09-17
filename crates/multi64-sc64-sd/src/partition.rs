//! FAT / exFAT mount over [`SdCardTransport`](crate::link::SdCardTransport) (SC64 `SD_READ` + `MEMORY_READ`, or EverDrive `RomRead` linear sectors with the **`ed64`** feature).

// Large exFAT workaround module: clippy -D warnings is relaxed here until a focused cleanup.
#![allow(
    clippy::single_match,
    clippy::manual_div_ceil,
    clippy::doc_lazy_continuation,
    clippy::manual_range_contains,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::incompatible_msrv,
    clippy::drop_non_drop
)]

#[cfg(feature = "ed64")]
use crate::ed64_linear::Ed64RomLinear;
use crate::link::{Sc64Link, SdCardTransport, SD_CARD_BUFFER_MAX_BYTES};
use crate::mem_disk::{PartitionDisk, RamPartitionDisk};
use fatfs::{FileAttributes as FatFileAttributes, FileSystem, FsOptions};
use hadris_common::types::endian::{Endian, LittleEndian};
use hadris_common::types::number::{U16, U32, U64};
use hadris_fat::exfat::{
    ExFatFs, ExFatTimestamp, RawFileDirectoryEntry, RawFileNameEntry, RawStreamExtensionEntry,
};
use std::io;
use std::io::prelude::*;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::SeekFrom;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Buffer size for streaming cart ↔ host file I/O (FAT/exFAT read/write loops).
/// [`crate::link::SD_CARD_BUFFER_MAX_BYTES`] caps one USB SD read/write batch; a full chunk may use two.
const STREAM_CHUNK: usize = 256 * 1024;

/// exFAT directory entry type bytes (see Microsoft exFAT / hadris `entry_type`).
/// Type-byte bit 7. Clear means the slot is unused: `0x00` (end of directory), or a deleted
/// entry such as `0x05`, `0x40`, `0x41`.
const EXFAT_ATTR_DIRECTORY: u16 = 0x10;
const EXFAT_ATTR_ARCHIVE: u16 = 0x20;
const EXFAT_ENTRY_IN_USE: u8 = 0x80;
const EXFAT_ENTRY_FILE_DIRECTORY: u8 = 0x85;
const EXFAT_ENTRY_STREAM_EXT: u8 = 0xC0;
const EXFAT_ENTRY_FILE_NAME: u8 = 0xC1;
const EXFAT_CHARS_PER_NAME_ENTRY: usize = 15;
const EXFAT_MAX_FILENAME_LEN: usize = 255;

/// One row for the Xfer64 UI.
#[derive(Clone, Debug)]
pub struct SessionEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// FAT/exFAT hidden attribute and/or leading-dot names (common on SD cards).
    pub hidden: bool,
}

/// Connected SC64 with SD initialized and FAT/exFAT partition bounds detected.
///
/// The cart holds the SD card for the PC side for as long as this session lives: until the USB
/// session is released the console refuses to boot with `SD card is locked by the PC side`.
/// [`Drop`] releases it, so a caller that never reaches [`close`](Self::close) — an early return,
/// a `?`, a panic — does not strand the card.
pub struct Sc64SdSession {
    link: Arc<Mutex<Sc64Link>>,
    pub partition_start_sector: u64,
    pub partition_bytes: u64,
    exfat: bool,
    /// Set once the USB SD session has been released, so `close()` and `Drop` together deinit once.
    released: AtomicBool,
}

/// EverDrive X-series: FAT/exFAT over **experimental** `RomRead` linear LBA mapping (`rom_linear_base + LBA·512`).
///
/// Released on [`Drop`] like [`Sc64SdSession`]; here that is a serial flush rather than an SD
/// deinit (the linear mapping takes no PC-side SD lock), but the ownership rule is the same one.
#[cfg(feature = "ed64")]
pub struct Ed64SdSession {
    link: Arc<Mutex<Ed64RomLinear>>,
    pub partition_start_sector: u64,
    pub partition_bytes: u64,
    exfat: bool,
    /// See [`Sc64SdSession::released`].
    released: AtomicBool,
}

impl Sc64SdSession {
    /// Open COM port, `SD_CARD_OP` init, detect MBR/FAT/exFAT, compute partition size.
    pub fn open(port_name: &str, baud: u32) -> io::Result<Self> {
        let mut link = Sc64Link::open(port_name, baud)
            .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
        link.set_timeout(std::time::Duration::from_millis(500))?;
        // Resync link and confirm device before touching the SD (avoids generic ERR after noise).
        let _ = link.usb_reset_link();
        link.identify()?;
        // Release any stale USB-side SD session before init (deployer uses deinit at end of transfers).
        link.sd_deinit_try();
        link.sd_init()?;

        // From here on the cart holds the SD card for the PC side. No `Self` exists yet, so a `?`
        // in the probing below would drop `link` with the lock still held and the console would
        // refuse to boot from the card. Probe in a closure and deinit before propagating.
        let probe = (|link: &mut Sc64Link| -> io::Result<(u64, bool, u64)> {
            let mut s0 = [0u8; 512];
            link.read_sd_sectors(0, &mut s0)?;
            let part_start = {
                let link_ref = &mut *link;
                detect_partition_start(&s0, |lba, buf| link_ref.read_sd_sectors(lba, buf))?
            };

            let mut bpb = [0u8; 512];
            link.read_sd_sectors(part_start, &mut bpb)?;
            let exfat = is_exfat_boot_sector(&bpb);
            let part_bytes = partition_volume_bytes(&bpb)?;
            Ok((part_start, exfat, part_bytes))
        })(&mut link);
        let (part_start, exfat, part_bytes) = match probe {
            Ok(v) => v,
            Err(e) => {
                link.sd_deinit_try();
                return Err(e);
            }
        };

        Ok(Self {
            link: Arc::new(Mutex::new(link)),
            partition_start_sector: part_start,
            partition_bytes: part_bytes,
            exfat,
            released: AtomicBool::new(false),
        })
    }

    /// Release SD lock on the device (`SD_CARD_OP` deinit) and flush the USB serial link.
    ///
    /// Idempotent, and [`Drop`] calls it too: call it explicitly when the error matters (a failed
    /// release leaves the card locked to the PC), and rely on the drop for every other path.
    pub fn close(&self) -> io::Result<()> {
        self.release_once()
    }

    /// Releases the USB SD session at most once.
    ///
    /// `SD_CARD_OP` deinit against a cart that holds no session answers ERR, so an explicit
    /// `close()` followed by the drop must not send it twice. A failed release clears the flag
    /// again: the card is still locked, so the drop should retry rather than treat it as done.
    fn release_once(&self) -> io::Result<()> {
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let r = (|| {
            let mut g = self
                .link
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?;
            SdCardTransport::release_usb_session(&mut *g)
        })();
        if r.is_err() {
            self.released.store(false, Ordering::SeqCst);
        }
        r
    }

    /// Flush host-side serial TX. Call after mutating SD operations so USB frames fully leave the pipe.
    pub fn flush_serial(&self) -> io::Result<()> {
        let mut g = self
            .link
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        g.flush_serial()
    }

    /// `Some(true)` if `path` is a directory, `Some(false)` if a file, `None` if absent.
    pub fn cart_path_entry_kind(&self, path: &str) -> io::Result<Option<bool>> {
        let (parent, name) = cart_path_parts(path);
        if name.is_empty() {
            return Ok(None);
        }
        let list = match self.list_dir(&parent) {
            Ok(l) => l,
            Err(e) => {
                if !parent.is_empty() && list_dir_failed_missing_parent(&e) {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        Ok(list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .map(|e| e.is_dir))
    }

    /// List a directory path (`/`, `/folder`, `folder`).
    pub fn list_dir(&self, path: &str) -> io::Result<Vec<SessionEntry>> {
        if self.exfat {
            list_dir_exfat(
                PartitionDiskUnion::Sc64(Sc64PartitionDisk::new(
                    self.link.clone(),
                    self.partition_start_sector,
                    self.partition_bytes,
                )),
                path,
            )
        } else {
            list_dir_fat(
                Sc64PartitionDisk::new(
                    self.link.clone(),
                    self.partition_start_sector,
                    self.partition_bytes,
                ),
                path,
            )
        }
    }

    /// `true` if the volume is exFAT.
    pub fn is_exfat(&self) -> bool {
        self.exfat
    }

    /// Total byte size of a cart file or directory tree (for progress).
    pub fn total_bytes_for_cart_entry(&self, cart_path: &str) -> io::Result<u64> {
        let (parent, name) = cart_path_parts(cart_path);
        let list = self.list_dir(&parent)?;
        let entry = list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cart path not found"))?;
        if entry.is_dir {
            cart_dir_total_bytes(self, &entry.path)
        } else {
            Ok(entry.size)
        }
    }

    /// Same as [`Self::copy_cart_entry_to_host`], reporting **bytes copied** (delta) to `progress`.
    /// When `skip_existing` is true, existing files at `dest` are left unchanged.
    /// Return `false` from `progress` to abort (yields [`io::ErrorKind::Interrupted`]).
    pub fn copy_cart_entry_to_host_with_progress<F>(
        &self,
        cart_path: &str,
        dest: &Path,
        skip_existing: bool,
        mut progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        let (parent, name) = cart_path_parts(cart_path);
        let list = self.list_dir(&parent)?;
        let entry = list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cart path not found"))?;
        if entry.is_dir {
            copy_cart_dir_to_host_with_progress(
                self,
                &entry.path,
                dest,
                skip_existing,
                &mut progress,
            )
        } else {
            if dest.exists() {
                if dest.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy file over existing folder on the PC",
                    ));
                }
                if skip_existing {
                    return Ok(());
                }
            }
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            let f = std::fs::File::create(dest)?;
            let mut out = BufWriter::with_capacity(STREAM_CHUNK * 2, f);
            let disk = Sc64PartitionDisk::new(
                self.link.clone(),
                self.partition_start_sector,
                self.partition_bytes,
            );
            let read_result = if self.exfat {
                read_file_exfat_streaming(
                    PartitionDiskUnion::Sc64(disk),
                    &entry.path,
                    &mut out,
                    &mut progress,
                    Some(entry.size),
                )
            } else {
                read_file_fat_streaming(
                    disk,
                    &entry.path,
                    &mut out,
                    &mut progress,
                    Some(entry.size),
                )
            };
            drop(out);
            if let Err(e) = read_result {
                if e.kind() == io::ErrorKind::Interrupted {
                    let _ = std::fs::remove_file(dest);
                }
                return Err(e);
            }
            Ok(())
        }
    }

    /// Same as [`Self::import_from_pc`], reporting **bytes written** (delta) to `progress`.
    /// When `skip_existing` is true, existing **files** on the cart at the destination path are not overwritten.
    /// Return `false` from `progress` to abort (yields [`io::ErrorKind::Interrupted`]).
    pub fn import_from_pc_with_progress<F>(
        &self,
        src: &Path,
        cart_parent: &str,
        dest_name: &str,
        skip_existing: bool,
        mut progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        let meta = std::fs::metadata(src).map_err(|e| io::Error::other(format!("{e}")))?;
        let cart_base = cart_parent_trimmed(cart_parent);
        let cart_root = if cart_base.is_empty() {
            dest_name.to_string()
        } else {
            format!("{cart_base}/{dest_name}")
        };
        if meta.is_file() {
            match self.cart_path_entry_kind(&cart_root)? {
                Some(true) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy file over existing folder on the cart",
                    ));
                }
                Some(false) if skip_existing => {
                    return Ok(());
                }
                _ => {}
            }
            let r = std::fs::File::open(src)?;
            let mut reader = BufReader::with_capacity(STREAM_CHUNK * 2, r);
            self.write_cart_file_streaming(&cart_root, meta.len(), &mut reader, &mut progress)?;
            return Ok(());
        }
        if meta.is_dir() {
            match self.cart_path_entry_kind(&cart_root)? {
                Some(false) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy folder over existing file on the cart",
                    ));
                }
                _ => {}
            }
            import_dir_from_pc_with_progress(self, src, &cart_root, skip_existing, &mut progress)?;
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported source type",
        ))
    }

    /// Delete a file or directory tree on the SD (FAT or exFAT).
    ///
    /// **FAT32:** one [`fatfs::FileSystem`] mount for the whole tree delete.
    ///
    /// **exFAT:** remounts from the SD after each committed delete (see
    /// [`remove_cart_path_exfat_unified`]) so hadris in-memory state cannot diverge from the card.
    pub fn remove_cart_path(&self, path: &str) -> io::Result<()> {
        let mut noop = |_msg: &str| {};
        self.remove_cart_path_traced(path, &mut noop)
    }

    /// Like [`remove_cart_path`], but invokes `trace` with human-readable steps (mount, delete,
    /// sync/unmount, USB flush). Intended for developer tooling; pass `&mut |_| {}` to disable.
    pub fn remove_cart_path_traced(
        &self,
        path: &str,
        trace: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        trace(&format!(
            "remove: fs={} part_start_lba={} part_bytes={}",
            if self.exfat { "exFAT" } else { "FAT" },
            self.partition_start_sector,
            self.partition_bytes
        ));
        if self.exfat {
            remove_cart_path_exfat_unified(
                ExfatVolumeSource::Sc64 {
                    link: self.link.clone(),
                },
                self.partition_start_sector,
                self.partition_bytes,
                path,
                trace,
            )
        } else {
            remove_cart_path_fat_unified(
                self.link.clone(),
                self.partition_start_sector,
                self.partition_bytes,
                path,
                trace,
            )
        }
    }

    /// Create a directory on the SD (FAT or exFAT).
    pub fn mkdir_cart(&self, path: &str) -> io::Result<()> {
        if self.exfat {
            return mkdir_cart_exfat(
                &ExfatVolumeSource::Sc64 {
                    link: self.link.clone(),
                },
                self.partition_start_sector,
                self.partition_bytes,
                path,
            );
        }
        let disk = Sc64PartitionDisk::new_writable(
            self.link.clone(),
            self.partition_start_sector,
            self.partition_bytes,
        );
        mkdir_fat_impl(disk, path)
    }

    /// Rename a file or folder on the SD (same parent directory only).
    pub fn rename_cart(&self, from: &str, to: &str) -> io::Result<()> {
        let (parent_from, name_from) = cart_path_parts(from);
        let (parent_to, name_to) = cart_path_parts(to);
        if parent_from != parent_to {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rename must stay within the same folder",
            ));
        }
        if name_from.is_empty() || name_to.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty path segment",
            ));
        }
        let from_rel = cart_rel_path_trimmed(from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty path"))?;
        let to_rel = cart_rel_path_trimmed(to)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty path"))?;
        if from_rel == to_rel {
            return Ok(());
        }
        if self.exfat {
            rename_cart_exfat(
                &ExfatVolumeSource::Sc64 {
                    link: self.link.clone(),
                },
                self.partition_start_sector,
                self.partition_bytes,
                &from_rel,
                &to_rel,
            )
        } else {
            rename_cart_fat(
                self.link.clone(),
                self.partition_start_sector,
                self.partition_bytes,
                &parent_from,
                &name_from,
                &name_to,
            )
        }
    }

    /// `len` is the source's length, which the caller has from `std::fs::metadata`. exFAT needs it
    /// up front to allocate the file's clusters in one go; FAT32 grows its chain as it writes and
    /// ignores it.
    fn write_cart_file_streaming(
        &self,
        rel_path: &str,
        len: u64,
        data: &mut impl Read,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        let r = if self.exfat {
            write_file_exfat_streaming(
                &ExfatVolumeSource::Sc64 {
                    link: self.link.clone(),
                },
                self.partition_start_sector,
                self.partition_bytes,
                rel_path,
                len,
                data,
                progress,
            )
        } else {
            self.write_file_fat_streaming(rel_path, data, progress)
        };
        // No cleanup here on cancel. Both writes remove their own partial work, and on a replace
        // the file at `rel_path` is still the untouched original: removing it, as this once did,
        // would delete exactly what #200's ordering keeps.
        r
    }

    fn write_file_fat_streaming(
        &self,
        rel_path: &str,
        data: &mut impl Read,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        let disk = Sc64PartitionDisk::new_writable(
            self.link.clone(),
            self.partition_start_sector,
            self.partition_bytes,
        );
        write_file_fat_streaming_impl(disk, rel_path, data, progress)
    }
}

impl Drop for Sc64SdSession {
    /// Releases the cart's PC-side SD lock on every path that does not reach
    /// [`close`](Self::close) — an early return, a `?`, a panic. Leaving it held is not a leak the
    /// process can clean up later: the console refuses to boot from the card until some other
    /// process opens and closes a session. Errors are unreportable here, so they are dropped;
    /// callers that need to know release succeeded call `close()` and check it.
    fn drop(&mut self) {
        let _ = self.release_once();
    }
}

#[cfg(feature = "ed64")]
impl Ed64SdSession {
    /// Open COM, EverDrive test handshake, detect MBR/GPT + FAT/exFAT using `RomRead` at `rom_linear_base + LBA·512`.
    pub fn open(port_name: &str, baud: u32, rom_linear_base: u32) -> io::Result<Self> {
        let mut link = Ed64RomLinear::open(port_name, baud, rom_linear_base)?;
        let mut s0 = [0u8; 512];
        link.read_sd_sectors(0, &mut s0)?;
        let part_start = detect_partition_start(&s0, |lba, buf| link.read_sd_sectors(lba, buf))?;
        let mut bpb = [0u8; 512];
        link.read_sd_sectors(part_start, &mut bpb)?;
        let exfat = is_exfat_boot_sector(&bpb);
        let part_bytes = partition_volume_bytes(&bpb)?;
        Ok(Self {
            link: Arc::new(Mutex::new(link)),
            partition_start_sector: part_start,
            partition_bytes: part_bytes,
            exfat,
            released: AtomicBool::new(false),
        })
    }

    /// Release the USB session (serial flush). Idempotent; also runs on [`Drop`].
    pub fn close(&self) -> io::Result<()> {
        self.release_once()
    }

    /// See [`Sc64SdSession::release_once`].
    fn release_once(&self) -> io::Result<()> {
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let r = (|| {
            let mut g = self
                .link
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?;
            SdCardTransport::release_usb_session(&mut *g)
        })();
        if r.is_err() {
            self.released.store(false, Ordering::SeqCst);
        }
        r
    }

    pub fn flush_serial(&self) -> io::Result<()> {
        let mut g = self
            .link
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        g.flush_serial()
    }

    pub fn cart_path_entry_kind(&self, path: &str) -> io::Result<Option<bool>> {
        let (parent, name) = cart_path_parts(path);
        if name.is_empty() {
            return Ok(None);
        }
        let list = match self.list_dir(&parent) {
            Ok(l) => l,
            Err(e) => {
                if !parent.is_empty() && list_dir_failed_missing_parent(&e) {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        Ok(list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .map(|e| e.is_dir))
    }

    pub fn list_dir(&self, path: &str) -> io::Result<Vec<SessionEntry>> {
        if self.exfat {
            list_dir_exfat(
                PartitionDiskUnion::Ed64(SectorPartitionDisk::new(
                    self.link.clone(),
                    self.partition_start_sector,
                    self.partition_bytes,
                )),
                path,
            )
        } else {
            list_dir_fat(
                SectorPartitionDisk::new(
                    self.link.clone(),
                    self.partition_start_sector,
                    self.partition_bytes,
                ),
                path,
            )
        }
    }

    pub fn is_exfat(&self) -> bool {
        self.exfat
    }

    pub fn total_bytes_for_cart_entry(&self, cart_path: &str) -> io::Result<u64> {
        let (parent, name) = cart_path_parts(cart_path);
        let list = self.list_dir(&parent)?;
        let entry = list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cart path not found"))?;
        if entry.is_dir {
            cart_dir_total_bytes_ed64(self, &entry.path)
        } else {
            Ok(entry.size)
        }
    }

    pub fn copy_cart_entry_to_host_with_progress<F>(
        &self,
        cart_path: &str,
        dest: &Path,
        skip_existing: bool,
        mut progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        let (parent, name) = cart_path_parts(cart_path);
        let list = self.list_dir(&parent)?;
        let entry = list
            .into_iter()
            .find(|e| cart_entry_name_matches(&e.name, &name))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cart path not found"))?;
        if entry.is_dir {
            copy_cart_dir_to_host_with_progress_ed64(
                self,
                &entry.path,
                dest,
                skip_existing,
                &mut progress,
            )
        } else {
            if dest.exists() {
                if dest.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy file over existing folder on the PC",
                    ));
                }
                if skip_existing {
                    return Ok(());
                }
            }
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            let f = std::fs::File::create(dest)?;
            let mut out = BufWriter::with_capacity(STREAM_CHUNK * 2, f);
            let disk = SectorPartitionDisk::new(
                self.link.clone(),
                self.partition_start_sector,
                self.partition_bytes,
            );
            let read_result = if self.exfat {
                read_file_exfat_streaming(
                    PartitionDiskUnion::Ed64(disk),
                    &entry.path,
                    &mut out,
                    &mut progress,
                    Some(entry.size),
                )
            } else {
                read_file_fat_streaming(
                    disk,
                    &entry.path,
                    &mut out,
                    &mut progress,
                    Some(entry.size),
                )
            };
            drop(out);
            if let Err(e) = read_result {
                if e.kind() == io::ErrorKind::Interrupted {
                    let _ = std::fs::remove_file(dest);
                }
                return Err(e);
            }
            Ok(())
        }
    }

    pub fn import_from_pc_with_progress<F>(
        &self,
        _src: &Path,
        _cart_parent: &str,
        _dest_name: &str,
        _skip_existing: bool,
        _progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The experimental EverDrive SD mode is read-only.",
        ))
    }

    pub fn remove_cart_path(&self, _path: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The experimental EverDrive SD mode is read-only.",
        ))
    }

    pub fn remove_cart_path_traced(
        &self,
        _path: &str,
        _trace: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The experimental EverDrive SD mode is read-only.",
        ))
    }

    pub fn mkdir_cart(&self, _path: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The experimental EverDrive SD mode is read-only.",
        ))
    }

    pub fn rename_cart(&self, _from: &str, _to: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The experimental EverDrive SD mode is read-only.",
        ))
    }
}

/// See [`Sc64SdSession`]'s `Drop`.
#[cfg(feature = "ed64")]
impl Drop for Ed64SdSession {
    fn drop(&mut self) {
        let _ = self.release_once();
    }
}

#[cfg(feature = "ed64")]
fn cart_dir_total_bytes_ed64(session: &Ed64SdSession, cart_path: &str) -> io::Result<u64> {
    let mut sum = 0u64;
    for e in session.list_dir(cart_path)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        if e.is_dir {
            sum += cart_dir_total_bytes_ed64(session, &e.path)?;
        } else {
            sum += e.size;
        }
    }
    Ok(sum)
}

#[cfg(feature = "ed64")]
fn copy_cart_dir_to_host_with_progress_ed64(
    session: &Ed64SdSession,
    cart_path: &str,
    dest_dir: &Path,
    skip_existing: bool,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for e in session.list_dir(cart_path)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        let sub = dest_dir.join(&e.name);
        if e.is_dir {
            if sub.exists() && sub.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cannot copy cart folder over existing file on the PC",
                ));
            }
            copy_cart_dir_to_host_with_progress_ed64(
                session,
                &e.path,
                &sub,
                skip_existing,
                progress,
            )?;
        } else {
            if skip_existing && sub.exists() {
                continue;
            }
            let f = std::fs::File::create(&sub)?;
            let mut out = BufWriter::with_capacity(STREAM_CHUNK * 2, f);
            let disk = SectorPartitionDisk::new(
                session.link.clone(),
                session.partition_start_sector,
                session.partition_bytes,
            );
            let read_result = if session.exfat {
                read_file_exfat_streaming(
                    PartitionDiskUnion::Ed64(disk),
                    &e.path,
                    &mut out,
                    progress,
                    Some(e.size),
                )
            } else {
                read_file_fat_streaming(disk, &e.path, &mut out, progress, Some(e.size))
            };
            drop(out);
            if let Err(err) = read_result {
                if err.kind() == io::ErrorKind::Interrupted {
                    let _ = std::fs::remove_file(&sub);
                }
                return Err(err);
            }
        }
    }
    Ok(())
}

/// FAT32 (or FAT12/16) streaming write — shared by [`Sc64SdSession`] and RAM-disk tests.
fn write_file_fat_streaming_impl<D: Read + Write + Seek>(
    disk: D,
    rel_path: &str,
    data: &mut impl Read,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let parts: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let (name, parents) = parts.split_last().unwrap();
    {
        let mut dir = fs.root_dir();
        for p in parents {
            dir = match dir.open_dir(p) {
                Ok(d) => d,
                Err(_) => dir.create_dir(p)?,
            };
        }
        // A replacement is written under a temporary name in the same folder, so the original
        // stays whole until the copy is complete (#200). Only then is the original removed and
        // the copy renamed over it. fatfs has no atomic replace, so that last step still has a
        // window, but it is two directory-entry updates rather than the whole transfer.
        let replacing = dir.open_file(name).is_ok();
        let target = if replacing {
            let temp = fat_replace_temp_name(name);
            // Left by a replace that was interrupted between its last two steps before.
            let _ = dir.remove(&temp);
            temp
        } else {
            name.to_string()
        };
        let written = (|| -> io::Result<()> {
            let mut f = dir.create_file(&target)?;
            // `create_file` opens an existing file without truncating it.
            f.truncate()?;
            let mut buf = vec![0u8; STREAM_CHUNK];
            loop {
                let n = data.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n])?;
                if !progress(n as u64) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
            }
            // fatfs writes the file's size into its directory entry on flush. `Drop` only logs a
            // failure, which would pass a short file off as a finished copy (#129).
            f.flush()
        })();
        if let Err(e) = written {
            // Whatever part of the new file exists is ours; the original, if any, was not touched.
            let _ = dir.remove(&target);
            return Err(e);
        }
        if replacing {
            dir.remove(name)?;
            dir.rename(&target, &dir, name).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "the new copy is complete on the card as \"{target}\" but could not be \
                         renamed to \"{name}\": {e}"
                    ),
                )
            })?;
        }
    }
    fs.unmount()
}

/// The temporary name a FAT32 replacement of `name` is written under before it is renamed into
/// place: a prefix no ordinary file carries, and short enough for a 255-unit long file name.
fn fat_replace_temp_name(name: &str) -> String {
    const PREFIX: &str = "~multi64-replace-";
    let room = 255 - PREFIX.len();
    let mut units = 0;
    let kept: String = name
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= room
        })
        .collect();
    format!("{PREFIX}{kept}")
}

/// Write `len` bytes of `data` to `rel_path`, creating any missing parent directories.
///
/// Every step resolves through this crate's own reader and places its entry sets through
/// [`ExfatDirSlots`], so an import works in a root that spans several clusters (#190). This used to
/// navigate with `fs.root_dir()` and create through hadris, whose free-slot scan runs past a
/// directory's end and cannot see a chained root's later clusters — which had to be refused
/// outright rather than risk writing an entry set into another file's data (#175).
///
/// **Replacing** an existing file writes the replacement first and only then swaps it in, so a
/// cancelled or failed replace leaves the original as it was (#200); see [`exfat_create_file_in`].
/// A partial write of a *new* file is rolled back here too, so callers have nothing to clean up.
fn write_file_exfat_streaming(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    rel_path: &str,
    len: u64,
    data: &mut impl Read,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    let fs = ExFatFs::open(disk).map_err(exfat_err)?;
    let info = fs.info().clone();
    let parts: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let (name, parents) = parts.split_last().unwrap();

    // Any folder made here is already on the card, bitmap included, whatever happens to the file.
    let path_prefix = exfat_create_missing_dirs(&fs, &info, vol, part_start, part_bytes, parents)?;

    let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
    let parent = exfat_resolve_dir_slots(&mut read_at, &info, &path_prefix)?;
    let existing = exfat_list_entries(&parent, &mut read_at)?
        .into_iter()
        .find(|e| fat_names_equal(&e.name, name));
    if existing.as_ref().is_some_and(|e| e.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a directory exists with this name",
        ));
    }
    // An existing file is not deleted here: the create writes the replacement first and takes the
    // original's slots only once it is complete, so a failure leaves the original intact (#200).
    exfat_create_file_in(
        &fs,
        &info,
        vol,
        part_start,
        part_bytes,
        &parent,
        rel_path,
        name,
        existing.as_ref(),
        len,
        data,
        progress,
    )?;
    drop(fs);
    vol.flush_serial()?;
    Ok(())
}

fn cart_dir_total_bytes(session: &Sc64SdSession, cart_path: &str) -> io::Result<u64> {
    let mut sum = 0u64;
    for e in session.list_dir(cart_path)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        if e.is_dir {
            sum += cart_dir_total_bytes(session, &e.path)?;
        } else {
            sum += e.size;
        }
    }
    Ok(sum)
}

fn cart_parent_trimmed(s: &str) -> String {
    s.trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

/// Match a list_dir [`SessionEntry::name`] against the last segment of a cart path, ignoring case
/// as FAT and exFAT do (see [`fat_names_equal`]).
fn cart_entry_name_matches(list_name: &str, path_last_component: &str) -> bool {
    fat_names_equal(list_name, path_last_component)
}

/// Case-insensitive name equality as FAT and exFAT define it: UTF-16 code units compared through a
/// one-to-one up-case mapping. That is exFAT's up-case table, and how Windows and FatFs fold long
/// names. Unicode simple uppercase stands in for a volume's own table, so `ä` matches `Ä` but `ß`
/// never matches `SS`. ASCII-only folding let `Ä.z64` pass for a different file than `ä.z64` (#131).
pub(crate) fn fat_names_equal(a: &str, b: &str) -> bool {
    a == b
        || a.encode_utf16()
            .map(fat_upcase_unit)
            .eq(b.encode_utf16().map(fat_upcase_unit))
}

fn fat_upcase_unit(unit: u16) -> u16 {
    // Surrogate halves are not characters, and the up-case table leaves them unchanged.
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => u16::try_from(u32::from(single)).unwrap_or(unit),
        _ => unit,
    }
}

/// True when [`Sc64SdSession::list_dir`] failed because a non-root parent path is missing on the volume.
/// Used by [`Sc64SdSession::cart_path_entry_kind`]: if `a/b/c` is queried and `a/b` does not exist yet
/// (e.g. PC→cart import plan before any mkdir), the entry cannot exist.
fn list_dir_failed_missing_parent(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::NotFound {
        return true;
    }
    let s = e.to_string().to_lowercase();
    s.contains("not found")
        || s.contains("no such file")
        || s.contains("path not found")
        || s.contains("does not exist")
}

pub fn cart_path_parts(path: &str) -> (String, String) {
    let p = path
        .trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if p.is_empty() {
        return (String::new(), String::new());
    }
    match p.rfind('/') {
        None => (String::new(), p),
        Some(0) => (String::new(), p[1..].to_string()),
        Some(i) => (p[..i].to_string(), p[i + 1..].to_string()),
    }
}

fn copy_cart_dir_to_host_with_progress(
    session: &Sc64SdSession,
    cart_path: &str,
    dest_dir: &Path,
    skip_existing: bool,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for e in session.list_dir(cart_path)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        let sub = dest_dir.join(&e.name);
        if e.is_dir {
            if sub.exists() && sub.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cannot copy cart folder over existing file on the PC",
                ));
            }
            copy_cart_dir_to_host_with_progress(session, &e.path, &sub, skip_existing, progress)?;
        } else {
            if skip_existing && sub.exists() {
                continue;
            }
            let f = std::fs::File::create(&sub)?;
            let mut out = BufWriter::with_capacity(STREAM_CHUNK * 2, f);
            let disk = Sc64PartitionDisk::new(
                session.link.clone(),
                session.partition_start_sector,
                session.partition_bytes,
            );
            let read_result = if session.exfat {
                read_file_exfat_streaming(
                    PartitionDiskUnion::Sc64(disk),
                    &e.path,
                    &mut out,
                    progress,
                    Some(e.size),
                )
            } else {
                read_file_fat_streaming(disk, &e.path, &mut out, progress, Some(e.size))
            };
            drop(out);
            if let Err(err) = read_result {
                if err.kind() == io::ErrorKind::Interrupted {
                    let _ = std::fs::remove_file(&sub);
                }
                return Err(err);
            }
        }
    }
    Ok(())
}

fn import_dir_from_pc_with_progress(
    session: &Sc64SdSession,
    src_dir: &Path,
    cart_base: &str,
    skip_existing: bool,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry.map_err(|e| io::Error::other(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let cart_sub = if cart_base.is_empty() {
            name.clone()
        } else {
            format!("{cart_base}/{name}")
        };
        let meta = entry
            .metadata()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if meta.is_dir() {
            match session.cart_path_entry_kind(&cart_sub)? {
                Some(false) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy folder over existing file on the cart",
                    ));
                }
                _ => {}
            }
            session.mkdir_cart(&cart_sub)?;
            import_dir_from_pc_with_progress(session, &path, &cart_sub, skip_existing, progress)?;
        } else {
            match session.cart_path_entry_kind(&cart_sub)? {
                Some(true) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy file over existing folder on the cart",
                    ));
                }
                Some(false) if skip_existing => {
                    continue;
                }
                _ => {}
            }
            let r = std::fs::File::open(&path)?;
            let mut reader = BufReader::with_capacity(STREAM_CHUNK * 2, r);
            session.write_cart_file_streaming(&cart_sub, meta.len(), &mut reader, progress)?;
        }
    }
    Ok(())
}

fn exfat_err<E: core::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

fn flush_link_serial(link: &Arc<Mutex<Sc64Link>>) -> io::Result<()> {
    let mut g = link.lock().map_err(|e| io::Error::other(e.to_string()))?;
    g.flush_serial()
}

/// Locates `entry`'s entry set in its parent directory by what is on disk, not by where hadris
/// says it is: hadris keeps [`hadris_fat::exfat::ExFatFileEntry`]'s `entry_offset` private, and
/// sets it directory-relative in some paths and volume-absolute in others.
///
/// Scans the parent's own slots, in stream order, for the first in-use entry set whose File,
/// Stream Extension and File Name entries carry `entry`'s exact name, directory flag, first
/// cluster and both lengths (see [`exfat_find_entry_set`]). Nothing outside the parent's slots is
/// ever considered.
///
/// Returns the parent directory's slot map and the index of the entry set's primary slot. Address
/// the set's other slots through that map: in a FAT-chained directory the next slot is not always
/// 32 bytes further on (#128).
///
/// Call with an entry hadris has just read, and while the primary is still `0x85` on disk (before
/// marking deleted).
fn exfat_locate_entry_set(
    fs: &ExFatFs<PartitionDiskUnion>,
    entry_path: &str,
    want: &ExfatEntryIdentity<'_>,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<(ExfatDirSlots, u64)> {
    let (parent_path, _) = exfat_parent_path_and_name(entry_path);
    let dir = exfat_parent_dir_slots(fs, parent_path, vol, part_start, part_bytes)?;
    let read_at = exfat_volume_reader(vol, part_start, part_bytes);
    match exfat_find_entry_set(&dir, want, read_at)? {
        Some(slot) => Ok((dir, slot)),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "could not find the exFAT directory entry set in its folder",
        )),
    }
}

/// The on-disk fields that identify a file's entry set in its directory.
struct ExfatEntryIdentity<'a> {
    name: &'a str,
    is_directory: bool,
    first_cluster: u32,
    valid_data_length: u64,
    data_length: u64,
}

impl<'a> ExfatEntryIdentity<'a> {
    /// The same identity from an entry this crate decoded itself, for paths that resolve without
    /// hadris — which is the only way to reach an entry past a chained root's first cluster (#190).
    fn of_decoded(entry: &'a ExfatDecodedEntry) -> Self {
        Self {
            name: &entry.name,
            is_directory: entry.is_dir(),
            first_cluster: entry.first_cluster,
            valid_data_length: entry.valid_data_length,
            data_length: entry.data_length,
        }
    }
}

/// Index of the primary slot of the first in-use entry set in `dir` that matches `want`, decoded
/// by the exFAT layout: attributes at File entry byte 4, and NameLength (3), ValidDataLength (8),
/// FirstCluster (20) and DataLength (24) in the Stream Extension, which the File Name entries
/// follow. The name compares exactly, UTF-16 unit by unit: hadris reports the name as stored.
///
/// The scan stops at the end-of-directory marker (`0x00`), and a set whose secondaries would run
/// past `dir`'s last slot is not a match. Of two sets that match, the first in stream order is the
/// one hadris's own lookup reaches first.
fn exfat_find_entry_set(
    dir: &ExfatDirSlots,
    want: &ExfatEntryIdentity<'_>,
    read_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Option<u64>> {
    const END_OF_DIRECTORY: u8 = 0x00;
    const ATTR_DIRECTORY: u16 = 0x10;
    let u32_at = |s: &[u8; 32], at: usize| u32::from_le_bytes(s[at..at + 4].try_into().unwrap());
    let u64_at = |s: &[u8; 32], at: usize| u64::from_le_bytes(s[at..at + 8].try_into().unwrap());

    let name: Vec<u16> = want.name.encode_utf16().collect();
    let mut slots = ExfatSlotReader::new(dir, read_at);
    let count = dir.slot_count();
    let mut i = 0;
    while i < count {
        let primary = slots.slot(i)?;
        if primary[0] == END_OF_DIRECTORY {
            break;
        }
        let secondaries = u64::from(primary[1]);
        if primary[0] != EXFAT_ENTRY_FILE_DIRECTORY
            || !(2..=18).contains(&secondaries)
            || i + secondaries >= count
        {
            i += 1;
            continue;
        }
        let stream = slots.slot(i + 1)?;
        if stream[0] != EXFAT_ENTRY_STREAM_EXT {
            i += 1;
            continue;
        }
        let attributes = u16::from_le_bytes([primary[4], primary[5]]);
        let mut matched = (attributes & ATTR_DIRECTORY != 0) == want.is_directory
            && usize::from(stream[3]) == name.len()
            && u64_at(&stream, 8) == want.valid_data_length
            && u32_at(&stream, 20) == want.first_cluster
            && u64_at(&stream, 24) == want.data_length;
        let mut units = 0;
        let mut s = i + 2;
        while matched && units < name.len() {
            if s > i + secondaries {
                matched = false;
                break;
            }
            let entry = slots.slot(s)?;
            if entry[0] != EXFAT_ENTRY_FILE_NAME {
                matched = false;
                break;
            }
            for unit in entry[2..].chunks_exact(2).take(name.len() - units) {
                if u16::from_le_bytes([unit[0], unit[1]]) != name[units] {
                    matched = false;
                    break;
                }
                units += 1;
            }
            s += 1;
        }
        if matched {
            return Ok(Some(i));
        }
        i += 1 + secondaries;
    }
    Ok(None)
}

/// Reads `dir`'s slots a chunk at a time and keeps every chunk it has read, so a scan costs one
/// disk read (one SC64 USB round trip) per chunk rather than one per slot. A chunk is at most
/// [`Self::MAX_CHUNK_SLOTS`] slots and never crosses a cluster boundary, so it is contiguous on
/// disk even in a FAT-chained directory.
struct ExfatSlotReader<'d, R> {
    dir: &'d ExfatDirSlots,
    read_at: R,
    chunk_slots: u64,
    chunks: std::collections::HashMap<u64, Vec<u8>>,
}

impl<'d, R: FnMut(u64, &mut [u8]) -> io::Result<()>> ExfatSlotReader<'d, R> {
    /// 8 KiB: 16 sectors.
    const MAX_CHUNK_SLOTS: u64 = 256;

    fn new(dir: &'d ExfatDirSlots, read_at: R) -> Self {
        let chunk_slots = if dir.slots_per_cluster % Self::MAX_CHUNK_SLOTS == 0 {
            Self::MAX_CHUNK_SLOTS
        } else {
            dir.slots_per_cluster
        };
        Self {
            dir,
            read_at,
            chunk_slots,
            chunks: std::collections::HashMap::new(),
        }
    }

    fn slot(&mut self, slot: u64) -> io::Result<[u8; 32]> {
        let chunk = slot / self.chunk_slots;
        if !self.chunks.contains_key(&chunk) {
            // Errors for a slot past the directory's last cluster.
            let at = self.dir.offset(chunk * self.chunk_slots)?;
            let mut buf = vec![0u8; (self.chunk_slots * ExfatDirSlots::SLOT_BYTES) as usize];
            (self.read_at)(at, &mut buf)?;
            self.chunks.insert(chunk, buf);
        }
        let off = ((slot % self.chunk_slots) * ExfatDirSlots::SLOT_BYTES) as usize;
        Ok(self.chunks[&chunk][off..off + 32].try_into().unwrap())
    }
}

/// Reads `buf.len()` bytes at a volume byte offset, refusing reads past the partition.
fn exfat_volume_reader(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> impl FnMut(u64, &mut [u8]) -> io::Result<()> {
    let mut disk = vol.partition_disk_ro(part_start, part_bytes);
    move |at, buf| {
        if at.saturating_add(buf.len() as u64) > part_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT directory read past partition",
            ));
        }
        disk.seek(SeekFrom::Start(at))?;
        disk.read_exact(buf)
    }
}

/// The same contract as [`exfat_volume_reader`], on a disk the caller already owns.
///
/// Listing owns its disk — `ExFatFs::open` would consume it — while everything reached through an
/// [`ExfatVolumeSource`] does not. Giving both the same reader shape lets one chain walk, one
/// lister and one resolver serve them, instead of a second set that drifts from the first.
fn exfat_disk_reader<D: PartitionDisk>(
    disk: &mut D,
) -> impl FnMut(u64, &mut [u8]) -> io::Result<()> + '_ {
    let part_bytes = disk.partition_byte_len();
    move |at, buf| {
        if at.saturating_add(buf.len() as u64) > part_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT directory read past partition",
            ));
        }
        disk.seek(SeekFrom::Start(at))?;
        disk.read_exact(buf)
    }
}

/// One FAT entry, for a chain walk that does not own its disk. Mirrors
/// [`exfat_read_fat_entry_inner`], which reads through a [`PartitionDisk`] instead.
fn exfat_read_fat_entry_with<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    read_at: &mut R,
    info: &hadris_fat::exfat::ExFatInfo,
    cluster: u32,
) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    read_at(info.fat_offset + u64::from(cluster) * 4, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// The entry at `path`, found through this crate's own chain-following reader rather than hadris's
/// lookup, which cannot see past a chained root's first cluster (#190).
fn exfat_resolve_entry<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    read_at: &mut R,
    info: &hadris_fat::exfat::ExFatInfo,
    path: &str,
) -> io::Result<ExfatDecodedEntry> {
    let normalized = path.trim().replace('\\', "/");
    let (parent, name) = exfat_parent_path_and_name(&normalized);
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty cart path",
        ));
    }
    let dir = exfat_resolve_dir_slots(read_at, info, parent)?;
    exfat_list_entries(&dir, read_at)?
        .into_iter()
        .find(|e| fat_names_equal(&e.name, name))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no such entry: {name}")))
}

/// As [`exfat_resolve_entry`], for callers that hold a volume source rather than a disk.
fn exfat_resolve_entry_vol(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    path: &str,
) -> io::Result<ExfatDecodedEntry> {
    let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
    exfat_resolve_entry(&mut read_at, info, path)
}

fn exfat_parent_path_and_name(path: &str) -> (&str, &str) {
    let path = path.trim().trim_start_matches('/');
    match path.rsplit_once('/') {
        None => ("", path),
        Some((p, n)) => (p, n),
    }
}

/// Slot map of the directory at `parent_path` (`""` is the root).
///
/// The root directory has no stream entry of its own and always follows the FAT. Treating it as
/// contiguous with size 0, as hadris does, let a scan run on into whatever clusters follow it (#128).
/// hadris's own creates had the same flaw and were guarded against rather than used; since #190
/// nothing in this crate reaches them (#175).
///
/// Every segment resolves through this crate's own reader. Going through `fs.open_path` for a
/// nested parent used hadris's lookup, which cannot see an entry past the root's first cluster, so
/// the chain-correct slot map above was reached only for top-level paths (#190). `fs` is now used
/// for nothing but [`ExFatFs::info`].
fn exfat_parent_dir_slots(
    fs: &ExFatFs<PartitionDiskUnion>,
    parent_path: &str,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<ExfatDirSlots> {
    let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
    exfat_resolve_dir_slots(&mut read_at, fs.info(), parent_path)
}

/// One exFAT directory's clusters in stream order, mapping a slot (32-byte entry) index to its
/// volume byte offset. A directory stored as a FAT chain is not contiguous on disk, so slot `n + 1`
/// is not always 32 bytes after slot `n` (#128).
struct ExfatDirSlots {
    /// Volume byte offset of each of the directory's clusters, in stream order.
    cluster_offsets: Vec<u64>,
    slots_per_cluster: u64,
}

impl ExfatDirSlots {
    const SLOT_BYTES: u64 = 32;

    /// A directory's clusters in stream order, over any reader.
    ///
    /// A contiguous (NoFatChain) directory spans `size` bytes. A chained one follows the FAT to its
    /// end, capped at `size` when that is non-zero. The root is chained with `size` 0.
    ///
    /// The caller supplies the reader: [`exfat_volume_reader`] from an [`ExfatVolumeSource`], or
    /// [`exfat_disk_reader`] from a disk it already owns — listing owns its disk, because
    /// `ExFatFs::open` would consume it, and `ExfatVolumeSource` has no EverDrive variant. One walk
    /// serves both, so a directory's clusters are found the same way whichever the caller holds.
    fn load_with<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
        read_at: &mut R,
        info: &hadris_fat::exfat::ExFatInfo,
        first_cluster: u32,
        is_contiguous: bool,
        size: u64,
    ) -> io::Result<Self> {
        let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
        let cluster_bytes = info.bytes_per_cluster as u64;
        if cluster_bytes < Self::SLOT_BYTES || !info.is_valid_cluster(first_cluster) {
            return Err(invalid("exFAT directory has no valid first cluster"));
        }
        let size_clusters = (size + cluster_bytes - 1) / cluster_bytes;
        let mut clusters = vec![first_cluster];
        if is_contiguous {
            for i in 1..size_clusters.max(1) {
                let c = u32::try_from(i)
                    .ok()
                    .and_then(|i| first_cluster.checked_add(i))
                    .filter(|&c| info.is_valid_cluster(c))
                    .ok_or_else(|| invalid("exFAT directory runs past the cluster heap"))?;
                clusters.push(c);
            }
        } else {
            let mut c = first_cluster;
            while size == 0 || (clusters.len() as u64) < size_clusters {
                let next = exfat_read_fat_entry_with(read_at, info, c)?;
                // End of chain, or anything else that is not a data cluster: nothing after it is
                // provably part of this directory.
                if !info.is_valid_cluster(next) {
                    break;
                }
                if clusters.len() as u64 >= u64::from(info.cluster_count) {
                    return Err(invalid("exFAT directory cluster chain loops"));
                }
                clusters.push(next);
                c = next;
            }
        }
        Ok(Self {
            cluster_offsets: clusters
                .iter()
                .map(|&c| info.cluster_to_offset(c))
                .collect(),
            slots_per_cluster: cluster_bytes / Self::SLOT_BYTES,
        })
    }

    fn slot_count(&self) -> u64 {
        self.cluster_offsets.len() as u64 * self.slots_per_cluster
    }

    /// Volume byte offset of slot `slot`.
    fn offset(&self, slot: u64) -> io::Result<u64> {
        let base = usize::try_from(slot / self.slots_per_cluster)
            .ok()
            .and_then(|i| self.cluster_offsets.get(i))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "exFAT entry set runs past the end of its directory",
                )
            })?;
        Ok(base + slot % self.slots_per_cluster * Self::SLOT_BYTES)
    }

    /// Volume byte offsets of `count` consecutive slots from `first`, following the chain.
    fn offsets(&self, first: u64, count: usize) -> io::Result<Vec<u64>> {
        (first..first + count as u64)
            .map(|s| self.offset(s))
            .collect()
    }

    /// The slot starting at volume byte offset `abs`, if it is one of this directory's slots — the
    /// inverse of [`Self::offset`].
    ///
    /// Test-only since #190 removed the guard that was its one production caller. It is what lets a
    /// test back a directory with a flat slot array while still exercising the real chain
    /// arithmetic, rather than assuming slots are 32 bytes apart.
    #[cfg(test)]
    fn slot_at(&self, abs: u64) -> Option<u64> {
        let cluster_bytes = self.slots_per_cluster * Self::SLOT_BYTES;
        let i = self.cluster_offsets.iter().position(|&base| {
            abs >= base && abs - base < cluster_bytes && (abs - base) % Self::SLOT_BYTES == 0
        })?;
        Some(i as u64 * self.slots_per_cluster + (abs - self.cluster_offsets[i]) / Self::SLOT_BYTES)
    }
}

/// Entries in an exFAT entry set for `name`: File, Stream Extension, then one File Name entry per
/// 15 UTF-16 code units.
fn exfat_entry_set_len(name: &str) -> usize {
    2 + (name.encode_utf16().count() + 14) / 15
}

fn exfat_read_fat_entry_inner(
    disk: &mut impl PartitionDisk,
    info: &hadris_fat::exfat::ExFatInfo,
    cluster: u32,
) -> io::Result<u32> {
    let off = info.fat_offset + u64::from(cluster) * 4;
    if off + 4 > disk.partition_byte_len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FAT entry read past partition",
        ));
    }
    disk.seek(SeekFrom::Start(off))?;
    let mut buf = [0u8; 4];
    disk.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// exFAT: when the **NoFatChain** bit is set on a stream, FAT entries for that allocation must be
/// **unused (zero)**; allocation is tracked only in the bitmap. hadris `allocate_cluster` instead
/// writes **end-of-chain** in the FAT, which Windows chkdsk treats as filesystem inconsistency.
///
/// Returns `true` if every FAT value in `first_cluster..+count` is **not** a pointer to another
/// valid data cluster (i.e. not a fragmented-chain link). EOC / free / garbage above cluster range
/// are treated as safe to overwrite with zero.
fn exfat_should_zero_fat_for_nofatchain_range(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    first_cluster: u32,
    cluster_count: u32,
) -> io::Result<bool> {
    let mut disk = vol.partition_disk_ro(part_start, part_bytes);
    for i in 0..cluster_count {
        let c = first_cluster.checked_add(i).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "exFAT FAT cluster overflow")
        })?;
        let fat = exfat_read_fat_entry_inner(&mut disk, info, c)?;
        if info.is_valid_cluster(fat) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// hadris `ExFatFs::free_clusters` with contiguous allocation clears the allocation bitmap only;
/// it does not write FAT (see `hadris-fat` `exfat/fs.rs`). Contiguous allocation in hadris also
/// bitmap-only, so FAT entries can still show allocated/EOC while the bitmap says free — chkdsk
/// reports volume bitmap corruption. Match [`hadris_fat::exfat::fat::ExFatTable::write_entry`]:
/// write **free** (`0`) to every FAT copy for each cluster in the run.
fn exfat_clear_fat_contiguous_range(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    first_cluster: u32,
    cluster_count: u32,
) -> io::Result<()> {
    const FREE: u32 = 0;
    let entries = (0..cluster_count)
        .map(|i| {
            first_cluster
                .checked_add(i)
                .map(|c| (c, FREE))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "exFAT FAT cluster overflow")
                })
        })
        .collect::<io::Result<Vec<_>>>()?;
    exfat_write_fat_entries(vol, part_start, part_bytes, info, &entries)
}

/// Write each `(cluster, value)` into every copy of the FAT, through one disk handle.
///
/// The one place that knows where a FAT entry lives, shared by clearing a NoFatChain run and by
/// linking a chain, so the two cannot disagree about it.
fn exfat_write_fat_entries(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    entries: &[(u32, u32)],
) -> io::Result<()> {
    const ENTRY_BYTES: u64 = 4;
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    for &(cluster, value) in entries {
        if !info.is_valid_cluster(cluster) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT FAT cluster out of range",
            ));
        }
        for fat_idx in 0..info.fat_count {
            let off = info.fat_offset
                + u64::from(fat_idx) * info.fat_length
                + u64::from(cluster) * ENTRY_BYTES;
            if off + ENTRY_BYTES > part_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "exFAT FAT write past partition",
                ));
            }
            disk.seek(SeekFrom::Start(off))?;
            disk.write_all(&value.to_le_bytes())?;
        }
    }
    disk.flush()?;
    Ok(())
}

/// Same algorithm as hadris `compute_entry_set_checksum` (see `hadris-fat` `exfat/entry.rs`).
fn exfat_compute_entry_set_checksum(entries: &[[u8; 32]]) -> u16 {
    let mut checksum: u16 = 0;
    for (entry_idx, entry) in entries.iter().enumerate() {
        for (byte_idx, &byte) in entry.iter().enumerate() {
            if entry_idx == 0 && (byte_idx == 2 || byte_idx == 3) {
                continue;
            }
            checksum = checksum.rotate_right(1).wrapping_add(byte as u16);
        }
    }
    checksum
}

const EXFAT_STREAM_EXT: u8 = 0xC0;
/// Stream extension with the **In-Use** bit (bit 7) cleared — required when the file is deleted.
const EXFAT_STREAM_EXT_DELETED: u8 = EXFAT_STREAM_EXT & 0x7f;

/// Mark the file directory entry deleted (`0x05`), mark **secondaries** deleted (clear bit 7:
/// `0xC0`→`0x40`, `0xC1`→`0x41` per Microsoft exFAT), invalidate the stream (zero lengths /
/// `first_cluster`), recompute the entry-set checksum, and write **all** slots back.
///
/// Leaving secondaries at `0xC0`/`0xC1` while the primary is `0x05` breaks Windows’ directory
/// validation (chkdsk “examining files in directory …”).
fn exfat_finalize_deleted_entry_set(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    dir: &ExfatDirSlots,
    primary_slot: u64,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    const DELETED_FILE: u8 = 0x05;

    let read_at = exfat_volume_reader(vol, part_start, part_bytes);
    let Some(mut entries) = exfat_read_entry_set(dir, primary_slot, read_at)? else {
        trace("exFAT: skip finalize delete (unexpected secondary_count)");
        return Ok(());
    };
    let total = entries.len();
    let offsets = dir.offsets(primary_slot, total)?;

    entries[0][0] = DELETED_FILE;
    for i in 1..total {
        entries[i][0] &= 0x7f;
    }
    if entries[1][0] == EXFAT_STREAM_EXT_DELETED {
        // Clear allocation metadata so the entry does not reference freed clusters.
        entries[1][8..32].fill(0);
        // AllocationPossible (bit 0) only; drop NoFatChain so nothing implies a FAT chain.
        entries[1][1] = 0x01;
    }

    let checksum = exfat_compute_entry_set_checksum(&entries);
    entries[0][2] = (checksum & 0xff) as u8;
    entries[0][3] = (checksum >> 8) as u8;
    exfat_write_entry_slabs_at(vol, part_start, part_bytes, &offsets, &entries)?;
    trace("exFAT: finalized deleted entry set (stream zeroed, checksum, full write)");
    Ok(())
}

/// Release the clusters `entry` occupies — in the bitmap, and in the FAT for a contiguous run —
/// without touching its entry set. The bitmap change is only in memory until `sync_bitmap`.
///
/// Shared by deleting an entry and by replacing a file, which frees the original's clusters only
/// once the replacement's entry set has taken its slots (#200).
fn exfat_free_entry_clusters(
    fs: &ExFatFs<PartitionDiskUnion>,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    entry: &ExfatDecodedEntry,
) -> io::Result<()> {
    if entry.first_cluster < 2 {
        return Ok(());
    }
    let info = fs.info();
    let cs = info.bytes_per_cluster as u64;
    let cluster_count = if entry.no_fat_chain {
        let stream_len = entry.data_length.max(entry.valid_data_length);
        ((stream_len + cs - 1) / cs).max(1) as u32
    } else {
        0
    };
    fs.free_clusters(entry.first_cluster, cluster_count, entry.no_fat_chain)
        .map_err(exfat_err)?;
    if entry.no_fat_chain && cluster_count > 0 {
        exfat_clear_fat_contiguous_range(
            vol,
            part_start,
            part_bytes,
            info,
            entry.first_cluster,
            cluster_count,
        )?;
    }
    Ok(())
}

/// hadris `ExFatFs::delete` passes [`hadris_fat::exfat::ExFatFileEntry::entry_offset`] to
/// `write_at`, but entries from directory iteration store **directory-stream-relative** offsets
/// (`exfat/dir.rs`), while `create_file` sets **volume-absolute** offsets (`fs.rs`). Deletes of
/// files opened via `open_path` therefore write `0x05` to the wrong byte and never remove the
/// listing entry. We find the entry set on disk with [`exfat_locate_entry_set`], then free
/// clusters, mark deleted, fix checksum, and sync the bitmap — matching hadris intent.
fn exfat_delete_entry_resolved(
    fs: &ExFatFs<PartitionDiskUnion>,
    entry_path: &str,
    entry: &ExfatDecodedEntry,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    if entry.is_dir() && entry.first_cluster >= 2 {
        // The entry already says where its contents are, so the check reads them directly rather
        // than resolving the path again through hadris — which could not see a directory whose own
        // entry sits past the root's first cluster (#190).
        let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
        let children = ExfatDirSlots::load_with(
            &mut read_at,
            fs.info(),
            entry.first_cluster,
            entry.no_fat_chain,
            entry.data_length,
        )?;
        if !exfat_list_entries(&children, &mut read_at)?.is_empty() {
            return Err(io::Error::other("exFAT directory is not empty"));
        }
    }

    trace("exFAT: resolve entry set volume offset…");
    let (dir, primary_slot) = exfat_locate_entry_set(
        fs,
        entry_path,
        &ExfatEntryIdentity::of_decoded(entry),
        vol,
        part_start,
        part_bytes,
    )?;

    trace("exFAT: free file/directory clusters…");
    exfat_free_entry_clusters(fs, vol, part_start, part_bytes, entry)?;
    trace("exFAT: free clusters OK");

    trace("exFAT: finalize deleted entry set (0x05, zero stream, checksum)…");
    exfat_finalize_deleted_entry_set(vol, part_start, part_bytes, &dir, primary_slot, trace)?;
    trace("exFAT: deleted entry set finalized OK");
    trace("exFAT: sync_bitmap…");
    fs.sync_bitmap().map_err(exfat_err)?;
    trace("exFAT: sync_bitmap OK");
    Ok(())
}

fn mkdir_cart_exfat(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    path: &str,
) -> io::Result<()> {
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    let fs = ExFatFs::open(disk).map_err(exfat_err)?;
    let normalized = path.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let info = fs.info().clone();
    exfat_create_missing_dirs(&fs, &info, vol, part_start, part_bytes, &parts)?;
    drop(fs);
    vol.flush_serial()?;
    Ok(())
}

fn rename_cart_fat(
    link: Arc<Mutex<Sc64Link>>,
    part_start: u64,
    part_bytes: u64,
    parent_path: &str,
    name_from: &str,
    name_to: &str,
) -> io::Result<()> {
    let disk = Sc64PartitionDisk::new_writable(link, part_start, part_bytes);
    rename_fat_impl(disk, parent_path, name_from, name_to)
}

/// FAT `mkdir -p` — shared by [`Sc64SdSession::mkdir_cart`] and RAM-disk tests.
fn mkdir_fat_impl<D: Read + Write + Seek>(disk: D, path: &str) -> io::Result<()> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let normalized = path.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    {
        let mut dir = fs.root_dir();
        for p in parts {
            dir = match dir.open_dir(p) {
                Ok(d) => d,
                Err(_) => dir.create_dir(p)?,
            };
        }
    }
    // Unmount writes FSInfo and clears the dirty flag; `Drop` only logs a failure (#129).
    fs.unmount()
}

/// FAT rename within `parent_path` — shared by [`rename_cart_fat`] and RAM-disk tests.
fn rename_fat_impl<D: Read + Write + Seek>(
    disk: D,
    parent_path: &str,
    name_from: &str,
    name_to: &str,
) -> io::Result<()> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let normalized = parent_path.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    {
        let mut dir = fs.root_dir();
        for p in parts {
            dir = dir.open_dir(p)?;
        }
        dir.rename(name_from, &dir, name_to)?;
    }
    // See `mkdir_fat_impl`: an unmount failure must reach the caller (#129).
    fs.unmount()
}

fn rename_cart_exfat(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    from_rel: &str,
    to_rel: &str,
) -> io::Result<()> {
    let (parent_path, _name_from) = cart_path_parts(from_rel);
    let name_to = cart_path_parts(to_rel).1;
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    let fs = ExFatFs::open(disk).map_err(exfat_err)?;
    let old = exfat_resolve_entry_vol(vol, part_start, part_bytes, fs.info(), from_rel)?;
    let (dir, old_slot) = exfat_locate_entry_set(
        &fs,
        from_rel,
        &ExfatEntryIdentity::of_decoded(&old),
        vol,
        part_start,
        part_bytes,
    )?;
    let to_path = if parent_path.is_empty() {
        name_to.clone()
    } else {
        format!("{parent_path}/{name_to}")
    };
    // The destination check reads the parent through this crate's own resolver: hadris's `find`
    // cannot see a name past the parent's first cluster, so it would report "free" for a name that
    // is really there and the rename would create a duplicate (#190).
    let existing = {
        let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
        let parent_slots = exfat_resolve_dir_slots(&mut read_at, fs.info(), &parent_path)?;
        exfat_list_entries(&parent_slots, &mut read_at)?
            .into_iter()
            .find(|e| fat_names_equal(&e.name, &name_to))
    };
    if let Some(candidate) = existing {
        let (_, cand_slot) = exfat_locate_entry_set(
            &fs,
            &to_path,
            &ExfatEntryIdentity::of_decoded(&candidate),
            vol,
            part_start,
            part_bytes,
        )?;
        if cand_slot != old_slot {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a file or folder already exists with that name",
            ));
        }
    }
    exfat_validate_rename_name(&name_to)?;
    let new_slabs = exfat_build_rename_entry_set_bytes(&fs, &old, &name_to)?;
    let new_total = new_slabs.len();
    // Read before anything is written. The slots marked deleted below are never written first, so
    // what was read is still what is on disk.
    let read_at = exfat_volume_reader(vol, part_start, part_bytes);
    let old_entries = exfat_read_entry_set(&dir, old_slot, read_at)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid exFAT secondary_count on disk",
        )
    })?;
    let old_total = old_entries.len();
    let old_offsets = dir.offsets(old_slot, old_total)?;
    if new_total <= old_total {
        exfat_write_entry_slabs_at(
            vol,
            part_start,
            part_bytes,
            &old_offsets[..new_total],
            &new_slabs,
        )?;
        exfat_mark_slots_deleted(
            vol,
            part_start,
            part_bytes,
            &old_offsets[new_total..],
            &old_entries[new_total..],
        )?;
    } else {
        let old_slots = old_slot..old_slot + old_total as u64;
        let read_at = exfat_volume_reader(vol, part_start, part_bytes);
        let dest_slot = exfat_find_free_entry_run(&dir, new_total, old_slots, read_at)?;
        let dest_offsets = dir.offsets(dest_slot, new_total)?;
        exfat_write_entry_slabs_at(vol, part_start, part_bytes, &dest_offsets, &new_slabs)?;
        exfat_mark_slots_deleted(vol, part_start, part_bytes, &old_offsets, &old_entries)?;
    }
    // No bitmap write: a rename moves directory entries within the folder's own clusters, and takes
    // or frees none, so there is nothing to persist, and the whole-bitmap write was the dominant
    // cost of the operation (#196, #224).
    drop(fs);
    vol.flush_serial()?;
    Ok(())
}

/// The entry set for a **new** entry: File, Stream Extension, then one File Name entry per 15
/// UTF-16 units, with the set checksum filled in.
///
/// The field values follow what Windows' own driver writes, read off a card: attributes as given,
/// and **ValidDataLength equal to DataLength**. hadris's builder instead leaves ValidDataLength `0`
/// for a directory, which is why every directory created through it carries a value Windows would
/// not have written (#194 — measured harmless, but there is no reason to keep writing it).
///
/// `contiguous` sets the NoFatChain flag. A directory is always one cluster here, so its caller
/// passes `true`; a file passes whatever its allocation turned out to be, since a fragmented one is
/// described by the FAT instead and must not claim otherwise.
fn exfat_build_new_entry_set_bytes(
    fs: &ExFatFs<PartitionDiskUnion>,
    name: &str,
    attributes: u16,
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> io::Result<Vec<[u8; 32]>> {
    exfat_validate_rename_name(name)?;
    let name_utf16: Vec<u16> = name.encode_utf16().collect();
    let name_len = name_utf16.len();
    let name_entry_count = (name_len + EXFAT_CHARS_PER_NAME_ENTRY - 1) / EXFAT_CHARS_PER_NAME_ENTRY;
    let secondary_count = (1 + name_entry_count) as u8;
    let (ts, ts_10ms, ts_utc) = ExFatTimestamp::now().to_raw();
    let file_entry = RawFileDirectoryEntry {
        entry_type: EXFAT_ENTRY_FILE_DIRECTORY,
        secondary_count,
        set_checksum: U16::<LittleEndian>::new(0),
        file_attributes: U16::<LittleEndian>::new(attributes),
        reserved1: U16::<LittleEndian>::new(0),
        create_timestamp: U32::<LittleEndian>::new(ts),
        last_modified_timestamp: U32::<LittleEndian>::new(ts),
        last_accessed_timestamp: U32::<LittleEndian>::new(ts),
        create_10ms_increment: ts_10ms,
        last_modified_10ms_increment: ts_10ms,
        create_utc_offset: ts_utc,
        last_modified_utc_offset: ts_utc,
        last_accessed_utc_offset: ts_utc,
        reserved2: [0; 7],
    };
    let stream_entry = RawStreamExtensionEntry {
        entry_type: EXFAT_ENTRY_STREAM_EXT,
        // Bit 0 AllocationPossible, always set on a Stream Extension entry; bit 1 NoFatChain, set
        // when the stream's clusters are consecutive so no FAT chain describes them. Same rule as
        // hadris's `build_stream_entry` (`exfat/entry_writer.rs`), rather than a second one that
        // could drift from it.
        general_secondary_flags: 0x01 | if contiguous { 0x02 } else { 0x00 },
        reserved1: 0,
        name_length: name_len as u8,
        name_hash: U16::<LittleEndian>::new(fs.name_hash(name)),
        reserved2: U16::<LittleEndian>::new(0),
        valid_data_length: U64::<LittleEndian>::new(data_length),
        reserved3: U32::<LittleEndian>::new(0),
        first_cluster: U32::<LittleEndian>::new(first_cluster),
        data_length: U64::<LittleEndian>::new(data_length),
    };
    let mut out = Vec::with_capacity(1 + secondary_count as usize);
    out.push(exfat_struct_to_entry_bytes(&file_entry));
    out.push(exfat_struct_to_entry_bytes(&stream_entry));
    for chunk in name_utf16.chunks(EXFAT_CHARS_PER_NAME_ENTRY) {
        let mut file_name = [0u8; 30];
        for (i, &u) in chunk.iter().enumerate() {
            let b = u.to_le_bytes();
            file_name[i * 2] = b[0];
            file_name[i * 2 + 1] = b[1];
        }
        out.push(exfat_struct_to_entry_bytes(&RawFileNameEntry {
            entry_type: EXFAT_ENTRY_FILE_NAME,
            general_secondary_flags: 0,
            file_name,
        }));
    }
    let checksum = exfat_compute_entry_set_checksum(&out);
    out[0][2] = (checksum & 0xff) as u8;
    out[0][3] = (checksum >> 8) as u8;
    Ok(out)
}

/// Create one directory named `name` inside the already-resolved directory `parent`.
///
/// Written here rather than through hadris's `create_dir`, which finds its free slots by a scan
/// that runs past the directory's end and cannot see a chained root's later clusters (#175, #190).
/// Because this places the entry set through [`ExfatDirSlots`], a directory can be created in a
/// root that spans several clusters, which had to be refused outright while hadris was doing the
/// writing.
///
/// Like hadris, this does **not** extend a full directory: with no free run long enough it fails
/// with `StorageFull` rather than growing the directory.
fn exfat_create_dir_in(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    parent: &ExfatDirSlots,
    name: &str,
) -> io::Result<()> {
    let cluster_bytes = info.bytes_per_cluster as u64;

    // Refuse a bad name, and reserve the directory slots, before taking a cluster: after it, a
    // failure has a cluster to give back.
    exfat_validate_rename_name(name)?;
    let slot_count = exfat_entry_set_len(name);
    let read_at = exfat_volume_reader(vol, part_start, part_bytes);
    let slot = exfat_find_free_entry_run(parent, slot_count, 0..0, read_at)?;
    let offsets = parent.offsets(slot, slot_count)?;

    let cluster = fs.allocate_cluster(2).map_err(exfat_err)?;
    let result = (|| -> io::Result<Vec<[u8; 32]>> {
        if !info.is_valid_cluster(cluster) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT allocated an out-of-range cluster",
            ));
        }
        // `allocate_cluster` marks the bitmap and writes end-of-chain into the FAT. The entry
        // below sets NoFatChain, and exFAT requires the FAT entries of such an allocation to be
        // free, so the end-of-chain it just wrote has to come back out. Windows chkdsk reports the
        // mismatch.
        exfat_clear_fat_contiguous_range(vol, part_start, part_bytes, info, cluster, 1)?;
        // An empty exFAT directory is simply a zeroed cluster: there are no "." or ".." entries,
        // and the first zero byte is the end-of-directory marker.
        let base = info.cluster_to_offset(cluster);
        exfat_write_bytes_at(
            vol,
            part_start,
            part_bytes,
            base,
            &vec![0u8; cluster_bytes as usize],
        )?;
        let slabs = exfat_build_new_entry_set_bytes(
            fs,
            name,
            EXFAT_ATTR_DIRECTORY,
            cluster,
            cluster_bytes,
            true,
        )?;
        debug_assert_eq!(slabs.len(), slot_count);
        Ok(slabs)
    })();
    let written = match result {
        Ok(slabs) => {
            exfat_write_entry_slabs_restoring(vol, part_start, part_bytes, &offsets, &slabs)
        }
        Err(error) => Err(ExfatEntryWriteFailed {
            error,
            restored: true,
        }),
    };
    match written {
        Ok(()) => Ok(()),
        // An out-of-range cluster is not one to hand back.
        Err(failed) if !info.is_valid_cluster(cluster) => Err(failed.error),
        Err(failed) => Err(exfat_undo_failed_create(
            fs,
            info,
            vol,
            part_start,
            part_bytes,
            &[cluster],
            failed,
        )),
    }
}

/// Undo a file or folder create that failed, given the `clusters` it took, and return the error to
/// report.
///
/// When the directory is known to be as it was, the clusters go back. When the entry set may be
/// part-written and could not be put back (#215), they stay allocated, bitmap written, and so does
/// a replaced original's: the card may now hold an entry that refers to them, and freeing clusters
/// something still points at is how data gets overwritten. Space lost that way is what a disk
/// check recovers, which is what the error says to run.
fn exfat_undo_failed_create(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    clusters: &[u32],
    failed: ExfatEntryWriteFailed,
) -> io::Error {
    if failed.restored {
        exfat_release_file_clusters(fs, info, vol, part_start, part_bytes, clusters);
        return failed.error;
    }
    let _ = fs.sync_bitmap();
    io::Error::new(
        failed.error.kind(),
        format!(
            "the directory entry was only partly written and could not be put back, so the \
             folder may be damaged; run a disk check on the card (chkdsk on Windows): {}",
            failed.error
        ),
    )
}

/// Walk `parts` below the root, creating each folder that does not exist yet, and return the
/// path they make up.
///
/// A folder's entry set reaches the card as soon as it is made, but its cluster is only marked in
/// hadris's in-memory bitmap. So when anything was created, the bitmap is written before this
/// returns, **on failure too** (#213): a later folder that cannot be made (an invalid name, a full
/// directory, a USB error) otherwise leaves the earlier ones on the card over clusters the on-disk
/// bitmap still calls free, to be handed out again. When nothing was created there is nothing to
/// persist, and the whole-bitmap write, the dominant cost of an operation (#196), is skipped.
fn exfat_create_missing_dirs(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    parts: &[&str],
) -> io::Result<String> {
    let mut made_a_directory = false;
    let mut path_prefix = String::new();
    let walked = (|| -> io::Result<()> {
        for p in parts {
            let mut read_at = exfat_volume_reader(vol, part_start, part_bytes);
            let parent = exfat_resolve_dir_slots(&mut read_at, info, &path_prefix)?;
            let existing = exfat_list_entries(&parent, &mut read_at)?
                .into_iter()
                .find(|e| fat_names_equal(&e.name, p));
            match existing {
                Some(e) if e.is_dir() => {}
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "a file already exists with that name",
                    ))
                }
                None => {
                    exfat_create_dir_in(fs, info, vol, part_start, part_bytes, &parent, p)?;
                    made_a_directory = true;
                }
            }
            if !path_prefix.is_empty() {
                path_prefix.push('/');
            }
            path_prefix.push_str(p);
        }
        Ok(())
    })();
    if made_a_directory {
        let synced = fs.sync_bitmap().map_err(exfat_err);
        walked?;
        synced?;
    } else {
        walked?;
    }
    Ok(path_prefix)
}

/// Copy up to `len` bytes of `data` into `clusters`, in order, returning how many bytes arrived.
///
/// The final cluster is zero-padded to its end. A freshly allocated cluster holds whatever the last
/// file to own it left behind, and `data_length` is what stops a reader seeing that tail, so the
/// padding is belt and braces — but it costs one buffer clear and means a short file does not carry
/// a stranger's data around on the card.
fn exfat_write_stream_to_clusters(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    clusters: &[u32],
    len: u64,
    data: &mut impl Read,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<u64> {
    let cluster_bytes = info.bytes_per_cluster as u64;
    let mut buf = vec![0u8; cluster_bytes as usize];
    let mut written = 0u64;

    for &cluster in clusters {
        let want = (len - written).min(cluster_bytes) as usize;
        buf[..want].fill(0);
        let mut got = 0usize;
        while got < want {
            let n = data.read(&mut buf[got..want])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        // Zero the rest of the cluster, both the tail of a partial final cluster and anything a
        // short source did not supply.
        buf[got..].fill(0);
        exfat_write_bytes_at(
            vol,
            part_start,
            part_bytes,
            info.cluster_to_offset(cluster),
            &buf,
        )?;
        written += got as u64;
        if got > 0 && !progress(got as u64) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if got < want {
            break;
        }
    }
    Ok(written)
}

/// Whether `data` still has bytes to give, reading at most one of them.
fn exfat_source_has_more(data: &mut impl Read) -> io::Result<bool> {
    let mut probe = [0u8; 1];
    loop {
        match data.read(&mut probe) {
            Ok(n) => return Ok(n > 0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Create the file `name` in the already-resolved directory `parent`, with `len` bytes from `data`.
///
/// Written here rather than through hadris's `create_file` + `write_file` for the same reason
/// [`exfat_create_dir_in`] exists: hadris finds its free slots by a scan that runs past the
/// directory's end and cannot see a chained root's later clusters (#175, #190). Placing the entry
/// set through [`ExfatDirSlots`] is what lets a file be imported into a root spanning clusters,
/// which until now had to be refused.
///
/// `len` is the length the caller expects, which every real caller knows —
/// [`Sc64SdSession::import_from_pc_with_progress`] has it from `std::fs::metadata`. Knowing it up
/// front is what lets [`exfat_allocate_file_clusters`] look for one contiguous run, and so write a
/// NoFatChain stream where the free space permits, instead of growing a chain cluster by cluster. A
/// source that then delivers a different number of bytes, fewer or more (#220), is an error and is
/// rolled back: it changed underneath us, and writing an entry whose `data_length` disagrees with
/// the data is worse than writing nothing.
///
/// Nothing is left behind on a failure. The clusters are released, and the entry set — the only
/// thing that makes a file reachable — is written last, so a reader never sees a partial file. The
/// one exception is an entry-set write that fails part-way and cannot be put back (#215): see
/// [`exfat_undo_failed_create`].
///
/// **Replacing** (`replacing` is the existing file at `rel_path`) keeps the original intact until
/// the very end (#200). The new data goes into clusters of its own while the original's entry set
/// and clusters are untouched, so a cancel, a transfer error or a short source leaves the original
/// exactly as it was. Only then is the new entry set written **over the original's slots** — the
/// same name, so the same number of slots, and no need for room for a second set — and the
/// original's clusters released. The card must hold both copies for that moment; a replace that
/// does not fit is refused rather than falling back to deleting first, which is the data loss this
/// ordering exists to prevent.
#[allow(clippy::too_many_arguments)]
fn exfat_create_file_in(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    parent: &ExfatDirSlots,
    rel_path: &str,
    name: &str,
    replacing: Option<&ExfatDecodedEntry>,
    len: u64,
    data: &mut impl Read,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    exfat_validate_rename_name(name)?;
    let cluster_bytes = info.bytes_per_cluster as u64;
    let slot_count = exfat_entry_set_len(name);

    // Find where the entry set will go before allocating anything, so a directory with no room
    // fails having written nothing and marked nothing in the bitmap. A replacement goes exactly
    // where the original's set is.
    let offsets = match replacing {
        None => {
            let read_at = exfat_volume_reader(vol, part_start, part_bytes);
            let first_slot = exfat_find_free_entry_run(parent, slot_count, 0..0, read_at)?;
            parent.offsets(first_slot, slot_count)?
        }
        Some(old) => {
            let (dir, slot) = exfat_locate_entry_set(
                fs,
                rel_path,
                &ExfatEntryIdentity::of_decoded(old),
                vol,
                part_start,
                part_bytes,
            )?;
            // Names are matched case-insensitively with a one-to-one up-case table, so a matching
            // name has the same UTF-16 length and the same slot count. Anything else means the set
            // on disk is not what the listing described; stop before touching it.
            if 1 + usize::from(old.primary[1]) != slot_count {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the existing file's entry set does not match its name; not replacing it",
                ));
            }
            dir.offsets(slot, slot_count)?
        }
    };

    let cluster_count = u32::try_from(len.div_ceil(cluster_bytes))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "exFAT file too large"))?;

    // Filled as clusters are taken, so a failure part-way through the allocation itself is rolled
    // back as completely as a failure after it.
    let mut clusters: Vec<u32> = Vec::with_capacity(cluster_count as usize);

    let result = (|| -> io::Result<Vec<[u8; 32]>> {
        let contiguous = exfat_allocate_file_clusters(
            fs,
            info,
            vol,
            part_start,
            part_bytes,
            cluster_count,
            &mut clusters,
        )
        .map_err(|e| match (e.kind(), replacing) {
            (io::ErrorKind::StorageFull, Some(_)) => io::Error::new(
                io::ErrorKind::StorageFull,
                "not enough free space to replace this file safely: the new copy is written \
                 before the old one is removed, so the card needs room for both. The original \
                 is unchanged; delete it first to make room, then copy again.",
            ),
            _ => e,
        })?;
        let written = exfat_write_stream_to_clusters(
            vol, part_start, part_bytes, info, &clusters, len, data, progress,
        )?;
        if written != len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("source supplied {written} bytes where {len} were expected"),
            ));
        }
        // The stream stops at `len`, so a source that grew since it was measured would otherwise
        // be cut short without a word (#220).
        if exfat_source_has_more(data)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the file grew while it was being copied; copy it again once it has stopped \
                 changing",
            ));
        }
        // An empty file has no allocation: first cluster 0, length 0, reported as contiguous, which
        // is how hadris's own `create_file` left the empty files already on this project's cards.
        let first_cluster = clusters.first().copied().unwrap_or(0);
        let slabs = exfat_build_new_entry_set_bytes(
            fs,
            name,
            EXFAT_ATTR_ARCHIVE,
            first_cluster,
            len,
            contiguous,
        )?;
        debug_assert_eq!(slabs.len(), slot_count);
        Ok(slabs)
    })();

    let written = match result {
        Ok(slabs) => {
            exfat_write_entry_slabs_restoring(vol, part_start, part_bytes, &offsets, &slabs)
        }
        Err(error) => Err(ExfatEntryWriteFailed {
            error,
            restored: true,
        }),
    };
    if let Err(failed) = written {
        return Err(exfat_undo_failed_create(
            fs, info, vol, part_start, part_bytes, &clusters, failed,
        ));
    }

    // From here the new file is the one on the card. The original's clusters are unreferenced; a
    // failure to release them costs space, not data, and must not roll the new file back.
    let freed = match replacing {
        Some(old) => exfat_free_entry_clusters(fs, vol, part_start, part_bytes, old),
        None => Ok(()),
    };
    // Whether or not that worked: the new file's clusters are marked only in memory until this
    // runs, and a card that has the entry without them hands them out again (#214).
    fs.sync_bitmap().map_err(exfat_err)?;
    freed.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("replaced the file, but could not free the old copy's space: {e}"),
        )
    })
}

/// Take `count` clusters for a new file from the **allocation bitmap**, pushing each onto
/// `clusters` as it is taken; returns whether they are one contiguous run.
///
/// Not hadris's `allocate_clusters`, and the reason is data loss. Its fallback for fragmented free
/// space, `ExFatTable::allocate_chain`, looks for free clusters by scanning the **FAT** for zero
/// entries. In exFAT that is not what free means: a contiguous (NoFatChain) file's clusters have
/// zero FAT entries by definition, and only the bitmap records them as in use. So on a card with
/// no free run long enough, it hands out clusters belonging to live files and the new file's data
/// is written over theirs. `exfat_fragmented_import_chains_through_free_clusters_without_touching_other_files`
/// shows exactly that. `ExFatFs::allocate_cluster`, which this uses, searches the bitmap.
///
/// A contiguous run is preferred, and is written NoFatChain with its FAT entries freed. Otherwise
/// the clusters are linked into a FAT chain, lowest first. Checking the free count first means a
/// volume that cannot hold the file fails before anything is taken.
#[allow(clippy::too_many_arguments)]
fn exfat_allocate_file_clusters(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    count: u32,
    clusters: &mut Vec<u32>,
) -> io::Result<bool> {
    if count == 0 {
        return Ok(true);
    }
    if fs.free_cluster_count() < count {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "not enough free space on the card for this file",
        ));
    }

    // The bitmap is held in memory, so this scan costs no USB traffic.
    let last_cluster = info.cluster_count + 1;
    let mut run_start = None;
    let mut run_len = 0u32;
    for c in 2..=last_cluster {
        if fs.is_cluster_allocated(c).map_err(exfat_err)? {
            run_len = 0;
        } else {
            if run_len == 0 {
                run_start = Some(c);
            }
            run_len += 1;
            if run_len == count {
                break;
            }
        }
    }

    if run_len == count {
        let start = run_start.expect("a run has a start");
        // Clusters the bitmap calls free but the FAT still chains mean the two already disagree.
        // Claiming them NoFatChain would bury that; refuse instead of writing over corruption this
        // did not cause.
        if !exfat_should_zero_fat_for_nofatchain_range(
            vol, part_start, part_bytes, info, start, count,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the card's FAT and allocation bitmap disagree about clusters {start}..{}; \
                     run a disk check before writing to it",
                    start + count - 1
                ),
            ));
        }
        for i in 0..count {
            let want = start + i;
            let got = fs.allocate_cluster(want).map_err(exfat_err)?;
            clusters.push(got);
            if got != want {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "exFAT allocation moved while taking a free run",
                ));
            }
        }
        // `allocate_cluster` writes end-of-chain into each FAT entry; a NoFatChain run's must be free.
        exfat_clear_fat_contiguous_range(vol, part_start, part_bytes, info, start, count)?;
        return Ok(true);
    }

    let mut hint = 2;
    for _ in 0..count {
        let c = fs.allocate_cluster(hint).map_err(exfat_err)?;
        if !info.is_valid_cluster(c) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT allocated an out-of-range cluster",
            ));
        }
        clusters.push(c);
        hint = c.saturating_add(1);
    }
    // Each entry already holds end-of-chain from `allocate_cluster`; point every one but the last
    // at its successor.
    let links: Vec<(u32, u32)> = clusters.windows(2).map(|w| (w[0], w[1])).collect();
    exfat_write_fat_entries(vol, part_start, part_bytes, info, &links)?;
    Ok(false)
}

/// Undo [`exfat_allocate_file_clusters`]: free each cluster in the bitmap and in the FAT.
///
/// Best effort, because it runs on a path that is already returning an error. Both halves matter:
/// `allocate_cluster` writes the FAT on the volume immediately, while the bitmap change is only in
/// memory until `sync_bitmap`, so freeing one without the other leaves them disagreeing.
fn exfat_release_file_clusters(
    fs: &ExFatFs<PartitionDiskUnion>,
    info: &hadris_fat::exfat::ExFatInfo,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    clusters: &[u32],
) {
    if clusters.is_empty() {
        return;
    }
    for &c in clusters {
        let _ = fs.free_clusters(c, 1, true);
    }
    let free: Vec<(u32, u32)> = clusters.iter().map(|&c| (c, 0)).collect();
    let _ = exfat_write_fat_entries(vol, part_start, part_bytes, info, &free);
    let _ = fs.sync_bitmap();
}

fn exfat_validate_rename_name(name: &str) -> io::Result<()> {
    let len = name.encode_utf16().count();
    if len == 0 || len > EXFAT_MAX_FILENAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid exFAT filename length",
        ));
    }
    for c in name.chars() {
        if exfat_invalid_filename_char(c) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid character in filename",
            ));
        }
    }
    Ok(())
}

fn exfat_invalid_filename_char(c: char) -> bool {
    matches!(
        c,
        '\0'..='\x1F' | '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|'
    )
}

fn exfat_build_rename_entry_set_bytes(
    fs: &ExFatFs<PartitionDiskUnion>,
    entry: &ExfatDecodedEntry,
    new_name: &str,
) -> io::Result<Vec<[u8; 32]>> {
    exfat_validate_rename_name(new_name)?;
    let name_utf16: Vec<u16> = new_name.encode_utf16().collect();
    let name_len = name_utf16.len();
    let name_entry_count = (name_len + EXFAT_CHARS_PER_NAME_ENTRY - 1) / EXFAT_CHARS_PER_NAME_ENTRY;
    let secondary_count = (1 + name_entry_count) as u8;
    // Keep the File entry as it is on disk and change only what a rename must: the secondary
    // count, and the checksum, recomputed once the set is built. Timestamps, attributes and the
    // reserved bytes are copied rather than rebuilt, so a rename cannot alter them — rebuilding
    // through hadris's `ExFatTimestamp` would clamp `increment_10ms` and rewrite an invalid UTC
    // offset, quietly changing a file's times on every rename.
    let mut file_entry = entry.primary;
    file_entry[1] = secondary_count;
    file_entry[2] = 0;
    file_entry[3] = 0;
    let flags = 0x01u8 | if entry.no_fat_chain { 0x02 } else { 0x00 };
    let stream_entry = RawStreamExtensionEntry {
        entry_type: EXFAT_ENTRY_STREAM_EXT,
        general_secondary_flags: flags,
        reserved1: 0,
        name_length: name_len as u8,
        name_hash: U16::<LittleEndian>::new(fs.name_hash(new_name)),
        reserved2: U16::<LittleEndian>::new(0),
        valid_data_length: U64::<LittleEndian>::new(entry.valid_data_length),
        reserved3: U32::<LittleEndian>::new(0),
        first_cluster: U32::<LittleEndian>::new(entry.first_cluster),
        data_length: U64::<LittleEndian>::new(entry.data_length),
    };
    let mut out = Vec::with_capacity(1 + secondary_count as usize);
    out.push(file_entry);
    out.push(exfat_struct_to_entry_bytes(&stream_entry));
    for chunk in name_utf16.chunks(EXFAT_CHARS_PER_NAME_ENTRY) {
        let mut file_name = [0u8; 30];
        for (i, &u) in chunk.iter().enumerate() {
            if i >= EXFAT_CHARS_PER_NAME_ENTRY {
                break;
            }
            let b = u.to_le_bytes();
            file_name[i * 2] = b[0];
            file_name[i * 2 + 1] = b[1];
        }
        let name_raw = RawFileNameEntry {
            entry_type: EXFAT_ENTRY_FILE_NAME,
            general_secondary_flags: 0,
            file_name,
        };
        out.push(exfat_struct_to_entry_bytes(&name_raw));
    }
    let checksum = exfat_compute_entry_set_checksum(&out);
    out[0][2] = (checksum & 0xff) as u8;
    out[0][3] = (checksum >> 8) as u8;
    Ok(out)
}

fn exfat_struct_to_entry_bytes<T: bytemuck::NoUninit>(entry: &T) -> [u8; 32] {
    let b = bytemuck::bytes_of(entry);
    let mut out = [0u8; 32];
    out[..b.len()].copy_from_slice(b);
    out
}

/// Write `slabs[i]` at volume byte offset `offsets[i]` (see [`ExfatDirSlots::offsets`]).
/// Write `bytes` at a volume byte offset, refusing a write that would run past the partition.
fn exfat_write_bytes_at(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offset: u64,
    bytes: &[u8],
) -> io::Result<()> {
    if offset.saturating_add(bytes.len() as u64) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT write past partition",
        ));
    }
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    disk.seek(SeekFrom::Start(offset))?;
    disk.write_all(bytes)?;
    disk.flush()?;
    Ok(())
}

/// [`exfat_write_entry_slabs_restoring`], for callers that only need to know it failed.
fn exfat_write_entry_slabs_at(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offsets: &[u64],
    slabs: &[[u8; 32]],
) -> io::Result<()> {
    exfat_write_entry_slabs_restoring(vol, part_start, part_bytes, offsets, slabs)
        .map_err(|failed| failed.error)
}

/// An entry-set write that failed, and whether the directory is known to be as it was before.
struct ExfatEntryWriteFailed {
    error: io::Error,
    /// Every sector the write may have changed was written back with its earlier contents.
    restored: bool,
}

/// Write each 32-byte slab at its offset, **one whole sector at a time**, and on a failure put back
/// the sectors already written (#215).
///
/// A set spans several slots, and each written on its own was a separate sector write, so a USB
/// failure part-way through left a mix: a File entry whose checksum no longer matched its
/// secondaries, or a complete-looking entry pointing at clusters the caller was about to free.
/// Grouping the slabs by sector makes a set that fits in one sector (most of them) a single
/// write. One that crosses a sector boundary still takes more than one, so each sector's earlier
/// contents are kept and written back, the failed sector included (a write that timed out may
/// still have landed), in reverse order.
fn exfat_write_entry_slabs_restoring(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offsets: &[u64],
    slabs: &[[u8; 32]],
) -> Result<(), ExfatEntryWriteFailed> {
    debug_assert_eq!(offsets.len(), slabs.len());
    let untouched = |error| ExfatEntryWriteFailed {
        error,
        restored: true,
    };
    // Consecutive slabs in the same sector, in order: (sector start, indices into `slabs`).
    let mut sectors: Vec<(u64, Vec<usize>)> = Vec::new();
    for (i, &offset) in offsets.iter().enumerate() {
        let start = offset - offset % 512;
        if start.saturating_add(512) > part_bytes {
            return Err(untouched(io::Error::new(
                io::ErrorKind::InvalidData,
                "exFAT entry write past partition",
            )));
        }
        match sectors.last_mut() {
            Some((s, group)) if *s == start => group.push(i),
            _ => sectors.push((start, vec![i])),
        }
    }

    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    let mut before: Vec<(u64, [u8; 512])> = Vec::with_capacity(sectors.len());
    for (start, group) in &sectors {
        let written = (|| -> io::Result<()> {
            let mut sector = [0u8; 512];
            disk.seek(SeekFrom::Start(*start))?;
            disk.read_exact(&mut sector)?;
            before.push((*start, sector));
            for &i in group {
                let within = (offsets[i] - start) as usize;
                sector[within..within + 32].copy_from_slice(&slabs[i]);
            }
            disk.seek(SeekFrom::Start(*start))?;
            disk.write_all(&sector)?;
            disk.flush()
        })();
        if let Err(error) = written {
            let mut restored = true;
            for (start, sector) in before.iter().rev() {
                restored &= disk
                    .seek(SeekFrom::Start(*start))
                    .and_then(|_| disk.write_all(sector))
                    .and_then(|_| disk.flush())
                    .is_ok();
            }
            return Err(ExfatEntryWriteFailed { error, restored });
        }
    }
    Ok(())
}

/// The entry set whose File entry is slot `primary_slot`: that entry, then its secondaries. `None`
/// when the File entry's secondary count is outside 2..=18, the range exFAT allows.
///
/// Read through [`ExfatSlotReader`], so the set costs one disk read (one SC64 USB round trip) per
/// chunk it touches, usually one, rather than one per slot.
fn exfat_read_entry_set(
    dir: &ExfatDirSlots,
    primary_slot: u64,
    read_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Option<Vec<[u8; 32]>>> {
    let mut slots = ExfatSlotReader::new(dir, read_at);
    let primary = slots.slot(primary_slot)?;
    let secondaries = u64::from(primary[1]);
    if !(2..=18).contains(&secondaries) {
        return Ok(None);
    }
    (primary_slot..=primary_slot + secondaries)
        .map(|s| slots.slot(s))
        .collect::<io::Result<Vec<_>>>()
        .map(Some)
}

/// Mark directory slots unused by clearing each one's In-Use bit (`0x85`→`0x05`, `0xC0`→`0x40`,
/// `0xC1`→`0x41`). Zeroing them instead writes `0x00`, end of directory, and every entry after
/// that point disappears from listings and can be overwritten (#127).
///
/// `entries` are the slots' contents, as read by [`exfat_read_entry_set`]: this only writes.
fn exfat_mark_slots_deleted(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offsets: &[u64],
    entries: &[[u8; 32]],
) -> io::Result<()> {
    debug_assert_eq!(offsets.len(), entries.len());
    if offsets.is_empty() {
        return Ok(());
    }
    let deleted: Vec<[u8; 32]> = entries
        .iter()
        .map(|e| {
            let mut e = *e;
            e[0] &= !EXFAT_ENTRY_IN_USE;
            e
        })
        .collect();
    exfat_write_entry_slabs_at(vol, part_start, part_bytes, offsets, &deleted)
}

/// Find `slots_needed` consecutive unused slots in `dir`, counting the slots in `skip` (the entry
/// being renamed) as used; returns the index of the first. The scan stays within the directory's
/// own clusters, and a run may continue from one cluster into the next in the chain (#128). Slots
/// are read a chunk at a time through [`ExfatSlotReader`].
fn exfat_find_free_entry_run(
    dir: &ExfatDirSlots,
    slots_needed: usize,
    skip: std::ops::Range<u64>,
    read_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<u64> {
    let mut slots = ExfatSlotReader::new(dir, read_at);
    let mut run_start = 0;
    let mut run_len = 0usize;
    for slot in 0..dir.slot_count() {
        if !skip.contains(&slot) && slots.slot(slot)?[0] & EXFAT_ENTRY_IN_USE == 0 {
            if run_len == 0 {
                run_start = slot;
            }
            run_len += 1;
            if run_len >= slots_needed {
                return Ok(run_start);
            }
        } else {
            run_len = 0;
        }
    }
    Err(io::Error::new(
        io::ErrorKind::StorageFull,
        "exFAT folder has no room for this entry: no run of free slots long enough",
    ))
}

/// Normalized path relative to cart root: no leading slash, `/` separators, non-empty.
fn cart_rel_path_trimmed(path: &str) -> Option<String> {
    let s = path.trim().replace('\\', "/");
    let normalized = s.trim_start_matches('/').trim_end_matches('/');
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

/// exFAT delete: **remount the volume for each delete** (depth-first tree walk).
///
/// hadris exFAT is explicitly WIP. A single long-lived [`ExFatFs`] kept the allocation bitmap
/// and other state in memory across many mutations; on real SD over SC64 that diverged from what
/// was on the card (files “still there”, volume corruption). FAT32 uses one [`fatfs::FileSystem`]
/// and behaves correctly; exFAT instead reloads metadata from the SD after each committed delete.
fn remove_cart_path_exfat_unified(
    vol: ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    path: &str,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let normalized = cart_rel_path_trimmed(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty cart path"))?;
    trace(&format!(
        "exFAT: delete tree root normalized={normalized:?}"
    ));
    delete_exfat_tree_remounted(vol, part_start, part_bytes, &normalized, trace)
}

fn delete_exfat_leaf_remounted(
    vol: ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    path: &str,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    trace(&format!("exFAT: leaf mount+delete path={path:?}"));
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    trace("exFAT: ExFatFs::open…");
    let fs = ExFatFs::open(disk).map_err(|e| {
        trace(&format!("exFAT: ExFatFs::open FAILED: {e}"));
        exfat_err(e)
    })?;
    trace("exFAT: ExFatFs::open OK");
    trace(&format!("exFAT: resolve({path:?})…"));
    let entry =
        exfat_resolve_entry_vol(&vol, part_start, part_bytes, fs.info(), path).map_err(|e| {
            trace(&format!("exFAT: resolve FAILED: {e}"));
            e
        })?;
    trace("exFAT: resolve OK");
    trace("exFAT: delete (volume-resolved)…");
    exfat_delete_entry_resolved(&fs, path, &entry, &vol, part_start, part_bytes, trace)?;
    trace("exFAT: delete OK");
    drop(fs);
    trace("exFAT: USB flush_serial…");
    vol.flush_serial().map_err(|e| {
        trace(&format!("exFAT: flush_serial FAILED: {e}"));
        e
    })?;
    trace("exFAT: flush_serial OK");
    Ok(())
}

fn delete_exfat_tree_remounted(
    vol: ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    path: &str,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    trace(&format!("exFAT: tree step mount path={path:?}"));
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    let fs = ExFatFs::open(disk).map_err(|e| {
        trace(&format!("exFAT: ExFatFs::open FAILED: {e}"));
        exfat_err(e)
    })?;
    let entry =
        exfat_resolve_entry_vol(&vol, part_start, part_bytes, fs.info(), path).map_err(|e| {
            trace(&format!("exFAT: resolve FAILED: {e}"));
            e
        })?;

    if entry.is_dir() {
        trace(&format!("exFAT: node is directory path={path:?}"));
        let mut children: Vec<String> = Vec::new();
        if entry.first_cluster >= 2 {
            let mut read_at = exfat_volume_reader(&vol, part_start, part_bytes);
            let slots = ExfatDirSlots::load_with(
                &mut read_at,
                fs.info(),
                entry.first_cluster,
                entry.no_fat_chain,
                entry.data_length,
            )?;
            for ch in exfat_list_entries(&slots, &mut read_at)? {
                if ch.name == "." || ch.name == ".." {
                    continue;
                }
                children.push(ch.name);
            }
        }
        trace(&format!(
            "exFAT: directory children count={} names={children:?}",
            children.len()
        ));
        drop(fs);

        for child_name in children {
            let child_path = format!("{path}/{child_name}");
            delete_exfat_tree_remounted(vol.clone(), part_start, part_bytes, &child_path, trace)?;
        }

        delete_exfat_leaf_remounted(vol, part_start, part_bytes, path, trace)
    } else {
        trace(&format!(
            "exFAT: node is file path={path:?} (delete in-place)"
        ));
        exfat_delete_entry_resolved(&fs, path, &entry, &vol, part_start, part_bytes, trace)?;
        drop(fs);
        trace("exFAT: USB flush_serial…");
        vol.flush_serial().map_err(|e| {
            trace(&format!("exFAT: flush_serial FAILED: {e}"));
            e
        })?;
        trace("exFAT: flush_serial OK");
        Ok(())
    }
}

/// Single FAT mount: [`fatfs::Dir::remove`] with paths relative to volume root (case-insensitive per component).
fn remove_cart_path_fat_unified(
    link: Arc<Mutex<Sc64Link>>,
    part_start: u64,
    part_bytes: u64,
    path: &str,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let normalized = cart_rel_path_trimmed(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty cart path"))?;
    trace(&format!("FAT: delete tree normalized={normalized:?}"));
    let link_flush = link.clone();
    trace("FAT: fatfs mount (writable)…");
    let disk = Sc64PartitionDisk::new_writable(link, part_start, part_bytes);
    let fs = FileSystem::new(disk, FsOptions::new()).map_err(|e| {
        trace(&format!("FAT: FileSystem::new FAILED: {e}"));
        e
    })?;
    trace("FAT: mount OK");
    delete_fat_tree_recursive(&fs, &normalized, trace)?;
    trace("FAT: unmount…");
    fs.unmount().map_err(|e| {
        trace(&format!("FAT: unmount FAILED: {e}"));
        e
    })?;
    trace("FAT: unmount OK");
    trace("FAT: USB flush_serial…");
    flush_link_serial(&link_flush).map_err(|e| {
        trace(&format!("FAT: flush_serial FAILED: {e}"));
        e
    })?;
    trace("FAT: flush_serial OK");
    Ok(())
}

fn delete_fat_tree_recursive(
    fs: &FileSystem<Sc64PartitionDisk>,
    rel: &str,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let root = fs.root_dir();
    if let Ok(dir) = root.open_dir(rel) {
        trace(&format!("FAT: recurse into directory {rel:?}"));
        let mut names: Vec<String> = Vec::new();
        for r in dir.iter() {
            let e = r?;
            let n = e.file_name();
            if n == "." || n == ".." {
                continue;
            }
            names.push(n);
        }
        trace(&format!("FAT: dir {rel:?} children count={}", names.len()));
        drop(dir);
        for n in names {
            let child = format!("{rel}/{n}");
            delete_fat_tree_recursive(fs, &child, trace)?;
        }
    }
    trace(&format!("FAT: root_dir.remove({rel:?})…"));
    fs.root_dir().remove(rel).map_err(|e| {
        trace(&format!("FAT: remove FAILED: {e}"));
        e
    })?;
    trace(&format!("FAT: removed {rel:?} OK"));
    Ok(())
}

/// Read a whole FAT file. Tests only: the app streams instead ([`read_file_fat_streaming`]).
#[cfg(test)]
fn read_file_fat<D: Read + Write + Seek>(disk: D, path: &str) -> io::Result<Vec<u8>> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let (name, parents) = parts.split_last().unwrap();
    let mut dir = fs.root_dir();
    for p in parents {
        dir = dir.open_dir(p)?;
    }
    let mut file = dir.open_file(name)?;
    let mut v = Vec::new();
    file.read_to_end(&mut v)?;
    Ok(v)
}

/// One file's bytes, both found and read through this crate's own chain walk.
///
/// hadris's `open_file` resolves through the lookup that cannot see past a chained root's first
/// cluster, and its `ExFatFileReader` needs an `ExFatFileEntry`, which cannot be built outside
/// hadris — `exfat::entry` is a private module and `parse_entry_set` is not re-exported. So a file
/// whose entry set lives in a later root cluster was listed but could not be opened (#190).
///
/// The copy stops at the file's `valid_data_length`, so the slack at the end of its last cluster is
/// never written out as content, and at `max_bytes` when the caller sets one.
fn exfat_read_file_with<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    read_at: &mut R,
    info: &hadris_fat::exfat::ExFatInfo,
    path: &str,
    mut out: impl Write,
    progress: &mut impl FnMut(u64) -> bool,
    max_bytes: Option<u64>,
) -> io::Result<()> {
    let entry = exfat_resolve_entry(read_at, info, path)?;
    if entry.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "that cart path is a folder, not a file",
        ));
    }
    let limit = match max_bytes {
        Some(m) => m.min(entry.valid_data_length),
        None => entry.valid_data_length,
    };
    // An empty file has no first cluster to walk.
    if limit == 0 || entry.first_cluster < 2 {
        return Ok(());
    }
    // A file's allocation is a cluster chain like a directory's, so the same walk maps it to
    // volume byte offsets, contiguous or FAT-chained.
    let chain = ExfatDirSlots::load_with(
        read_at,
        info,
        entry.first_cluster,
        entry.no_fat_chain,
        entry.data_length.max(entry.valid_data_length),
    )?;
    let cluster_bytes = info.bytes_per_cluster as u64;
    let mut buf = vec![0u8; cluster_bytes.min(STREAM_CHUNK as u64) as usize];
    let mut copied = 0u64;
    for &base in &chain.cluster_offsets {
        if copied >= limit {
            break;
        }
        let mut within = 0u64;
        while within < cluster_bytes && copied < limit {
            let n = buf
                .len()
                .min((limit - copied) as usize)
                .min((cluster_bytes - within) as usize);
            read_at(base + within, &mut buf[..n])?;
            out.write_all(&buf[..n])?;
            within += n as u64;
            copied += n as u64;
            if !progress(n as u64) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
    }
    Ok(())
}

/// Read a whole exFAT file. Tests only: the app streams instead ([`read_file_exfat_streaming`]).
#[cfg(test)]
fn read_file_exfat<D: PartitionDisk>(mut disk: D, path: &str) -> io::Result<Vec<u8>> {
    let info = exfat_info_from_disk(&mut disk)?;
    let mut read_at = exfat_disk_reader(&mut disk);
    let mut v = Vec::new();
    exfat_read_file_with(&mut read_at, &info, path, &mut v, &mut |_| true, None)?;
    Ok(v)
}

/// `max_bytes`: stop after this many bytes (directory entry size). Passing the logical size avoids
/// reading past EOF when the underlying reader would otherwise return cluster slack (wrong PC file size).
fn read_file_fat_streaming<D: Read + Write + Seek>(
    disk: D,
    path: &str,
    mut out: impl Write,
    progress: &mut impl FnMut(u64) -> bool,
    max_bytes: Option<u64>,
) -> io::Result<()> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let (name, parents) = parts.split_last().unwrap();
    let mut dir = fs.root_dir();
    for p in parents {
        dir = dir.open_dir(p)?;
    }
    let mut file = dir.open_file(name)?;
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut copied = 0u64;
    loop {
        if let Some(max) = max_bytes {
            if copied >= max {
                break;
            }
        }
        let cap = match max_bytes {
            None => buf.len(),
            Some(max) => buf.len().min((max - copied) as usize),
        };
        if cap == 0 {
            break;
        }
        let n = file.read(&mut buf[..cap])?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        copied += n as u64;
        if !progress(n as u64) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
    }
    Ok(())
}

fn read_file_exfat_streaming<D: PartitionDisk>(
    mut disk: D,
    path: &str,
    out: impl Write,
    progress: &mut impl FnMut(u64) -> bool,
    max_bytes: Option<u64>,
) -> io::Result<()> {
    let info = exfat_info_from_disk(&mut disk)?;
    let mut read_at = exfat_disk_reader(&mut disk);
    exfat_read_file_with(&mut read_at, &info, path, out, progress, max_bytes)
}

fn list_dir_fat<D: Read + Write + Seek>(disk: D, path: &str) -> io::Result<Vec<SessionEntry>> {
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let root = fs.root_dir();
    let dir = if trimmed.is_empty() {
        root
    } else {
        root.open_dir(trimmed)?
    };
    let mut out = Vec::new();
    let prefix = if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    };
    for r in dir.iter() {
        let e = r?;
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = e.is_dir();
        let size = if is_dir { 0 } else { e.len() };
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}{name}")
        };
        let hidden = e.attributes().contains(FatFileAttributes::HIDDEN) || name.starts_with('.');
        out.push(SessionEntry {
            name,
            path,
            is_dir,
            size,
            hidden,
        });
    }
    sort_session_entries(&mut out);
    Ok(out)
}

/// One in-use exFAT entry set, decoded from raw slots.
///
/// hadris's own `parse_entry_set` cannot be used: `exfat::entry` is a private module and the
/// re-export list omits it. The layout is exFAT's own — attributes at File entry byte 4, and
/// NameLength (3), ValidDataLength (8), FirstCluster (20) and DataLength (24) in the Stream
/// Extension, which the File Name entries follow.
struct ExfatDecodedEntry {
    name: String,
    attributes: u16,
    first_cluster: u32,
    data_length: u64,
    valid_data_length: u64,
    no_fat_chain: bool,
    /// The File entry slot exactly as it is on disk. A rename rewrites the entry set, and copying
    /// this keeps the timestamps, attributes and reserved bytes byte-identical rather than
    /// rebuilding them from parsed values (#190).
    primary: [u8; 32],
}

impl ExfatDecodedEntry {
    const ATTR_HIDDEN: u16 = 0x02;
    const ATTR_DIRECTORY: u16 = 0x10;

    fn is_dir(&self) -> bool {
        self.attributes & Self::ATTR_DIRECTORY != 0
    }

    fn is_hidden(&self) -> bool {
        self.attributes & Self::ATTR_HIDDEN != 0
    }
}

/// Decodes the entry set whose File entry is at `primary_slot`, with the number of secondary slots
/// it occupies. `None` when the slots there are not a well-formed set.
fn exfat_decode_entry_set<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    slots: &mut ExfatSlotReader<'_, R>,
    primary_slot: u64,
    slot_count: u64,
) -> io::Result<Option<(ExfatDecodedEntry, u64)>> {
    let primary = slots.slot(primary_slot)?;
    if primary[0] != EXFAT_ENTRY_FILE_DIRECTORY {
        return Ok(None);
    }
    let secondaries = u64::from(primary[1]);
    if !(2..=18).contains(&secondaries) || primary_slot + secondaries >= slot_count {
        return Ok(None);
    }
    let stream = slots.slot(primary_slot + 1)?;
    if stream[0] != EXFAT_ENTRY_STREAM_EXT {
        return Ok(None);
    }
    let name_len = usize::from(stream[3]);
    let mut units: Vec<u16> = Vec::with_capacity(name_len);
    for s in primary_slot + 2..=primary_slot + secondaries {
        if units.len() >= name_len {
            break;
        }
        let entry = slots.slot(s)?;
        if entry[0] != EXFAT_ENTRY_FILE_NAME {
            return Ok(None);
        }
        for unit in entry[2..].chunks_exact(2) {
            if units.len() >= name_len {
                break;
            }
            units.push(u16::from_le_bytes([unit[0], unit[1]]));
        }
    }
    if units.len() != name_len {
        return Ok(None);
    }
    let u32_at = |s: &[u8; 32], at: usize| u32::from_le_bytes(s[at..at + 4].try_into().unwrap());
    let u64_at = |s: &[u8; 32], at: usize| u64::from_le_bytes(s[at..at + 8].try_into().unwrap());
    Ok(Some((
        ExfatDecodedEntry {
            name: String::from_utf16_lossy(&units),
            attributes: u16::from_le_bytes([primary[4], primary[5]]),
            first_cluster: u32_at(&stream, 20),
            data_length: u64_at(&stream, 24),
            valid_data_length: u64_at(&stream, 8),
            // Stream Extension general secondary flags, bit 1: NoFatChain.
            no_fat_chain: stream[1] & 0x02 != 0,
            primary,
        },
        secondaries,
    )))
}

/// Every in-use entry set in `dir`, following the directory's real cluster chain (#189).
fn exfat_list_entries<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    dir: &ExfatDirSlots,
    read_at: &mut R,
) -> io::Result<Vec<ExfatDecodedEntry>> {
    const END_OF_DIRECTORY: u8 = 0x00;
    let count = dir.slot_count();
    let mut out = Vec::new();
    let mut slots = ExfatSlotReader::new(dir, &mut *read_at);
    let mut i = 0;
    while i < count {
        let primary = slots.slot(i)?;
        if primary[0] == END_OF_DIRECTORY {
            break;
        }
        // A freed set has the in-use bit clear (#127); skip it without ending the directory.
        if primary[0] & EXFAT_ENTRY_IN_USE == 0 {
            i += 1;
            continue;
        }
        match exfat_decode_entry_set(&mut slots, i, count)? {
            Some((entry, secondaries)) => {
                out.push(entry);
                i += 1 + secondaries;
            }
            None => i += 1,
        }
    }
    Ok(out)
}

/// The volume's exFAT parameters, read straight from its boot sector.
///
/// Listing cannot take these from `ExFatFs::open`, which consumes the disk. Every field of
/// `ExFatInfo` is public, so they are simply read here.
fn exfat_info_from_disk(disk: &mut impl PartitionDisk) -> io::Result<hadris_fat::exfat::ExFatInfo> {
    let invalid = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let mut bs = [0u8; 512];
    disk.seek(SeekFrom::Start(0))?;
    disk.read_exact(&mut bs)?;
    if &bs[3..11] != b"EXFAT   " {
        return Err(invalid("not an exFAT boot sector"));
    }
    let u32_at = |at: usize| u32::from_le_bytes(bs[at..at + 4].try_into().unwrap());
    if bs[108] < 9 || bs[108] > 12 || bs[109] > 25 {
        return Err(invalid("exFAT boot sector has implausible geometry"));
    }
    let bytes_per_sector = 1usize << bs[108];
    let sectors_per_cluster = 1usize << bs[109];
    Ok(hadris_fat::exfat::ExFatInfo {
        bytes_per_sector,
        sectors_per_cluster,
        bytes_per_cluster: bytes_per_sector * sectors_per_cluster,
        fat_offset: u64::from(u32_at(80)) * bytes_per_sector as u64,
        fat_length: u64::from(u32_at(84)) * bytes_per_sector as u64,
        cluster_heap_offset: u64::from(u32_at(88)) * bytes_per_sector as u64,
        cluster_count: u32_at(92),
        root_cluster: u32_at(96),
        volume_serial: u32_at(100),
        fat_count: bs[110],
    })
}

/// Slot map of the directory at `path`, resolved segment by segment through this crate's own
/// chain-following reader.
///
/// The root is loaded as a FAT chain with no recorded size. hadris instead pins the root
/// contiguous (`root_contiguous = true`, `root_size = 0`), so its iterator steps to the
/// *physically* next cluster when the first is exhausted and stops at the first zero byte it finds
/// there — which on a real card listed 74 of 1273 entries and made the rest unopenable (#189).
fn exfat_resolve_dir_slots<R: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    read_at: &mut R,
    info: &hadris_fat::exfat::ExFatInfo,
    path: &str,
) -> io::Result<ExfatDirSlots> {
    let mut dir = ExfatDirSlots::load_with(read_at, info, info.root_cluster, false, 0)?;
    let trimmed = path.trim().replace('\\', "/");
    for seg in trimmed.split('/').filter(|s| !s.is_empty()) {
        let found = exfat_list_entries(&dir, read_at)?
            .into_iter()
            .find(|e| fat_names_equal(&e.name, seg))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no such folder: {seg}"))
            })?;
        if !found.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a folder: {seg}"),
            ));
        }
        dir = ExfatDirSlots::load_with(
            read_at,
            info,
            found.first_cluster,
            found.no_fat_chain,
            found.data_length,
        )?;
    }
    Ok(dir)
}

fn list_dir_exfat<D: PartitionDisk>(mut disk: D, path: &str) -> io::Result<Vec<SessionEntry>> {
    let info = exfat_info_from_disk(&mut disk)?;
    let mut read_at = exfat_disk_reader(&mut disk);
    let dir = exfat_resolve_dir_slots(&mut read_at, &info, path)?;
    let normalized = path.trim().replace('\\', "/");
    let segs: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    let path_prefix = if segs.is_empty() {
        String::new()
    } else {
        format!("{}/", segs.join("/"))
    };
    let mut out = Vec::new();
    for e in exfat_list_entries(&dir, &mut read_at)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        let is_dir = e.is_dir();
        let hidden = e.is_hidden() || e.name.starts_with('.');
        out.push(SessionEntry {
            path: format!("{path_prefix}{}", e.name),
            name: e.name,
            is_dir,
            size: if is_dir { 0 } else { e.valid_data_length },
            hidden,
        });
    }
    sort_session_entries(&mut out);
    Ok(out)
}

fn sort_session_entries(out: &mut Vec<SessionEntry>) {
    if out.len() <= 1 {
        return;
    }
    let mut decorated: Vec<(bool, String, SessionEntry)> = out
        .drain(..)
        .map(|e| {
            let nl = e.name.to_lowercase();
            let is_dir = e.is_dir;
            (is_dir, nl, e)
        })
        .collect();
    decorated.sort_by(|a, b| match (a.0, b.0) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.1.cmp(&b.1),
    });
    *out = decorated.into_iter().map(|(_, _, e)| e).collect();
}

/// Start LBA of the MBR's first partition entry, or `0` when that entry is not a real one: its
/// boot flag must be `0x00` or `0x80`, and its type and start LBA non-zero.
fn legacy_mbr_first_partition_lba(sector0: &[u8]) -> u64 {
    if sector0.len() < 512 {
        return 0;
    }
    let boot_flag = sector0[0x1BE];
    let part_type = sector0[0x1C2];
    let lba = u32::from_le_bytes([
        sector0[0x1C6],
        sector0[0x1C7],
        sector0[0x1C8],
        sector0[0x1C9],
    ]);
    if (boot_flag == 0x00 || boot_flag == 0x80) && part_type != 0 && lba != 0 {
        u64::from(lba)
    } else {
        0
    }
}

/// A FAT12/16/32 boot sector with a plausible BPB: what sector 0 holds on a card formatted without
/// a partition table. An MBR has boot code where the BPB would be.
fn is_fat_boot_sector(s: &[u8]) -> bool {
    if s.len() < 512 {
        return false;
    }
    let u16_at = |o: usize| u16::from_le_bytes([s[o], s[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]);
    let jump = (s[0] == 0xEB && s[2] == 0x90) || s[0] == 0xE9;
    let media = s[0x15];
    jump && matches!(u16_at(0x0B), 512 | 1024 | 2048 | 4096)
        && s[0x0D].is_power_of_two()
        && u16_at(0x0E) != 0
        && (s[0x10] == 1 || s[0x10] == 2)
        && (media == 0xF0 || media >= 0xF8)
        && (u16_at(0x13) != 0 || u32_at(0x20) != 0)
        && (u16_at(0x16) != 0 || u32_at(0x24) != 0)
}

/// First data partition LBA: **legacy MBR** (first entry) or **GPT** (protective MBR `0xEE` → read
/// EFI header + first partition entry). Super-floppy / no partition table returns `0`.
fn detect_partition_start<F>(sector0: &[u8], mut read_sector: F) -> io::Result<u64>
where
    F: FnMut(u64, &mut [u8; 512]) -> io::Result<()>,
{
    if sector0.len() < 512 {
        return Ok(0);
    }
    // A volume boot sector also ends in `55 AA`, and its boot code sits where an MBR keeps its
    // partition entries: reading those bytes as an LBA sent `open` far past the card (#130).
    if is_exfat_boot_sector(sector0) || is_fat_boot_sector(sector0) {
        return Ok(0);
    }
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Ok(0);
    }
    let part_type = sector0[0x1C2];
    if part_type == 0xEE {
        let mut gpt_hdr = [0u8; 512];
        read_sector(1, &mut gpt_hdr)?;
        if &gpt_hdr[0..8] != b"EFI PART" {
            return Ok(legacy_mbr_first_partition_lba(sector0));
        }
        let entries_lba = u64::from_le_bytes([
            gpt_hdr[0x48],
            gpt_hdr[0x49],
            gpt_hdr[0x4A],
            gpt_hdr[0x4B],
            gpt_hdr[0x4C],
            gpt_hdr[0x4D],
            gpt_hdr[0x4E],
            gpt_hdr[0x4F],
        ]);
        let mut pe = [0u8; 512];
        read_sector(entries_lba, &mut pe)?;
        let start_lba = u64::from_le_bytes([
            pe[0x20], pe[0x21], pe[0x22], pe[0x23], pe[0x24], pe[0x25], pe[0x26], pe[0x27],
        ]);
        if start_lba == 0 {
            return Ok(legacy_mbr_first_partition_lba(sector0));
        }
        return Ok(start_lba);
    }
    Ok(legacy_mbr_first_partition_lba(sector0))
}

/// exFAT main boot sector uses "EXFAT   " at 0x03 and zeros 0x0B–0x3F, so classic FAT BPB
/// `bytes_per_sector` at 0x0B reads as 0.
fn is_exfat_boot_sector(sector: &[u8]) -> bool {
    sector.len() >= 11 && &sector[3..11] == b"EXFAT   "
}

fn partition_volume_bytes(bpb: &[u8]) -> io::Result<u64> {
    if bpb.len() < 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid boot sector",
        ));
    }
    if is_exfat_boot_sector(bpb) {
        return exfat_partition_byte_length(bpb);
    }
    fat_partition_byte_length(bpb)
}

fn exfat_partition_byte_length(sector: &[u8]) -> io::Result<u64> {
    if sector.len() < 0x6E {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid exFAT boot sector",
        ));
    }
    let bps_shift = sector[0x6C];
    if !(9..=12).contains(&bps_shift) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid exFAT bytes_per_sector_shift",
        ));
    }
    let bytes_per_sector = 1u64 << bps_shift;
    let volume_length = u64::from_le_bytes([
        sector[0x48],
        sector[0x49],
        sector[0x4A],
        sector[0x4B],
        sector[0x4C],
        sector[0x4D],
        sector[0x4E],
        sector[0x4F],
    ]);
    volume_length
        .checked_mul(bytes_per_sector)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "exFAT volume size overflow"))
}

fn fat_partition_byte_length(bpb: &[u8]) -> io::Result<u64> {
    let bps = u16::from_le_bytes([bpb[0x0B], bpb[0x0C]]) as u64;
    if bps == 0 || bps % 512 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid bytes per sector in BPB (expected FAT12/16/32 volume; wrong partition offset?)",
        ));
    }
    let t16 = u16::from_le_bytes([bpb[0x13], bpb[0x14]]) as u32;
    let t32 = u32::from_le_bytes([bpb[0x20], bpb[0x21], bpb[0x22], bpb[0x23]]);
    let total = if t16 != 0 { t16 as u64 } else { t32 as u64 };
    Ok(total * bps)
}

pub(crate) struct SectorPartitionDisk<T: SdCardTransport> {
    link: Arc<Mutex<T>>,
    partition_start: u64,
    partition_bytes: u64,
    pos: u64,
    writable: bool,
}

pub(crate) type Sc64PartitionDisk = SectorPartitionDisk<Sc64Link>;

impl<T: SdCardTransport> SectorPartitionDisk<T> {
    fn new(link: Arc<Mutex<T>>, partition_start: u64, partition_bytes: u64) -> Self {
        Self {
            link,
            partition_start,
            partition_bytes,
            pos: 0,
            writable: false,
        }
    }

    fn new_writable(link: Arc<Mutex<T>>, partition_start: u64, partition_bytes: u64) -> Self {
        Self {
            link,
            partition_start,
            partition_bytes,
            pos: 0,
            writable: true,
        }
    }
}

impl<T: SdCardTransport> Read for SectorPartitionDisk<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let start = self.pos;
        if start >= self.partition_bytes {
            return Ok(0);
        }
        let max = (self.partition_bytes - start) as usize;
        let want = buf.len().min(max);
        let mut got = 0usize;
        let max_sectors_batch = SD_CARD_BUFFER_MAX_BYTES / 512;
        while got < want {
            let abs_byte = start + got as u64;
            let abs_sec = self.partition_start + abs_byte / 512;
            let off = (abs_byte % 512) as usize;
            let mut link = self
                .link
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?;

            if off != 0 {
                let mut sector = [0u8; 512];
                link.read_sd_sectors(abs_sec, &mut sector)?;
                let take = (512 - off).min(want - got);
                buf[got..got + take].copy_from_slice(&sector[off..off + take]);
                got += take;
                drop(link);
                continue;
            }

            let remaining = want - got;
            let bytes_left_in_partition = (self.partition_bytes - (start + got as u64)) as usize;
            let contiguous = remaining.min(bytes_left_in_partition);

            if contiguous < 512 {
                let mut sector = [0u8; 512];
                link.read_sd_sectors(abs_sec, &mut sector)?;
                buf[got..got + contiguous].copy_from_slice(&sector[0..contiguous]);
                got += contiguous;
                drop(link);
                continue;
            }

            let contiguous_sectors = contiguous / 512;
            let batch_sectors = contiguous_sectors.min(max_sectors_batch);
            let batch_bytes = batch_sectors * 512;
            link.read_sd_sectors(abs_sec, &mut buf[got..got + batch_bytes])?;
            got += batch_bytes;
        }
        self.pos = start + got as u64;
        Ok(got)
    }
}

impl<T: SdCardTransport> Write for SectorPartitionDisk<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "read-only cart SD session",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let start = self.pos;
        let mut written = 0usize;
        let max_sectors_batch = SD_CARD_BUFFER_MAX_BYTES / 512;
        while written < buf.len() {
            let abs = start + written as u64;
            if abs >= self.partition_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write past end of partition",
                ));
            }
            let abs_sec = self.partition_start + abs / 512;
            let off = (abs % 512) as usize;
            let mut link = self
                .link
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?;

            if off != 0 {
                let mut sector = [0u8; 512];
                link.read_sd_sectors(abs_sec, &mut sector)?;
                let take = (512 - off).min(buf.len() - written);
                sector[off..off + take].copy_from_slice(&buf[written..written + take]);
                link.write_sd_sectors(abs_sec, &sector)?;
                written += take;
                drop(link);
                continue;
            }

            let remaining = buf.len() - written;
            let bytes_left_in_partition = (self.partition_bytes - abs) as usize;
            let contiguous = remaining.min(bytes_left_in_partition);

            if contiguous < 512 {
                let mut sector = [0u8; 512];
                link.read_sd_sectors(abs_sec, &mut sector)?;
                sector[0..contiguous].copy_from_slice(&buf[written..written + contiguous]);
                link.write_sd_sectors(abs_sec, &sector)?;
                written += contiguous;
                drop(link);
                continue;
            }

            let contiguous_sectors = contiguous / 512;
            let batch_sectors = contiguous_sectors.min(max_sectors_batch);
            let batch_bytes = batch_sectors * 512;
            link.write_sd_sectors(abs_sec, &buf[written..written + batch_bytes])?;
            written += batch_bytes;
        }
        self.pos = start + written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut g = self
            .link
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        g.flush_serial()
    }
}

impl<T: SdCardTransport> Seek for SectorPartitionDisk<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(s) => s as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.partition_bytes as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        let n = new_pos as u64;
        if n > self.partition_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek past end of partition",
            ));
        }
        self.pos = n;
        Ok(self.pos)
    }
}

impl<T: SdCardTransport> PartitionDisk for SectorPartitionDisk<T> {
    fn partition_byte_len(&self) -> u64 {
        self.partition_bytes
    }
}

/// SC64, EverDrive (linear `RomRead`), or RAM backing for hadris [`ExFatFs`].
pub(crate) enum PartitionDiskUnion {
    Sc64(Sc64PartitionDisk),
    #[cfg(feature = "ed64")]
    Ed64(SectorPartitionDisk<Ed64RomLinear>),
    Ram(RamPartitionDisk),
    /// A RAM image behind [`SdReadCache`](crate::sd_read_cache::SdReadCache), so tests can run
    /// real exFAT operations through the session cache (#196).
    #[cfg(test)]
    CachedRam(SectorPartitionDisk<CachedRamTransport>),
}

impl Read for PartitionDiskUnion {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Sc64(d) => d.read(buf),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.read(buf),
            Self::Ram(d) => d.read(buf),
            #[cfg(test)]
            Self::CachedRam(d) => d.read(buf),
        }
    }
}

impl Write for PartitionDiskUnion {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Sc64(d) => d.write(buf),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.write(buf),
            Self::Ram(d) => d.write(buf),
            #[cfg(test)]
            Self::CachedRam(d) => d.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Sc64(d) => d.flush(),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.flush(),
            Self::Ram(d) => d.flush(),
            #[cfg(test)]
            Self::CachedRam(d) => d.flush(),
        }
    }
}

impl Seek for PartitionDiskUnion {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Sc64(d) => d.seek(pos),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.seek(pos),
            Self::Ram(d) => d.seek(pos),
            #[cfg(test)]
            Self::CachedRam(d) => d.seek(pos),
        }
    }
}

impl PartitionDisk for PartitionDiskUnion {
    fn partition_byte_len(&self) -> u64 {
        match self {
            Self::Sc64(d) => d.partition_byte_len(),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.partition_byte_len(),
            Self::Ram(d) => d.partition_byte_len(),
            #[cfg(test)]
            Self::CachedRam(d) => d.partition_byte_len(),
        }
    }
}

/// exFAT metadata helpers need either SC64 serial access or a RAM test image.
#[derive(Clone)]
pub(crate) enum ExfatVolumeSource {
    Sc64 {
        link: Arc<Mutex<Sc64Link>>,
    },
    /// Used by RAM-disk tests (`write_file_exfat_streaming` and friends).
    #[cfg_attr(not(test), allow(dead_code))]
    Ram {
        buf: Arc<Mutex<Vec<u8>>>,
    },
    /// The same RAM image reached through [`SdReadCache`](crate::sd_read_cache::SdReadCache),
    /// sector by sector, as `Sc64Link` reaches a card (#196).
    #[cfg(test)]
    CachedRam {
        link: Arc<Mutex<CachedRamTransport>>,
    },
}

/// A RAM image served as SD sectors through the session read cache, exactly as `Sc64Link` serves
/// the card: every read through [`SdReadCache::read_through`](crate::sd_read_cache::SdReadCache),
/// every write through `write_through`. It lets the exFAT paths run end to end against the cache,
/// where a missed invalidation shows up as a wrong image rather than as a unit-test assertion.
#[cfg(test)]
pub(crate) struct CachedRamTransport {
    pub(crate) image: Arc<Mutex<Vec<u8>>>,
    pub(crate) cache: crate::sd_read_cache::SdReadCache,
    /// Read requests issued by the filesystem layer, cached or not.
    pub(crate) requests: u32,
    /// Fails a sector write, as a USB timeout would, when it returns true for the write's first
    /// LBA. Nothing reaches the image or the cache for a failed write.
    pub(crate) fail_write: Option<Box<dyn FnMut(u64) -> bool + Send>>,
}

#[cfg(test)]
impl SdCardTransport for CachedRamTransport {
    fn read_sd_sectors(&mut self, start_lba: u64, buf: &mut [u8]) -> io::Result<()> {
        self.requests += 1;
        let image = &self.image;
        self.cache.read_through(start_lba, buf, |lba, buf| {
            let g = image.lock().map_err(|e| io::Error::other(e.to_string()))?;
            let at = lba as usize * 512;
            let src = g
                .get(at..at + buf.len())
                .ok_or_else(|| io::Error::other("read past the image"))?;
            buf.copy_from_slice(src);
            Ok(())
        })
    }

    fn write_sd_sectors(&mut self, start_lba: u64, buf: &[u8]) -> io::Result<()> {
        if self.fail_write.as_mut().is_some_and(|fail| fail(start_lba)) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "injected write failure",
            ));
        }
        let image = &self.image;
        self.cache.write_through(start_lba, buf, |lba, buf| {
            let mut g = image.lock().map_err(|e| io::Error::other(e.to_string()))?;
            let at = lba as usize * 512;
            let dst = g
                .get_mut(at..at + buf.len())
                .ok_or_else(|| io::Error::other("write past the image"))?;
            dst.copy_from_slice(buf);
            Ok(())
        })
    }

    fn flush_serial(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ExfatVolumeSource {
    fn partition_disk_ro(&self, part_start: u64, part_bytes: u64) -> PartitionDiskUnion {
        match self {
            Self::Sc64 { link } => PartitionDiskUnion::Sc64(Sc64PartitionDisk::new(
                link.clone(),
                part_start,
                part_bytes,
            )),
            Self::Ram { buf } => {
                debug_assert_eq!(buf.lock().map(|g| g.len() as u64).unwrap_or(0), part_bytes);
                PartitionDiskUnion::Ram(RamPartitionDisk::new_readonly(buf.clone()))
            }
            #[cfg(test)]
            Self::CachedRam { link } => PartitionDiskUnion::CachedRam(SectorPartitionDisk::new(
                link.clone(),
                part_start,
                part_bytes,
            )),
        }
    }

    fn partition_disk_rw(&self, part_start: u64, part_bytes: u64) -> PartitionDiskUnion {
        match self {
            Self::Sc64 { link } => PartitionDiskUnion::Sc64(Sc64PartitionDisk::new_writable(
                link.clone(),
                part_start,
                part_bytes,
            )),
            Self::Ram { buf } => {
                debug_assert_eq!(buf.lock().map(|g| g.len() as u64).unwrap_or(0), part_bytes);
                PartitionDiskUnion::Ram(RamPartitionDisk::new_writable(buf.clone()))
            }
            #[cfg(test)]
            Self::CachedRam { link } => PartitionDiskUnion::CachedRam(
                SectorPartitionDisk::new_writable(link.clone(), part_start, part_bytes),
            ),
        }
    }

    fn flush_serial(&self) -> io::Result<()> {
        match self {
            Self::Sc64 { link } => flush_link_serial(link),
            Self::Ram { .. } => Ok(()),
            #[cfg(test)]
            Self::CachedRam { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cart_entry_name_matches, cart_rel_path_trimmed};

    /// #131: FAT and exFAT fold case with a one-to-one up-case table, not just ASCII.
    #[test]
    fn cart_entry_names_fold_case_beyond_ascii() {
        assert!(cart_entry_name_matches("ä.sav", "Ä.SAV"));
        assert!(cart_entry_name_matches("Ωμέγα.z64", "ΩΜΈΓΑ.Z64"));
        assert!(cart_entry_name_matches("game.z64", "GAME.Z64"));
        assert!(!cart_entry_name_matches("ä.sav", "a.sav"));
        // One code unit never folds to two: the up-case table maps each unit to one unit.
        assert!(!cart_entry_name_matches("straße", "STRASSE"));
    }

    #[test]
    fn cart_rel_path_trims_slashes_and_backslashes() {
        assert_eq!(
            cart_rel_path_trimmed(" /foo/bar/baz ").as_deref(),
            Some("foo/bar/baz")
        );
        assert_eq!(cart_rel_path_trimmed(r"a\b\c").as_deref(), Some("a/b/c"));
    }

    #[test]
    fn cart_rel_path_rejects_empty() {
        assert_eq!(cart_rel_path_trimmed("   "), None);
        assert_eq!(cart_rel_path_trimmed("/"), None);
        assert_eq!(cart_rel_path_trimmed(""), None);
    }
}

/// RAM-backed FAT32 / exFAT integration tests (no SC64 hardware).
#[cfg(test)]
mod fs_tests {
    use super::{
        detect_partition_start, exfat_find_entry_set, exfat_find_free_entry_run,
        exfat_read_entry_set, list_dir_exfat, list_dir_fat, mkdir_cart_exfat, mkdir_fat_impl,
        partition_volume_bytes, read_file_exfat, read_file_fat, remove_cart_path_exfat_unified,
        rename_cart_exfat, rename_fat_impl, write_file_exfat_streaming,
        write_file_fat_streaming_impl, CachedRamTransport, ExfatDirSlots, ExfatEntryIdentity,
        ExfatSlotReader, ExfatVolumeSource,
    };
    use crate::mem_disk::RamPartitionDisk;
    use fatfs::FormatVolumeOptions;
    use hadris_fat::exfat::{format_exfat, ExFatFormatOptions, ExFatFs, ExFatInfo};
    use sha2::{Digest, Sha256};
    use std::cell::RefCell;
    use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const FAT32_IMAGE_BYTES: usize = 8 * 1024 * 1024;
    const EXFAT_IMAGE_BYTES: usize = 4 * 1024 * 1024;

    type Image = Arc<Mutex<Vec<u8>>>;

    fn fat_image() -> Image {
        let arc = Arc::new(Mutex::new(vec![0u8; FAT32_IMAGE_BYTES]));
        let mut d = RamPartitionDisk::new_writable(Arc::clone(&arc));
        fatfs::format_volume(&mut d, FormatVolumeOptions::new()).expect("format FAT");
        arc
    }

    fn exfat_image(opts: &ExFatFormatOptions) -> (Image, ExfatVolumeSource, u64) {
        let arc = Arc::new(Mutex::new(vec![0u8; EXFAT_IMAGE_BYTES]));
        let part_bytes = EXFAT_IMAGE_BYTES as u64;
        format_exfat(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            part_bytes,
            opts,
        )
        .expect("format exFAT");
        let vol = ExfatVolumeSource::Ram {
            buf: Arc::clone(&arc),
        };
        (arc, vol, part_bytes)
    }

    /// A RAM disk whose writes fail wherever `fail(position)` says, as an SC64 USB timeout would.
    struct FlakyDisk<F: FnMut(u64) -> bool> {
        inner: RamPartitionDisk,
        fail: F,
    }

    impl<F: FnMut(u64) -> bool> Read for FlakyDisk<F> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl<F: FnMut(u64) -> bool> Seek for FlakyDisk<F> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    impl<F: FnMut(u64) -> bool> Write for FlakyDisk<F> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let pos = self.inner.stream_position()?;
            if (self.fail)(pos) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "injected write failure",
                ));
            }
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    /// Lets fatfs mark the volume dirty (the first boot-sector write), then fails the write that
    /// marks it clean again on unmount.
    fn fail_the_unmount_write() -> impl FnMut(u64) -> bool {
        let mut boot_sector_writes = 0;
        move |pos| {
            if pos < 512 {
                boot_sector_writes += 1;
            }
            boot_sector_writes >= 2
        }
    }

    /// #129: fatfs writes a file's size into its directory entry on flush; a failure there must
    /// not be reported as a finished copy.
    #[test]
    fn fat_write_reports_a_failed_directory_entry_update() {
        let arc = fat_image();
        let armed = Arc::new(AtomicBool::new(false));
        let disk = FlakyDisk {
            inner: RamPartitionDisk::new_writable(Arc::clone(&arc)),
            fail: {
                let armed = Arc::clone(&armed);
                move |_| armed.load(Ordering::SeqCst)
            },
        };
        let err = write_file_fat_streaming_impl(
            disk,
            "rom.z64",
            &mut Cursor::new(&[7u8; 5000][..]),
            // Every data byte is on the card; only the metadata writes after this fail.
            &mut |_| {
                armed.store(true, Ordering::SeqCst);
                true
            },
        )
        .expect_err("a lost directory-entry write must not report success");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    fn fat_root_names(arc: &Image) -> Vec<String> {
        list_dir_fat(RamPartitionDisk::new_readonly(Arc::clone(arc)), "/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    fn fat_write(
        arc: &Image,
        path: &str,
        body: &[u8],
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> io::Result<()> {
        write_file_fat_streaming_impl(
            RamPartitionDisk::new_writable(Arc::clone(arc)),
            path,
            &mut Cursor::new(body),
            &mut |n| progress(n),
        )
    }

    fn fat_read(arc: &Image, path: &str) -> Vec<u8> {
        read_file_fat(RamPartitionDisk::new_readonly(Arc::clone(arc)), path).unwrap()
    }

    /// #200: cancelling a FAT32 replace leaves the original byte for byte, and leaves no temporary
    /// copy behind.
    ///
    /// Fail-first, demonstrated: restore the delete-before-create order and the original is gone
    /// — `read_file_fat` panics finding it.
    #[test]
    fn fat_cancelled_replace_keeps_the_original() {
        let arc = fat_image();
        let original: Vec<u8> = (0..20_000).map(|i| (i % 241) as u8).collect();
        fat_write(&arc, "game.z64", &original, &mut |_| true).unwrap();
        let before = fat_root_names(&arc);

        let replacement = vec![0xEEu8; 300_000];
        let err = fat_write(&arc, "game.z64", &replacement, &mut |_| false)
            .expect_err("a cancelled replace");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");

        assert!(
            fat_read(&arc, "game.z64") == original,
            "the original changed"
        );
        assert_eq!(
            fat_root_names(&arc),
            before,
            "the folder should be exactly as it was"
        );
    }

    /// A completed FAT32 replace holds the new contents under the original name, once, with no
    /// temporary copy left beside it.
    #[test]
    fn fat_replace_leaves_only_the_new_file() {
        let arc = fat_image();
        fat_write(&arc, "game.z64", &[1u8; 5000], &mut |_| true).unwrap();
        let before = fat_root_names(&arc);

        let replacement: Vec<u8> = (0..70_000).map(|i| (i % 233) as u8).collect();
        fat_write(&arc, "game.z64", &replacement, &mut |_| true).expect("replace");

        assert!(fat_read(&arc, "game.z64") == replacement);
        assert_eq!(
            fat_root_names(&arc),
            before,
            "no temporary file, no duplicate"
        );
    }

    /// A cancelled FAT32 write of a new file leaves nothing, since the session no longer removes
    /// the destination on cancel (#200) — it would be the original on a replace.
    #[test]
    fn fat_cancelled_new_file_leaves_nothing() {
        let arc = fat_image();
        let before = fat_root_names(&arc);
        fat_write(&arc, "new.z64", &[3u8; 100_000], &mut |_| false).expect_err("a cancelled write");
        assert_eq!(fat_root_names(&arc), before);
    }

    /// The temporary name always fits a long file name, and never collides with the name it stands
    /// in for.
    #[test]
    fn fat_replace_temp_name_fits_and_differs() {
        use super::fat_replace_temp_name;
        for name in ["a.z64", &"x".repeat(250), &"é".repeat(255)] {
            let t = fat_replace_temp_name(name);
            assert!(t.encode_utf16().count() <= 255, "{t}");
            assert_ne!(t, name);
        }
    }

    /// Each flaky case needs a fresh image: a failed unmount leaves the volume marked dirty, and
    /// the next mount then has no dirty flag to clear.
    fn flaky_unmount(arc: &Image) -> FlakyDisk<impl FnMut(u64) -> bool> {
        FlakyDisk {
            inner: RamPartitionDisk::new_writable(Arc::clone(arc)),
            fail: fail_the_unmount_write(),
        }
    }

    /// #129: FAT mkdir reports a failure to finish the volume, not just to start.
    #[test]
    fn fat_mkdir_reports_a_failed_unmount() {
        let arc = fat_image();
        mkdir_fat_impl(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            "saves/eep",
        )
        .unwrap();
        assert!(fat_root_names(&arc).contains(&"saves".to_string()));

        let err =
            mkdir_fat_impl(flaky_unmount(&arc), "roms").expect_err("mkdir must report the unmount");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    /// #129: FAT rename reports a failure to finish the volume, not just to start.
    #[test]
    fn fat_rename_reports_a_failed_unmount() {
        let arc = fat_image();
        write_file_fat_streaming_impl(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            "a.txt",
            &mut Cursor::new(&b"a"[..]),
            &mut |_| true,
        )
        .unwrap();
        rename_fat_impl(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            "",
            "a.txt",
            "b.txt",
        )
        .unwrap();
        assert!(fat_root_names(&arc).contains(&"b.txt".to_string()));

        let err = rename_fat_impl(flaky_unmount(&arc), "", "b.txt", "c.txt")
            .expect_err("rename must report the unmount");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    /// Every 32-byte slot of the directory stored in `chain`, read straight from the image.
    fn raw_dir_slots(arc: &Image, info: &ExFatInfo, chain: &[u32]) -> Vec<[u8; 32]> {
        let g = arc.lock().unwrap();
        let mut out = Vec::new();
        for &c in chain {
            let base = info.cluster_to_offset(c) as usize;
            for i in 0..info.bytes_per_cluster / 32 {
                out.push(g[base + i * 32..base + i * 32 + 32].try_into().unwrap());
            }
        }
        out
    }

    /// In-use file entry sets in `slots`: index of the primary entry, and the name.
    fn in_use_names(slots: &[[u8; 32]]) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < slots.len() {
            let sec = slots[i][1] as usize;
            if slots[i][0] == 0x85 && i + sec < slots.len() && slots[i + 1][0] == 0xC0 {
                let mut units: Vec<u16> = slots[i + 2..=i + sec]
                    .iter()
                    .filter(|s| s[0] == 0xC1)
                    .flat_map(|s| {
                        (0..15).map(move |k| u16::from_le_bytes([s[2 + 2 * k], s[3 + 2 * k]]))
                    })
                    .collect();
                units.truncate(slots[i + 1][3] as usize);
                out.push((i, String::from_utf16_lossy(&units)));
                i += 1 + sec;
            } else {
                i += 1;
            }
        }
        out
    }

    /// #127: renaming to a shorter name must not end the directory at the freed slots.
    #[test]
    fn exfat_rename_to_a_shorter_name_keeps_later_entries() {
        let (arc, vol, part_bytes) = exfat_image(&ExFatFormatOptions::new());
        let long = "The Legend of Zelda - Ocarina of Time (USA) (Rev 2).z64";
        let write = |name: &str, data: &[u8]| {
            write_file_exfat_streaming(
                &vol,
                0,
                part_bytes,
                name,
                data.len() as u64,
                &mut Cursor::new(data),
                &mut |_| true,
            )
            .unwrap()
        };
        write(long, b"rom");
        for i in 0..20 {
            write(&format!("file{i:02}.z64"), b"x");
        }

        rename_cart_exfat(&vol, 0, part_bytes, long, "oot.z64").expect("rename");

        let names: Vec<_> = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 21, "{names:?}");
        assert!(names.contains(&"oot.z64".to_string()), "{names:?}");
        assert!(names.contains(&"file19.z64".to_string()), "{names:?}");
        let rom = read_file_exfat(RamPartitionDisk::new_readonly(arc), "oot.z64").unwrap();
        assert_eq!(rom, b"rom");
    }

    /// #127: when a longer name moves the entry set, the old slots are marked deleted, not zeroed.
    #[test]
    fn exfat_rename_to_a_longer_name_keeps_later_entries() {
        let (arc, vol, part_bytes) = exfat_image(&ExFatFormatOptions::new());
        let write = |name: &str| {
            write_file_exfat_streaming(
                &vol,
                0,
                part_bytes,
                name,
                1,
                &mut Cursor::new(&b"x"[..]),
                &mut |_| true,
            )
            .unwrap()
        };
        write("a.z64");
        for i in 0..20 {
            write(&format!("file{i:02}.z64"));
        }
        let long = "The Legend of Zelda - Majora's Mask (USA).z64";

        rename_cart_exfat(&vol, 0, part_bytes, "a.z64", long).expect("rename");

        let names: Vec<_> = list_dir_exfat(RamPartitionDisk::new_readonly(arc), "/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 21, "{names:?}");
        assert!(names.contains(&long.to_string()), "{names:?}");
        assert!(names.contains(&"file00.z64".to_string()), "{names:?}");
    }

    struct ChainedRoot {
        arc: Image,
        vol: ExfatVolumeSource,
        part_bytes: u64,
        info: ExFatInfo,
        /// The root's clusters in chain order. When there are two they are not adjacent: a ROM sits
        /// between them.
        chain: Vec<u32>,
        rom: Vec<u8>,
    }

    /// An exFAT volume whose root directory is two non-adjacent clusters, the first holding all but
    /// `free_slots_left` in-use slots, with a zero-padded ROM in the cluster physically after it.
    fn exfat_chained_root(free_slots_left: usize) -> ChainedRoot {
        exfat_full_root(free_slots_left, true)
    }

    /// As [`exfat_chained_root`], but the root is only its first cluster unless `chained`.
    fn exfat_full_root(free_slots_left: usize, chained: bool) -> ChainedRoot {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let mut rom = vec![0u8; 1024];
        rom.extend((0..512u32).map(|i| (i % 251) as u8 + 1));
        write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "rom.z64",
            rom.len() as u64,
            &mut Cursor::new(&rom[..]),
            &mut |_| true,
        )
        .unwrap();

        let fs = ExFatFs::open(RamPartitionDisk::new_writable(Arc::clone(&arc))).unwrap();
        let info = fs.info().clone();
        let root = info.root_cluster;
        assert_eq!(
            fs.open_path("rom.z64").unwrap().first_cluster,
            root + 1,
            "fixture: the ROM must follow the root cluster physically"
        );
        let per_cluster = info.bytes_per_cluster / 32;
        let used = raw_dir_slots(&arc, &info, &[root])
            .iter()
            .position(|s| s[0] == 0)
            .unwrap();
        let mut fill = per_cluster - used - free_slots_left;
        assert!(fill >= 3, "fixture: room for at least one entry set");
        let mut n = 0;
        while fill > 0 {
            let slots = if fill == 4 || fill == 5 { fill } else { 3 };
            // A file and a stream entry, then one name entry per 15 characters.
            let name = format!("f{n:02}{}.bin", "x".repeat((slots - 2) * 15 - 7));
            fs.create_file(&fs.root_dir(), &name).unwrap();
            fill -= slots;
            n += 1;
        }
        let mut chain = vec![root];
        if chained {
            let ext = fs.allocate_cluster(root + 20).unwrap();
            assert!(
                ext > root + 3,
                "fixture: extension cluster clear of the ROM"
            );
            fs.sync_bitmap().unwrap();
            chain.push(ext);
        }
        drop(fs);
        if let &[root, ext] = chain.as_slice() {
            let mut g = arc.lock().unwrap();
            for copy in 0..u64::from(info.fat_count) {
                let fat = info.fat_offset + copy * info.fat_length;
                let at = |c: u32| (fat + u64::from(c) * 4) as usize;
                g[at(root)..at(root) + 4].copy_from_slice(&ext.to_le_bytes());
                g[at(ext)..at(ext) + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            let base = info.cluster_to_offset(ext) as usize;
            g[base..base + info.bytes_per_cluster].fill(0);
        }
        ChainedRoot {
            arc,
            vol,
            part_bytes,
            info,
            chain,
            rom,
        }
    }

    /// #128 (and #127): a longer name in a chained root lands in the root's own clusters, mapped
    /// slot by slot through the chain, and never in the data cluster that happens to follow.
    fn rename_to_a_longer_name_in_a_chained_root(free_slots_left: usize) {
        let fx = exfat_chained_root(free_slots_left);
        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        let (old_idx, old_name) = before
            .iter()
            .find(|(_, n)| n.starts_with("f00"))
            .cloned()
            .unwrap();
        let new_name = format!("renamed-{}.bin", "y".repeat(32));

        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, &old_name, &new_name).expect("rename");

        let rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "rom.z64",
        )
        .unwrap();
        assert!(rom == fx.rom, "the rename wrote into the ROM's data");
        let slots = raw_dir_slots(&fx.arc, &fx.info, &fx.chain);
        let after = in_use_names(&slots);
        let names: Vec<&str> = after.iter().map(|(_, n)| n.as_str()).collect();
        assert!(names.contains(&new_name.as_str()), "{names:?}");
        assert!(!names.contains(&old_name.as_str()), "{names:?}");
        assert_eq!(after.len(), before.len(), "{names:?}");
        let old_types: Vec<u8> = slots[old_idx..old_idx + 3].iter().map(|s| s[0]).collect();
        assert_eq!(old_types, [0x05, 0x40, 0x41]);
    }

    /// #190: a file whose entry set lives past the root's first cluster must still be readable.
    /// Reading resolved through hadris's `open_file`, which cannot see it, so on a real card such a
    /// file was listed but could not be copied off.
    ///
    /// To watch this fail, blind the entry resolver to the chain: in [`exfat_resolve_entry`],
    /// swap the `exfat_resolve_dir_slots` call for
    /// `ExfatDirSlots::load_with(read_at, info, info.root_cluster, true, 0)`, which reproduces
    /// hadris's contiguous-root assumption on that path alone. The test then fails with
    /// `no such entry`.
    ///
    /// Do **not** break [`exfat_resolve_dir_slots`] itself instead: the rename below takes its
    /// slot map from the same resolver, so the setup dies with `StorageFull` before the case under
    /// test ever runs, and the failure tells you nothing.
    #[test]
    fn exfat_read_file_whose_entry_is_past_the_first_root_cluster() {
        let fx = exfat_chained_root(3);
        // Renaming to a longer name rewrites the entry set somewhere it fits. With only 3 slots
        // free in the first cluster it lands in the chain's second one — which is not the cluster
        // physically after the first, so a contiguous reader looks in the wrong place entirely.
        let new_name = format!("moved-{}.z64", "y".repeat(40));
        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, "rom.z64", &new_name).expect("rename");

        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            &new_name,
        )
        .expect("the entry moved into the root's second cluster; the file must still read");

        assert_eq!(
            back, fx.rom,
            "the bytes must be the file's own, not cluster slack or another file's data"
        );
    }

    /// #190: deleting an entry whose set lives past the root's first cluster. Delete resolved its
    /// entry through hadris's `open_path`, which cannot see one there, so a file that was plainly
    /// listed could not be removed.
    ///
    /// To watch this fail, blind [`exfat_resolve_entry`] to the chain, exactly as described on
    /// the read test above. A file's delete resolves in `delete_exfat_tree_remounted`, not in
    /// `delete_exfat_leaf_remounted` — the leaf is only reached for directories — and restoring
    /// `fs.open_path` there no longer type-checks, since the delete takes a decoded entry now.
    #[test]
    fn exfat_delete_an_entry_past_the_first_root_cluster() {
        let fx = exfat_chained_root(3);
        // Only 3 free slots in the first cluster, so the longer name's entry set lands in the
        // chain's second cluster — which is not the cluster physically after the first.
        let new_name = format!("doomed-{}.bin", "z".repeat(40));
        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, "rom.z64", &new_name).expect("rename");

        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert!(
            before.iter().any(|(_, n)| *n == new_name),
            "fixture: the renamed entry set should be in the chain: {before:?}"
        );

        let mut quiet = |_s: &str| {};
        remove_cart_path_exfat_unified(fx.vol.clone(), 0, fx.part_bytes, &new_name, &mut quiet)
            .expect("the entry set is in the chain's second cluster; delete must still find it");

        // Checked on the raw slots, not through a listing: a listing is the component these bugs
        // live in, so it cannot be the witness for its own fix.
        let after = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert!(
            !after.iter().any(|(_, n)| *n == new_name),
            "the deleted name must be gone from the directory: {after:?}"
        );
        assert_eq!(
            after.len(),
            before.len() - 1,
            "only the deleted entry may disappear: {after:?}"
        );
    }

    /// #190: renaming an entry whose set already lives past the root's first cluster. The rename
    /// resolved its source through hadris's `open_path`, which cannot see one there.
    ///
    /// Also pins the entry set being *copied* rather than rebuilt: the File entry's timestamps,
    /// attributes and reserved bytes must come through a rename byte-identical. Rebuilding them
    /// through hadris's `ExFatTimestamp` clamps `increment_10ms` and rewrites an invalid UTC
    /// offset, which would alter a file's times every time it was renamed.
    ///
    /// Break [`exfat_resolve_entry`] as described on the read test above to watch it fail.
    #[test]
    fn exfat_rename_an_entry_past_the_first_root_cluster() {
        let fx = exfat_chained_root(3);
        // First move it out of the first cluster; this rename is setup, not the case under test.
        let moved = format!("moved-{}.z64", "y".repeat(40));
        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, "rom.z64", &moved).expect("setup rename");

        let slots_before = raw_dir_slots(&fx.arc, &fx.info, &fx.chain);
        let (idx_before, _) = in_use_names(&slots_before)
            .into_iter()
            .find(|(_, n)| *n == moved)
            .expect("fixture: the moved entry should be in the chain");
        let stamps_before: Vec<u8> = slots_before[idx_before][8..25].to_vec();

        // The source entry now sits past the first cluster: this is the rename hadris could not do.
        let final_name = "renamed-again.z64";
        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, &moved, final_name)
            .expect("the source entry is past the first cluster; the rename must still find it");

        let slots_after = raw_dir_slots(&fx.arc, &fx.info, &fx.chain);
        let names_after = in_use_names(&slots_after);
        let (idx_after, _) = names_after
            .iter()
            .find(|(_, n)| n == final_name)
            .cloned()
            .expect("the new name must be in the directory");
        assert!(
            !names_after.iter().any(|(_, n)| *n == moved),
            "the old name must be gone: {names_after:?}"
        );
        assert_eq!(
            slots_after[idx_after][8..25].to_vec(),
            stamps_before,
            "a rename must carry the File entry's timestamps through unchanged"
        );

        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            final_name,
        )
        .expect("read the file back under its new name");
        assert_eq!(back, fx.rom, "a rename must not disturb the file's data");
    }

    /// #189: a root spanning more than one cluster must list every entry, including those in its
    /// later clusters. hadris pins the root contiguous with size 0, so its iterator steps to the
    /// physically next cluster and stops at the first zero byte there. On a real card that listed
    /// 74 of 1273 entries, and the rest could not be opened at all.
    #[test]
    fn exfat_list_dir_reads_a_root_past_its_first_cluster() {
        // Only 3 free slots left, so the longer name cannot fit in the first cluster: its entry set
        // lands in the chain's second cluster, which is exactly where a contiguous reader cannot
        // look, and which is not the cluster physically following the first.
        let fx = exfat_chained_root(3);
        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        let (_, old_name) = before
            .iter()
            .find(|(_, n)| n.starts_with("f00"))
            .cloned()
            .unwrap();
        let new_name = format!("renamed-{}.bin", "y".repeat(32));
        rename_cart_exfat(&fx.vol, 0, fx.part_bytes, &old_name, &new_name).expect("rename");

        let names: Vec<String> =
            list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), "/")
                .expect("list")
                .into_iter()
                .map(|e| e.name)
                .collect();

        assert!(
            names.contains(&new_name),
            "the renamed entry is in the root's second cluster and must still be listed: {names:?}"
        );
        assert!(
            !names.contains(&old_name),
            "the freed entry set must not be listed: {names:?}"
        );
        assert_eq!(
            names.len(),
            before.len(),
            "every entry must be listed exactly once: {names:?}"
        );
    }

    #[test]
    fn exfat_rename_in_a_full_chained_root_uses_the_next_cluster_in_the_chain() {
        rename_to_a_longer_name_in_a_chained_root(0);
    }

    #[test]
    fn exfat_rename_entry_set_straddling_chained_root_clusters() {
        rename_to_a_longer_name_in_a_chained_root(2);
    }

    fn write_small(fx: &ChainedRoot, path: &str) -> io::Result<()> {
        write_file_exfat_streaming(
            &fx.vol,
            0,
            fx.part_bytes,
            path,
            4,
            &mut Cursor::new(&b"data"[..]),
            &mut |_| true,
        )
    }

    /// Clusters the volume's allocation bitmap reports as in use.
    ///
    /// The point of counting them is that a create which fails must leave the number where it
    /// started. A leaked cluster is invisible to every listing — the space is simply gone until the
    /// card is reformatted — so nothing but the bitmap can catch it.
    fn allocated_cluster_count(arc: &Image) -> u32 {
        let fs = ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(arc))).expect("open");
        let info = fs.info().clone();
        info.cluster_count - fs.free_cluster_count()
    }

    /// Every copy of the FAT, as bytes.
    ///
    /// The allocation bitmap alone cannot show a rollback working: a claimed cluster is only marked
    /// in memory until something flushes it, so a failed create that forgets to release it leaves
    /// the on-disk bitmap looking untouched anyway. The FAT is different — `allocate_cluster` writes
    /// end-of-chain into it on the volume at once, and a fragmented file's links go there too — so
    /// a FAT that comes through byte-identical is the evidence that nothing was left behind.
    fn fat_bytes(arc: &Image) -> Vec<u8> {
        let info = ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(arc)))
            .expect("open")
            .info()
            .clone();
        let start = info.fat_offset as usize;
        let end = start + (info.fat_length * u64::from(info.fat_count)) as usize;
        arc.lock().unwrap()[start..end].to_vec()
    }

    /// A file spanning several clusters comes back byte for byte, and its entry reports the length
    /// that was asked for rather than the space it occupies (#194's other half).
    #[test]
    fn exfat_import_spanning_several_clusters_roundtrips() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        // Deliberately not a whole number of clusters: the final one is partly used, and its tail
        // must not come back as part of the file.
        let data: Vec<u8> = (0..(512 * 5 + 37)).map(|i| (i % 251) as u8).collect();

        write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "big.z64",
            data.len() as u64,
            &mut Cursor::new(&data[..]),
            &mut |_| true,
        )
        .expect("import a multi-cluster file");

        let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "big.z64")
            .expect("read it back");
        assert_eq!(back.len(), data.len(), "length");
        assert!(back == data, "contents");

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        let e = listed.iter().find(|e| e.name == "big.z64").expect("listed");
        assert_eq!(e.size, data.len() as u64, "the listed size");
    }

    /// A directory created on the way to a file that then fails must still own its cluster.
    ///
    /// The two halves of creating a directory persist at different times: the entry set is written
    /// straight to the volume, while the cluster behind it is only marked in hadris's in-memory
    /// allocation bitmap until something flushes it. If the file create then fails and nothing
    /// flushes, the card carries a directory whose cluster is still marked free — and the next
    /// allocation hands the same cluster to something else.
    ///
    /// The leaf name here is rejected **before** anything is allocated for the file, which is what
    /// makes it the case that bites: the rollback for a failed file flushes the bitmap as a side
    /// effect and so happens to persist the directory too, but a failure earlier than the
    /// allocation never reaches it.
    ///
    /// Fail-first, demonstrated: drop the bitmap sync from `exfat_create_missing_dirs`
    /// and this fails with the directory present but the allocated count unchanged.
    #[test]
    fn exfat_a_directory_made_for_a_failed_import_keeps_its_cluster() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);

        // `:` is not a legal exFAT filename character, so the leaf is refused by name validation,
        // before any cluster is allocated for it.
        write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "Saves/bad:name.z64",
            4,
            &mut Cursor::new(&b"data"[..]),
            &mut |_| true,
        )
        .expect_err("the leaf must be refused");

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert!(
            listed.iter().any(|e| e.name == "Saves" && e.is_dir),
            "the directory was created, so it must still be there: {listed:?}"
        );
        assert_eq!(
            allocated_cluster_count(&arc),
            before_allocated + 1,
            "the directory's own cluster must be marked allocated, or it will be handed out twice"
        );
    }

    /// Where a test image's FAT and root directory are, as sector numbers, so a
    /// [`flaky_cached_volume`] can fail writes by what they touch.
    struct ExfatRegions {
        fat: std::ops::Range<u64>,
        root: std::ops::Range<u64>,
    }

    impl ExfatRegions {
        fn of(arc: &Image) -> Self {
            let info = ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(arc)))
                .expect("open")
                .info()
                .clone();
            let fat_start = info.fat_offset / 512;
            let fat_end = fat_start + info.fat_length * u64::from(info.fat_count) / 512;
            let root_start = info.cluster_to_offset(info.root_cluster) / 512;
            let root_end = root_start + info.bytes_per_cluster as u64 / 512;
            Self {
                fat: fat_start..fat_end,
                root: root_start..root_end,
            }
        }

        fn is_fat(&self, lba: u64) -> bool {
            self.fat.contains(&lba)
        }

        fn is_root(&self, lba: u64) -> bool {
            self.root.contains(&lba)
        }
    }

    /// `arc` reached sector by sector, as a card is, with each write first offered to `fail`.
    fn flaky_cached_volume(
        arc: &Image,
        fail: impl FnMut(u64) -> bool + Send + 'static,
    ) -> ExfatVolumeSource {
        ExfatVolumeSource::CachedRam {
            link: Arc::new(Mutex::new(CachedRamTransport {
                image: Arc::clone(arc),
                cache: Default::default(),
                requests: 0,
                fail_write: Some(Box::new(fail)),
            })),
        }
    }

    /// `arc` reached through [`flaky_cached_volume`], failing nothing, with every sector it writes
    /// recorded in the returned list.
    fn recording_volume(arc: &Image) -> (ExfatVolumeSource, Arc<Mutex<Vec<u64>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&written);
        let vol = flaky_cached_volume(arc, move |lba| {
            log.lock().unwrap().push(lba);
            false
        });
        (vol, written)
    }

    /// A rename writes only the folder's own entries: it takes and frees no clusters, so it has no
    /// bitmap to write (#224). Both a rename in place and one that moves the set to new slots.
    ///
    /// Fail-first, demonstrated: with the bitmap sync back in `rename_cart_exfat`, both renames also
    /// write the allocation bitmap's sector, outside the root.
    #[test]
    fn exfat_rename_writes_only_the_folders_entries() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        exfat_write(&vol, part_bytes, "a.z64", 4, b"data", &mut |_| true).unwrap();
        let regions = ExfatRegions::of(&arc);

        for (from, to) in [
            ("a.z64", "b.z64"),
            ("b.z64", "a much longer name that needs more slots.z64"),
        ] {
            let (recording, written) = recording_volume(&arc);
            rename_cart_exfat(&recording, 0, part_bytes, from, to).unwrap();
            let written = written.lock().unwrap().clone();
            assert!(!written.is_empty(), "{from} -> {to} wrote nothing");
            assert!(
                written.iter().all(|&lba| regions.is_root(lba)),
                "{from} -> {to} wrote outside the root's entries: {written:?}"
            );
        }
        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "a much longer name that needs more slots.z64",
        )
        .unwrap();
        assert_eq!(back, b"data");
    }

    /// A mkdir of a path that already exists writes nothing at all (#224).
    ///
    /// Fail-first, demonstrated: with the unconditional sync `mkdir_cart_exfat` had before #213,
    /// the bitmap is written.
    #[test]
    fn exfat_mkdir_of_an_existing_path_writes_nothing() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        mkdir_cart_exfat(&vol, 0, part_bytes, "Saves/Deeper").unwrap();

        let (recording, written) = recording_volume(&arc);
        mkdir_cart_exfat(&recording, 0, part_bytes, "Saves/Deeper").unwrap();
        assert_eq!(*written.lock().unwrap(), Vec::<u64>::new());
    }

    /// A nested mkdir whose second folder cannot be made keeps the first one's cluster marked
    /// allocated on the card (#213).
    ///
    /// Fail-first, demonstrated: with the bitmap written only after the whole walk succeeds, as
    /// before, the `Saves` folder is on the card and the allocated count is unchanged.
    #[test]
    fn exfat_a_nested_mkdir_that_fails_keeps_the_folders_it_made() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);

        mkdir_cart_exfat(&vol, 0, part_bytes, "Saves/bad:name")
            .expect_err("the second folder's name is refused");

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert!(
            listed.iter().any(|e| e.name == "Saves" && e.is_dir),
            "{listed:?}"
        );
        assert_eq!(allocated_cluster_count(&arc), before_allocated + 1);
    }

    /// The same for an import whose destination folders are made on the way (#213): a parent that
    /// cannot be made, not the file, is what fails.
    ///
    /// Fail-first, demonstrated: as for the mkdir, the allocated count is unchanged.
    #[test]
    fn exfat_an_import_whose_second_parent_fails_keeps_the_first() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);

        write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "Saves/bad:dir/game.sav",
            4,
            &mut Cursor::new(&b"data"[..]),
            &mut |_| true,
        )
        .expect_err("the second parent's name is refused");

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert!(
            listed.iter().any(|e| e.name == "Saves" && e.is_dir),
            "{listed:?}"
        );
        assert_eq!(allocated_cluster_count(&arc), before_allocated + 1);
    }

    /// A folder that fails part-way through being made, after its cluster was taken, gives that
    /// cluster back, while the folder made before it keeps its own (#213).
    ///
    /// The write that fails is the second folder's zeroed cluster: the first write into the heap,
    /// outside the root, after `Saves`'s entry set reached the root. Only that one write fails.
    ///
    /// Fail-first, demonstrated: with no give-back in `exfat_create_dir_in`, the count is two up,
    /// the failed folder's cluster leaked. (The give-back writes the bitmap itself, so this test
    /// does not also catch a walk that skips the sync on failure; the two tests above do.) The FAT
    /// check shows the failed folder's end-of-chain came back out.
    #[test]
    fn exfat_a_folder_that_fails_after_taking_its_cluster_gives_it_back() {
        let (arc, _, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);
        let before_fat = fat_bytes(&arc);
        let regions = ExfatRegions::of(&arc);
        let heap_start = regions.root.start;
        let (mut root_written, mut failed) = (false, false);
        let vol = flaky_cached_volume(&arc, move |lba| {
            root_written |= regions.is_root(lba);
            let fail = root_written && !failed && lba >= heap_start && !regions.is_root(lba);
            failed |= fail;
            fail
        });

        let err = mkdir_cart_exfat(&vol, 0, part_bytes, "Saves/Deeper")
            .expect_err("the second folder's cluster write fails");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert!(
            listed.iter().any(|e| e.name == "Saves" && e.is_dir),
            "{listed:?}"
        );
        assert_eq!(allocated_cluster_count(&arc), before_allocated + 1);
        assert!(fat_bytes(&arc) == before_fat, "no end-of-chain left behind");
    }

    /// Cancelling an import leaves no entry behind, loses no space, and leaves the FAT exactly as
    /// it was.
    ///
    /// This is the **contiguous** case, and it passes even without [`exfat_release_file_clusters`]
    /// — correctly, not weakly. A contiguous run's FAT entries are cleared the moment it is taken,
    /// and the bitmap reaches the card only when a create succeeds, so a cancel part-way through
    /// leaves the volume byte-identical either way. The rollback is shown to matter on the
    /// fragmented path instead, by `exfat_cancelled_fragmented_import_unwinds_its_chain`.
    #[test]
    fn exfat_cancelled_import_frees_what_it_allocated() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);
        let before_fat = fat_bytes(&arc);
        let before_names = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list")
            .len();
        let data = vec![7u8; 512 * 4];

        let err = write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "cancelled.z64",
            data.len() as u64,
            &mut Cursor::new(&data[..]),
            // Refuse after the first cluster's worth of progress.
            &mut |_| false,
        )
        .expect_err("a cancelled import");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");

        assert_eq!(
            allocated_cluster_count(&arc),
            before_allocated,
            "a cancelled import must not leak clusters"
        );
        assert!(
            fat_bytes(&arc) == before_fat,
            "a cancelled import left entries in the FAT"
        );
        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert_eq!(listed.len(), before_names, "no entry: {listed:?}");
    }

    /// A volume whose free space is fragmented into single clusters, built only through this
    /// crate's own create and delete paths.
    ///
    /// 32 KiB clusters keep the volume to about a hundred of them. It is filled with one-cluster
    /// files until the **allocation bitmap** reports nothing free — not until a write fails, since
    /// a write into a full volume is exactly the path under suspicion — and then every other file
    /// is deleted. What is left is a checkerboard: no two free clusters are adjacent, so any file
    /// longer than one cluster cannot be allocated contiguously.
    struct FragmentedVolume {
        arc: Image,
        vol: ExfatVolumeSource,
        part_bytes: u64,
        info: ExFatInfo,
        /// The files still on the volume, with the exact bytes each should hold.
        survivors: Vec<(String, Vec<u8>)>,
    }

    fn exfat_fragmented_volume() -> FragmentedVolume {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(64));
        let info = ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(&arc)))
            .expect("open")
            .info()
            .clone();
        let cluster_bytes = info.bytes_per_cluster;
        let free = |arc: &Image| {
            ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(arc)))
                .expect("open")
                .free_cluster_count()
        };

        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        while free(&arc) > 0 {
            let i = files.len();
            assert!(i < 10_000, "fixture: the volume never filled");
            let name = format!("fill{i:04}.bin");
            // Distinct bytes per file, so a cluster handed to the wrong file shows up as a mismatch
            // rather than as identical zeros.
            let body: Vec<u8> = (0..cluster_bytes)
                .map(|k| (i as u8).wrapping_mul(31) ^ (k % 253) as u8)
                .collect();
            write_file_exfat_streaming(
                &vol,
                0,
                part_bytes,
                &name,
                body.len() as u64,
                &mut Cursor::new(&body[..]),
                &mut |_| true,
            )
            .expect("fixture: fill the volume");
            files.push((name, body));
        }
        assert!(files.len() >= 8, "fixture: too few clusters to fragment");

        let mut survivors = Vec::new();
        for (i, (name, body)) in files.into_iter().enumerate() {
            if i % 2 == 0 {
                remove_cart_path_exfat_unified(vol.clone(), 0, part_bytes, &name, &mut |_| {})
                    .expect("fixture: delete every other file");
            } else {
                survivors.push((name, body));
            }
        }
        FragmentedVolume {
            arc,
            vol,
            part_bytes,
            info,
            survivors,
        }
    }

    /// An import too long for any free run takes a FAT chain through the scattered free clusters,
    /// and **every other file on the volume comes through byte for byte**.
    ///
    /// The survivors are the assertion that matters. A contiguous (NoFatChain) file's FAT entries
    /// are free by the exFAT spec — only the allocation bitmap records that its clusters are in use
    /// — so an allocator that looks for free clusters in the FAT rather than the bitmap will hand
    /// out clusters belonging to live files, and the damage lands in *those* files, not the new one.
    ///
    /// Fail-first, demonstrated: against the first version of this change, which allocated through
    /// hadris's `allocate_clusters`, this failed with *"fill0001.bin was overwritten by the
    /// fragmented import"*.
    #[test]
    fn exfat_fragmented_import_chains_through_free_clusters_without_touching_other_files() {
        let fx = exfat_fragmented_volume();
        let cluster_bytes = fx.info.bytes_per_cluster;
        let before_allocated = allocated_cluster_count(&fx.arc);

        // Three and a bit clusters: four, none of which can be adjacent.
        let data: Vec<u8> = (0..cluster_bytes * 3 + 1000)
            .map(|k| 0xA5 ^ (k % 241) as u8)
            .collect();
        write_file_exfat_streaming(
            &fx.vol,
            0,
            fx.part_bytes,
            "frag.bin",
            data.len() as u64,
            &mut Cursor::new(&data[..]),
            &mut |_| true,
        )
        .expect("import into fragmented free space");

        for (name, body) in &fx.survivors {
            let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), name)
                .unwrap_or_else(|e| panic!("read {name} back: {e}"));
            assert!(
                back == *body,
                "{name} was overwritten by the fragmented import"
            );
        }

        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "frag.bin",
        )
        .expect("read the import back");
        assert!(back == data, "the fragmented import's own contents");

        // Prove the fragmented path was taken, or the survivors above prove nothing.
        let root = raw_dir_slots(&fx.arc, &fx.info, &[fx.info.root_cluster]);
        let (at, _) = in_use_names(&root)
            .into_iter()
            .find(|(_, n)| n == "frag.bin")
            .expect("frag.bin's entry set");
        assert_eq!(
            root[at + 1][1] & 0x02,
            0,
            "the import must not claim NoFatChain: its clusters cannot be contiguous"
        );

        assert_eq!(
            allocated_cluster_count(&fx.arc),
            before_allocated + 4,
            "exactly the import's four clusters should have been allocated"
        );
    }

    /// Cancelling a fragmented import unwinds the chain: the links written between its clusters
    /// come back out of the FAT, the clusters are free again, and every other file is untouched.
    ///
    /// Fail-first, demonstrated: skip [`exfat_release_file_clusters`] and this fails with *"a
    /// cancelled fragmented import left its chain in the FAT"*.
    #[test]
    fn exfat_cancelled_fragmented_import_unwinds_its_chain() {
        let fx = exfat_fragmented_volume();
        let cluster_bytes = fx.info.bytes_per_cluster;
        let before_allocated = allocated_cluster_count(&fx.arc);
        let before_fat = fat_bytes(&fx.arc);
        let data = vec![0x3Cu8; cluster_bytes * 3 + 1000];

        let err = write_file_exfat_streaming(
            &fx.vol,
            0,
            fx.part_bytes,
            "frag.bin",
            data.len() as u64,
            &mut Cursor::new(&data[..]),
            &mut |_| false,
        )
        .expect_err("a cancelled import");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");

        assert!(
            fat_bytes(&fx.arc) == before_fat,
            "a cancelled fragmented import left its chain in the FAT"
        );
        assert_eq!(
            allocated_cluster_count(&fx.arc),
            before_allocated,
            "a cancelled fragmented import must not leak clusters"
        );
        for (name, body) in &fx.survivors {
            let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), name)
                .unwrap_or_else(|e| panic!("read {name} back: {e}"));
            assert!(
                back == *body,
                "{name} was overwritten by the cancelled import"
            );
        }
    }

    /// Two images that must be identical except where a clock was read.
    ///
    /// Compared in 32-byte slots, which is how directory entries sit on the volume. A slot may
    /// differ only if it is a File entry in both images, in use (`0x85`) or deleted (`0x05`), and
    /// only in its timestamp bytes (8..24) and the entry-set checksum that covers them (2..4). Those
    /// are the only bytes a create stamps with the time; every other byte of the volume — FAT,
    /// bitmap, file data, names, lengths, which slots are used — must match exactly.
    fn assert_same_but_for_timestamps(a: &[u8], b: &[u8]) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.chunks(32).zip(b.chunks(32)).enumerate() {
            if x == y {
                continue;
            }
            let at = i * 32;
            assert!(
                x[0] == y[0] && (x[0] == 0x85 || x[0] == 0x05),
                "images differ at volume offset {at:#x}, outside any File entry: {x:02x?} vs {y:02x?}"
            );
            for k in (0..32).filter(|&k| x[k] != y[k]) {
                assert!(
                    matches!(k, 2 | 3 | 8..=23),
                    "File entry at {at:#x} differs at byte {k}, not a timestamp: {x:02x?} vs {y:02x?}"
                );
            }
        }
    }

    /// The session read cache never changes what an operation writes (#196).
    ///
    /// The same sequence runs on two copies of the chained-root fixture: one read and written
    /// directly, one reached sector by sector through [`SdReadCache`], as `Sc64Link` reaches a card.
    /// The sequence is chosen to write over sectors the cache has just served: an import into the
    /// chained root, a multi-cluster file in a new folder, a rename, a replace, a nested mkdir, a
    /// recursive delete of that folder, and a final mkdir. Every step re-reads the root it has just
    /// written to. A stale read anywhere — the root slots, the FAT, the bitmap — makes some later
    /// step place or free something differently, and the images diverge.
    ///
    /// It also checks the cache is doing something: fewer reads reached the image than were asked
    /// for.
    ///
    /// Fail-first, demonstrated: stop `write_through` evicting what a write overlaps and this fails
    /// at the second step, before the images are ever compared — *"write Saves/big.bin: no such
    /// folder: Saves"*. The folder has just been created; the root read that should find it is
    /// served from before the create.
    #[test]
    fn exfat_operations_through_the_session_read_cache_write_the_same_image() {
        let fx = exfat_chained_root(1);
        let (part_bytes, rom) = (fx.part_bytes, fx.rom.clone());
        let plain = Arc::new(Mutex::new(fx.arc.lock().unwrap().clone()));
        let cached_image = Arc::new(Mutex::new(fx.arc.lock().unwrap().clone()));
        let transport = Arc::new(Mutex::new(CachedRamTransport {
            image: Arc::clone(&cached_image),
            cache: Default::default(),
            requests: 0,
            fail_write: None,
        }));
        let vols = [
            ExfatVolumeSource::Ram {
                buf: Arc::clone(&plain),
            },
            ExfatVolumeSource::CachedRam {
                link: Arc::clone(&transport),
            },
        ];

        let big: Vec<u8> = (0..3000).map(|i| (i % 239) as u8).collect();
        let bigger: Vec<u8> = (0..2000).map(|i| 0x5A ^ (i % 233) as u8).collect();
        let write = |vol: &ExfatVolumeSource, path: &str, body: &[u8]| {
            write_file_exfat_streaming(
                vol,
                0,
                part_bytes,
                path,
                body.len() as u64,
                &mut Cursor::new(body),
                &mut |_| true,
            )
            .unwrap_or_else(|e| panic!("write {path}: {e}"))
        };

        // Step by step on both, so the two runs read the clock as close together as they can.
        for (label, step) in [
            ("import into the chained root", 0),
            ("multi-cluster file in a new folder", 1),
            ("rename", 2),
            ("replace", 3),
            ("nested mkdir", 4),
            ("recursive delete", 5),
            ("final mkdir", 6),
        ] {
            for vol in &vols {
                match step {
                    0 => write(vol, "a.z64", b"data"),
                    1 => write(vol, "Saves/big.bin", &big),
                    2 => rename_cart_exfat(vol, 0, part_bytes, "a.z64", "renamed.z64")
                        .unwrap_or_else(|e| panic!("{label}: {e}")),
                    3 => write(vol, "Saves/big.bin", &bigger),
                    4 => mkdir_cart_exfat(vol, 0, part_bytes, "Saves/Inner")
                        .unwrap_or_else(|e| panic!("{label}: {e}")),
                    5 => remove_cart_path_exfat_unified(
                        vol.clone(),
                        0,
                        part_bytes,
                        "Saves",
                        &mut |_| {},
                    )
                    .unwrap_or_else(|e| panic!("{label}: {e}")),
                    _ => mkdir_cart_exfat(vol, 0, part_bytes, "Last")
                        .unwrap_or_else(|e| panic!("{label}: {e}")),
                }
            }
        }

        let plain = plain.lock().unwrap().clone();
        let cached = cached_image.lock().unwrap().clone();
        assert_same_but_for_timestamps(&plain, &cached);

        // The plain run's own results, so identical images are also correct ones.
        let names: Vec<String> = list_dir_exfat(
            RamPartitionDisk::new_readonly(Arc::new(Mutex::new(cached.clone()))),
            "/",
        )
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
        assert!(names.contains(&"renamed.z64".to_string()), "{names:?}");
        assert!(names.contains(&"Last".to_string()), "{names:?}");
        assert!(
            !names.iter().any(|n| n == "Saves" || n == "a.z64"),
            "{names:?}"
        );
        let back_rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::new(Mutex::new(cached))),
            "rom.z64",
        )
        .unwrap();
        assert!(back_rom == rom, "the ROM came through changed");

        let t = transport.lock().unwrap();
        assert!(
            t.cache.fetches < t.requests,
            "the cache served nothing: {} of {} reads reached the image",
            t.cache.fetches,
            t.requests
        );
        println!(
            "{} of {} reads reached the image through the cache",
            t.cache.fetches, t.requests
        );
    }

    fn exfat_write(
        vol: &ExfatVolumeSource,
        part_bytes: u64,
        path: &str,
        declared: u64,
        body: &[u8],
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> io::Result<()> {
        write_file_exfat_streaming(
            vol,
            0,
            part_bytes,
            path,
            declared,
            &mut Cursor::new(body),
            &mut |n| progress(n),
        )
    }

    /// Everything an interrupted replace must leave as it found: the original's bytes, the folder's
    /// listing, the FAT, and how many clusters are in use.
    fn assert_replace_left_the_original(
        arc: &Image,
        original: &[u8],
        listing_before: &[(String, u64)],
        fat_before: &[u8],
        allocated_before: u32,
    ) {
        assert_replace_left_the_original_at(
            arc,
            "game.z64",
            original,
            listing_before,
            fat_before,
            allocated_before,
        );
    }

    /// [`assert_replace_left_the_original`] for an original at `path`.
    fn assert_replace_left_the_original_at(
        arc: &Image,
        path: &str,
        original: &[u8],
        listing_before: &[(String, u64)],
        fat_before: &[u8],
        allocated_before: u32,
    ) {
        let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(arc)), path)
            .expect("the original must still be there");
        assert!(back == original, "the original's contents changed");
        let listing: Vec<(String, u64)> =
            list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(arc)), "/")
                .unwrap()
                .into_iter()
                .map(|e| (e.name, e.size))
                .collect();
        assert_eq!(listing, listing_before, "the folder changed");
        assert!(fat_bytes(arc) == fat_before, "the FAT changed");
        assert_eq!(
            allocated_cluster_count(arc),
            allocated_before,
            "clusters leaked or were freed"
        );
    }

    /// An exFAT volume holding `game.z64`, with the state a failed replace must preserve.
    struct WithOriginal {
        arc: Image,
        vol: ExfatVolumeSource,
        part_bytes: u64,
        original: Vec<u8>,
        listing: Vec<(String, u64)>,
        fat: Vec<u8>,
        allocated: u32,
    }

    fn exfat_with_original(len: usize) -> WithOriginal {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let original: Vec<u8> = (0..len).map(|i| (i % 241) as u8 ^ 0x5A).collect();
        exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            len as u64,
            &original,
            &mut |_| true,
        )
        .unwrap();
        let listing = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.size))
            .collect();
        let fat = fat_bytes(&arc);
        let allocated = allocated_cluster_count(&arc);
        WithOriginal {
            arc,
            vol,
            part_bytes,
            original,
            listing,
            fat,
            allocated,
        }
    }

    /// #200: cancelling a replace leaves the original exactly as it was.
    ///
    /// Fail-first, demonstrated: restore the delete-before-create order in
    /// `write_file_exfat_streaming` and this fails at once — *"the original must still be there"*.
    #[test]
    fn exfat_cancelled_replace_keeps_the_original() {
        let WithOriginal {
            arc,
            vol,
            part_bytes,
            original,
            listing,
            fat,
            allocated,
        } = exfat_with_original(3000);
        let replacement = vec![0xEEu8; 9000];

        let err = exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            9000,
            &replacement,
            &mut |_| false,
        )
        .expect_err("a cancelled replace");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");

        assert_replace_left_the_original(&arc, &original, &listing, &fat, allocated);
    }

    /// A replacement whose source runs short is refused with the original intact, the same as a
    /// cancel: the original is only released once the new copy is complete.
    #[test]
    fn exfat_replace_from_a_short_source_keeps_the_original() {
        let WithOriginal {
            arc,
            vol,
            part_bytes,
            original,
            listing,
            fat,
            allocated,
        } = exfat_with_original(3000);

        exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            9000,
            b"far too short",
            &mut |_| true,
        )
        .expect_err("a short source");

        assert_replace_left_the_original(&arc, &original, &listing, &fat, allocated);
    }

    /// A completed replace holds the new contents under the name, once, and gives back the
    /// original's clusters: 3000 bytes was six 512-byte clusters, 1000 bytes is two.
    #[test]
    fn exfat_replace_releases_the_originals_clusters() {
        let WithOriginal {
            arc,
            vol,
            part_bytes,
            listing,
            allocated,
            ..
        } = exfat_with_original(3000);
        let replacement: Vec<u8> = (0..1000).map(|i| (i % 229) as u8).collect();

        exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            1000,
            &replacement,
            &mut |_| true,
        )
        .expect("replace");

        let back =
            read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "game.z64").unwrap();
        assert!(back == replacement);
        let names: Vec<String> =
            list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect();
        assert_eq!(
            names,
            listing.into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "the same names, no duplicate"
        );
        assert_eq!(allocated_cluster_count(&arc), allocated - 6 + 2);
    }

    /// A replace whose new entry set is on the card, but whose freeing of the old copy fails,
    /// still writes the new file's clusters into the bitmap (#214).
    ///
    /// Every FAT write after the entry set lands fails, as a USB timeout would; the old copy's
    /// free clears its (NoFatChain) FAT range, so that is where it stops. The error is reported,
    /// and the new file is there, but its clusters must be marked allocated on the card, or the
    /// next import is handed them.
    ///
    /// Fail-first, demonstrated: with the bitmap sync back after the `?` on the free, the replace
    /// reports the same error but the allocated count is the original's, unchanged: the new
    /// file's two clusters are free on the card.
    #[test]
    fn exfat_replace_whose_old_copy_cannot_be_freed_still_marks_the_new_one() {
        let WithOriginal {
            arc,
            part_bytes,
            allocated,
            ..
        } = exfat_with_original(3000);
        let regions = ExfatRegions::of(&arc);
        let mut entry_set_written = false;
        let vol = flaky_cached_volume(&arc, move |lba| {
            entry_set_written |= regions.is_root(lba);
            entry_set_written && regions.is_fat(lba)
        });
        let replacement: Vec<u8> = (0..1000).map(|i| (i % 229) as u8).collect();

        let err = exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            1000,
            &replacement,
            &mut |_| true,
        )
        .expect_err("the old copy's free fails");
        assert!(
            err.to_string().contains("could not free the old copy"),
            "{err}"
        );

        let back =
            read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "game.z64").unwrap();
        assert!(back == replacement, "the new copy is the file on the card");
        assert_eq!(
            allocated_cluster_count(&arc),
            allocated - 6 + 2,
            "the new file's clusters must be marked allocated on the card"
        );
    }

    /// A 240-character name: an 18-slot entry set, 576 bytes, so it always crosses a sector.
    fn long_exfat_name() -> String {
        format!("{}.z64", "a".repeat(236))
    }

    /// An image with a 4-sector (2048-byte) root cluster and a 3000-byte original under
    /// [`long_exfat_name`], plus what a test compares against afterwards.
    fn exfat_with_long_named_original() -> WithOriginal {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(4));
        let original: Vec<u8> = (0..3000).map(|i| (i % 241) as u8 ^ 0x5A).collect();
        exfat_write(
            &vol,
            part_bytes,
            &long_exfat_name(),
            3000,
            &original,
            &mut |_| true,
        )
        .unwrap();
        let listing = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.size))
            .collect();
        let fat = fat_bytes(&arc);
        let allocated = allocated_cluster_count(&arc);
        WithOriginal {
            arc,
            vol,
            part_bytes,
            original,
            listing,
            fat,
            allocated,
        }
    }

    /// A replace whose entry set crosses a sector, and whose second sector's write fails, puts the
    /// first sector back and leaves the original exactly as it was (#215).
    ///
    /// Only that one write fails, so the restore succeeds.
    ///
    /// Fail-first, demonstrated: with the restore in `exfat_write_entry_slabs_restoring` skipped
    /// (still reporting the directory restored), the first sector keeps the new File and Stream
    /// entries over the old names, pointing at the replacement's freed cluster, and the original
    /// reads back with the wrong contents.
    #[test]
    fn exfat_replace_whose_entry_set_write_fails_part_way_keeps_the_original() {
        let WithOriginal {
            arc,
            part_bytes,
            original,
            listing,
            fat,
            allocated,
            ..
        } = exfat_with_long_named_original();
        let regions = ExfatRegions::of(&arc);
        let (mut root_writes, mut failed) = (0, false);
        let vol = flaky_cached_volume(&arc, move |lba| {
            if !regions.is_root(lba) || failed {
                return false;
            }
            root_writes += 1;
            failed = root_writes == 2;
            failed
        });
        let replacement: Vec<u8> = (0..1000).map(|i| (i % 229) as u8).collect();

        let err = exfat_write(
            &vol,
            part_bytes,
            &long_exfat_name(),
            1000,
            &replacement,
            &mut |_| true,
        )
        .expect_err("the entry set's second sector fails");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");

        assert_replace_left_the_original_at(
            &arc,
            &long_exfat_name(),
            &original,
            &listing,
            &fat,
            allocated,
        );
    }

    /// The same failure, but the sector cannot be put back either: the clusters of both copies stay
    /// allocated, and the error says to check the card (#215). Space may be lost; nothing an entry
    /// on the card might point at is handed out again.
    ///
    /// Fail-first, demonstrated: releasing the new clusters regardless, as before, gives an
    /// allocated count of the original's alone.
    #[test]
    fn exfat_an_entry_set_that_cannot_be_put_back_keeps_its_clusters() {
        let WithOriginal {
            arc,
            part_bytes,
            allocated,
            ..
        } = exfat_with_long_named_original();
        let regions = ExfatRegions::of(&arc);
        let mut root_writes = 0;
        let vol = flaky_cached_volume(&arc, move |lba| {
            if !regions.is_root(lba) {
                return false;
            }
            root_writes += 1;
            root_writes >= 2
        });
        let replacement: Vec<u8> = (0..1000).map(|i| (i % 229) as u8).collect();

        let err = exfat_write(
            &vol,
            part_bytes,
            &long_exfat_name(),
            1000,
            &replacement,
            &mut |_| true,
        )
        .expect_err("the entry set's second sector fails, and so does the restore");
        assert_eq!(
            allocated_cluster_count(&arc),
            allocated + 1,
            "the replacement's cluster stays allocated beside the original's two"
        );
        assert!(err.to_string().contains("disk check"), "{err}");
    }

    /// A source with more bytes than the length it was measured at is refused and rolled back,
    /// not cut short (#220).
    ///
    /// Fail-first, demonstrated: without the check after the stream, the import succeeds with the
    /// first 1000 of 1500 bytes.
    #[test]
    fn exfat_import_of_a_source_that_grew_is_refused() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_names = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .unwrap()
            .len();
        let before_allocated = allocated_cluster_count(&arc);
        let before_fat = fat_bytes(&arc);
        let grown = vec![0x33u8; 1500];

        let err = exfat_write(&vol, part_bytes, "log.txt", 1000, &grown, &mut |_| true)
            .expect_err("the source has more than its declared length");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert!(err.to_string().contains("grew"), "{err}");

        assert_eq!(
            list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
                .unwrap()
                .len(),
            before_names
        );
        assert_eq!(allocated_cluster_count(&arc), before_allocated);
        assert!(fat_bytes(&arc) == before_fat);
    }

    /// A replace that fits only if the original's space is freed first is refused, with the
    /// original intact, rather than falling back to deleting first.
    ///
    /// This is the behaviour #200 trades for safety: the old order would have succeeded here.
    #[test]
    fn exfat_replace_without_room_for_both_copies_is_refused() {
        let WithOriginal {
            arc,
            vol,
            part_bytes,
            original,
            ..
        } = exfat_with_original(4 * 512);
        // Fill the rest of the volume, leaving two free clusters: too few for a four-cluster copy,
        // though the original's four would make room if they were released first.
        let free = ExFatFs::open(RamPartitionDisk::new_readonly(Arc::clone(&arc)))
            .unwrap()
            .free_cluster_count() as usize;
        let filler = vec![0x11u8; (free - 2) * 512];
        exfat_write(
            &vol,
            part_bytes,
            "filler.bin",
            filler.len() as u64,
            &filler,
            &mut |_| true,
        )
        .expect("fill the volume");
        let listing = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.size))
            .collect::<Vec<_>>();
        let (fat, allocated) = (fat_bytes(&arc), allocated_cluster_count(&arc));

        let err = exfat_write(
            &vol,
            part_bytes,
            "game.z64",
            4 * 512,
            &[0xEE; 4 * 512],
            &mut |_| true,
        )
        .expect_err("no room for both copies");
        assert_eq!(err.kind(), io::ErrorKind::StorageFull, "{err}");
        assert!(err.to_string().contains("room for both"), "{err}");

        assert_replace_left_the_original(&arc, &original, &listing, &fat, allocated);
    }

    /// A source that supplies fewer bytes than it declared is refused outright, rather than
    /// recorded as a file whose entry disagrees with its data.
    #[test]
    fn exfat_import_shorter_than_declared_is_refused_and_frees_its_clusters() {
        let (arc, vol, part_bytes) =
            exfat_image(&ExFatFormatOptions::new().with_sectors_per_cluster(1));
        let before_allocated = allocated_cluster_count(&arc);
        let before_fat = fat_bytes(&arc);

        let err = write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "short.z64",
            4096,
            &mut Cursor::new(&b"only a few bytes"[..]),
            &mut |_| true,
        )
        .expect_err("a source shorter than it declared");
        assert!(
            err.to_string().contains("where 4096 were expected"),
            "{err}"
        );

        assert_eq!(
            allocated_cluster_count(&arc),
            before_allocated,
            "a refused import must not leak clusters"
        );
        assert!(
            fat_bytes(&arc) == before_fat,
            "a refused import left entries in the FAT"
        );
        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/")
            .expect("list the root");
        assert!(
            !listed.iter().any(|e| e.name == "short.z64"),
            "no entry: {listed:?}"
        );
    }

    /// Neither the root's slots nor the ROM after its first cluster changed.
    fn assert_nothing_written(fx: &ChainedRoot, root_before: &[[u8; 32]]) {
        let rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "rom.z64",
        )
        .unwrap();
        assert!(
            rom == fx.rom,
            "a new entry set was written into the ROM's data"
        );
        assert!(
            raw_dir_slots(&fx.arc, &fx.info, &fx.chain) == root_before,
            "the root's slots changed"
        );
    }

    /// A single-cluster root with no run of free slots long enough has nowhere to put the entry
    /// set, so the create fails having written nothing.
    ///
    /// #175 is what makes the *nothing* part worth a test. hadris would take the first run of free
    /// slots and finish the entry set past the directory's end, in whatever follows it on disk —
    /// here a ROM. Both paths now reserve their slots through [`ExfatDirSlots`] before they write
    /// or allocate anything, so a full directory is an error rather than a corruption.
    fn create_past_the_root_cluster_has_nowhere_to_go(free_slots_left: usize) {
        let fx = exfat_full_root(free_slots_left, false);
        let before = raw_dir_slots(&fx.arc, &fx.info, &fx.chain);

        // 14 characters: a File, a Stream Extension and one File Name entry.
        let err = write_small(&fx, "new-import.z64").expect_err("an import past the root");
        assert!(err.to_string().contains("no room"), "{err}");
        mkdir_cart_exfat(&fx.vol, 0, fx.part_bytes, "Saves").expect_err("a folder past the root");

        assert_nothing_written(&fx, &before);
    }

    #[test]
    fn exfat_create_in_a_full_root_has_nowhere_to_go() {
        create_past_the_root_cluster_has_nowhere_to_go(0);
    }

    #[test]
    fn exfat_create_that_would_run_past_the_root_cluster_has_nowhere_to_go() {
        create_past_the_root_cluster_has_nowhere_to_go(2);
    }

    /// A create that does fit must still happen: the bound is the directory's real end, not a
    /// blanket refusal.
    #[test]
    fn exfat_create_that_fits_the_root_cluster_is_allowed() {
        let fx = exfat_full_root(3, false);

        write_small(&fx, "new-import.z64").expect("import");

        let rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "rom.z64",
        )
        .unwrap();
        assert!(rom == fx.rom, "the import wrote into the ROM's data");
        let names = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert!(
            names.iter().any(|(_, n)| n == "new-import.z64"),
            "{names:?}"
        );
        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "new-import.z64",
        )
        .unwrap();
        assert_eq!(back, b"data");
    }

    /// #190: a file can be imported into a root that spans several clusters. This was refused
    /// outright while hadris did the writing — it lists only the root's first cluster, so it could
    /// not see a name already further along and would have added a second entry carrying it.
    ///
    /// The fixture leaves **one** free slot in the first cluster, so the three-slot entry set has
    /// to start in the chain's second cluster, which is not the cluster physically after the first.
    /// A ROM sits in that physically-next cluster. Its bytes coming through unchanged is the whole
    /// point: writing into them is the corruption #175 describes, and a create that walks off the
    /// chain lands there.
    ///
    /// Fail-first, demonstrated: replace `parent.offsets(..)` in [`exfat_create_file_in`] with
    /// offsets computed as `base + i * 32` — treating the directory as contiguous, which is exactly
    /// what hadris does — and this fails with *"the import wrote into the ROM's data"*. It
    /// reproduces the corruption in #175 rather than merely erroring.
    #[test]
    fn exfat_import_into_a_root_past_one_cluster_works() {
        let fx = exfat_chained_root(1);
        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));

        write_small(&fx, "a.z64").expect("an import into a root past one cluster");

        let rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "rom.z64",
        )
        .expect("read the ROM back");
        assert!(rom == fx.rom, "the import wrote into the ROM's data");

        let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), "a.z64")
            .expect("read the import back");
        assert_eq!(back, b"data", "the imported file's contents");

        let after = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert_eq!(
            after.len(),
            before.len() + 1,
            "exactly one entry should have been added: {after:?}"
        );

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), "/")
            .expect("list the root");
        assert_eq!(
            listed.iter().filter(|e| e.name == "a.z64").count(),
            1,
            "the import must be listed exactly once"
        );
    }

    /// #190: a directory can now be created in a root that spans several clusters, because the
    /// entry set is placed through [`ExfatDirSlots`] rather than by hadris's scan. That is what
    /// hadris could not do this at all: it does not list a chained root's later clusters.
    ///
    /// The fixture leaves **one** free slot in the first cluster, so the three-slot entry set has
    /// to go into the chain's second cluster — which is not the cluster physically after the first.
    /// A ROM sits in that physically-next cluster, and it must come through untouched: writing
    /// into it is exactly the corruption the guard exists to prevent.
    #[test]
    fn exfat_mkdir_works_in_a_root_past_one_cluster() {
        let fx = exfat_chained_root(1);
        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));

        mkdir_cart_exfat(&fx.vol, 0, fx.part_bytes, "Saves")
            .expect("a directory in a root past one cluster");

        let rom = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            "rom.z64",
        )
        .unwrap();
        assert!(rom == fx.rom, "the create wrote into the ROM's data");

        let after = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert_eq!(
            after.len(),
            before.len() + 1,
            "exactly one entry should have been added: {after:?}"
        );

        let listed = list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)), "/")
            .expect("list the root");
        assert!(
            listed.iter().any(|e| e.name == "Saves" && e.is_dir),
            "the new directory must be listed: {:?}",
            listed.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
    }

    /// Replacing a file in a root with **no free slots at all** works, because the delete frees
    /// exactly the slots the replacement needs: same name, so the same number of entries.
    ///
    /// This used to be refused. The guard ran before the delete, so a replace with no room kept the
    /// original — the best that could be done when hadris picked the slots and might have written
    /// them past the directory's end. Placing the entry set ourselves makes the refusal unnecessary
    /// rather than merely safe.
    #[test]
    fn exfat_replace_works_in_a_root_with_no_free_slots() {
        let fx = exfat_full_root(0, false);
        let before = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        let (_, existing) = before
            .iter()
            .find(|(_, n)| n.starts_with("f00"))
            .cloned()
            .unwrap();

        write_small(&fx, &existing).expect("a replace in a root with no free slots");

        let back = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&fx.arc)),
            &existing,
        )
        .expect("read the replacement back");
        assert_eq!(back, b"data", "the replacement's contents");

        let after = in_use_names(&raw_dir_slots(&fx.arc, &fx.info, &fx.chain));
        assert_eq!(
            after.len(),
            before.len(),
            "a replace must not change how many entries the root holds"
        );
        assert_eq!(
            after.iter().filter(|(_, n)| *n == existing).count(),
            1,
            "exactly one entry should carry the name, not a duplicate: {after:?}"
        );
    }

    /// An entry set as exFAT stores it: File, Stream Extension, then File Name entries.
    fn raw_entry_set(name: &str, first_cluster: u32, len: u64, is_dir: bool) -> Vec<[u8; 32]> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut file = [0u8; 32];
        file[0] = 0x85;
        file[1] = (1 + (units.len() + 14) / 15) as u8;
        file[4] = if is_dir { 0x10 } else { 0x20 };
        let mut stream = [0u8; 32];
        stream[0] = 0xC0;
        stream[3] = units.len() as u8;
        stream[8..16].copy_from_slice(&len.to_le_bytes());
        stream[20..24].copy_from_slice(&first_cluster.to_le_bytes());
        stream[24..32].copy_from_slice(&len.to_le_bytes());
        let mut out = vec![file, stream];
        for chunk in units.chunks(15) {
            let mut e = [0u8; 32];
            e[0] = 0xC1;
            for (k, u) in chunk.iter().enumerate() {
                e[2 + 2 * k..4 + 2 * k].copy_from_slice(&u.to_le_bytes());
            }
            out.push(e);
        }
        out
    }

    /// A reader over `slots`, laid out in `dir`'s clusters, that logs each read's offset and length.
    fn slot_disk<'a>(
        dir: &'a ExfatDirSlots,
        slots: &'a [[u8; 32]],
        reads: &'a RefCell<Vec<(u64, usize)>>,
    ) -> impl FnMut(u64, &mut [u8]) -> io::Result<()> + 'a {
        move |at, buf| {
            reads.borrow_mut().push((at, buf.len()));
            for (k, b) in buf.chunks_exact_mut(32).enumerate() {
                let slot = dir
                    .slot_at(at + k as u64 * 32)
                    .expect("a read inside the directory");
                b.copy_from_slice(slots.get(slot as usize).unwrap_or(&[0; 32]));
            }
            Ok(())
        }
    }

    /// Three 4-slot clusters, far apart and out of disk order.
    fn three_small_clusters() -> ExfatDirSlots {
        ExfatDirSlots {
            cluster_offsets: vec![0x1000, 0x8000, 0x3000],
            slots_per_cluster: 4,
        }
    }

    /// An entry set is found by its on-disk name and stream fields, through the chain, and only
    /// among the directory's own in-use slots. Nothing depends on hadris's `entry_offset`.
    #[test]
    fn exfat_entry_set_is_found_by_its_on_disk_fields() {
        let dir = three_small_clusters();
        let mut slots = Vec::new();
        slots.extend(raw_entry_set("game.z64", 5, 10, false)); // 0: same name, other file
        slots.extend(raw_entry_set("game.z64", 9, 10, false)); // 3: straddles clusters 0 and 1
        let mut deleted = raw_entry_set("save.eep", 7, 4, false);
        deleted[0][0] = 0x05;
        slots.extend(deleted); // 6: a deleted set with the same fields as the next one
        slots.extend(raw_entry_set("save.eep", 7, 4, false)); // 9
        let find = |slots: &[[u8; 32]], name, first_cluster, len, is_dir| {
            let reads = RefCell::new(Vec::new());
            let want = ExfatEntryIdentity {
                name,
                is_directory: is_dir,
                first_cluster,
                valid_data_length: len,
                data_length: len,
            };
            let found = exfat_find_entry_set(&dir, &want, slot_disk(&dir, slots, &reads)).unwrap();
            (found, reads.into_inner().len())
        };

        assert_eq!(find(&slots, "game.z64", 9, 10, false), (Some(3), 2));
        assert_eq!(find(&slots, "save.eep", 7, 4, false).0, Some(9));
        assert_eq!(find(&slots, "GAME.Z64", 9, 10, false).0, None, "exact name");
        assert_eq!(find(&slots, "game.z64", 9, 10, true).0, None, "a folder");
        assert_eq!(
            find(&slots, "game.z64", 9, 11, false).0,
            None,
            "another length"
        );

        // Nothing after the end-of-directory marker counts.
        let mut ended = slots.clone();
        ended[3][0] = 0x00;
        assert_eq!(find(&ended, "save.eep", 7, 4, false).0, None);

        // A set whose secondaries would run past the directory's last slot is not the directory's.
        let mut cut = slots[..9].to_vec();
        cut.push([0; 32]);
        cut.extend(&raw_entry_set("x", 2, 1, false)[..2]); // 10, 11: needs slot 12
        cut[10][1] = 2;
        assert_eq!(find(&cut, "x", 2, 1, false).0, None);
    }

    /// The free-slot scan reads a chunk at a time, not a slot at a time, and still treats the
    /// entry being moved as taken.
    #[test]
    fn exfat_free_slot_scan_reads_each_chunk_once() {
        const U: u8 = 0x85;
        let dir = three_small_clusters();
        let slots: Vec<[u8; 32]> = [U, U, U, U, U, U, 0x05, 0x40, 0x00, U, 0x00, 0x00]
            .iter()
            .map(|&t| {
                let mut s = [0u8; 32];
                s[0] = t;
                s
            })
            .collect();

        let reads = RefCell::new(Vec::new());
        let run = exfat_find_free_entry_run(&dir, 3, 0..0, slot_disk(&dir, &slots, &reads));
        assert_eq!(run.unwrap(), 6);
        assert_eq!(
            reads.into_inner(),
            [(0x1000, 128), (0x8000, 128), (0x3000, 128)],
            "one read per chunk, following the chain"
        );

        let reads = RefCell::new(Vec::new());
        let err = exfat_find_free_entry_run(&dir, 3, 6..7, slot_disk(&dir, &slots, &reads))
            .expect_err("the moved entry's own slot is not free");
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    }

    /// Reading an entry set to rewrite it (delete, rename, size fix) costs one disk read per chunk
    /// it touches, not one per slot: on an SC64 each read is a USB round trip.
    #[test]
    fn exfat_entry_set_reads_each_chunk_once() {
        let dir = three_small_clusters();
        let mut slots = Vec::new();
        slots.extend(raw_entry_set("a.z64", 5, 10, false)); // 0..=2: inside cluster 0
        slots.extend(raw_entry_set("b.z64", 6, 10, false)); // 3..=5: straddles clusters 0 and 1
        let mut bad = [0u8; 32];
        bad[0] = 0x85;
        bad[1] = 1; // 6: too few secondaries
        slots.push(bad);
        let read = |slots: &[[u8; 32]], primary| {
            let reads = RefCell::new(Vec::new());
            let set = exfat_read_entry_set(&dir, primary, slot_disk(&dir, slots, &reads));
            (set, reads.into_inner())
        };

        let (set, reads) = read(&slots, 0);
        assert_eq!(set.unwrap().unwrap(), slots[0..3]);
        assert_eq!(reads, [(0x1000, 128)], "one read for the whole set");

        let (set, reads) = read(&slots, 3);
        assert_eq!(set.unwrap().unwrap(), slots[3..6]);
        assert_eq!(reads, [(0x1000, 128), (0x8000, 128)], "one per cluster");

        let (set, reads) = read(&slots, 6);
        assert_eq!(set.unwrap(), None);
        assert_eq!(reads.len(), 1);

        // A set whose secondaries would run past the directory's last slot.
        let mut end = vec![[0u8; 32]; 11];
        end.push(raw_entry_set("c.z64", 7, 1, false)[0]);
        assert!(read(&end, 11).0.is_err());
    }

    /// In a large cluster a chunk is 256 slots (16 sectors), and never crosses into the next
    /// cluster.
    #[test]
    fn exfat_slot_reader_chunks_stay_inside_a_cluster() {
        let dir = ExfatDirSlots {
            cluster_offsets: vec![0x10_0000, 0x40_0000],
            slots_per_cluster: 512,
        };
        let reads = RefCell::new(Vec::new());
        let mut r = ExfatSlotReader::new(&dir, slot_disk(&dir, &[], &reads));
        for slot in [300, 301, 511, 512, 1023] {
            r.slot(slot).unwrap();
        }
        assert!(r.slot(1024).is_err(), "past the directory's last slot");
        drop(r);
        assert_eq!(
            reads.into_inner(),
            [
                (0x10_0000 + 256 * 32, 8192),
                (0x40_0000, 8192),
                (0x40_0000 + 256 * 32, 8192)
            ]
        );
    }

    /// A case-only rename, and deletes in the root and in a folder, all locate their entry sets
    /// on a real volume.
    #[test]
    fn exfat_rename_and_delete_locate_entry_sets_on_disk() {
        let (arc, vol, part_bytes) = exfat_image(&ExFatFormatOptions::new());
        for path in ["a.z64", "b.z64", "Saves/game.sav"] {
            write_file_exfat_streaming(
                &vol,
                0,
                part_bytes,
                path,
                4,
                &mut Cursor::new(&b"data"[..]),
                &mut |_| true,
            )
            .unwrap();
        }
        let names = |dir: &str| -> Vec<String> {
            let mut n: Vec<_> =
                list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), dir)
                    .unwrap()
                    .into_iter()
                    .map(|e| e.name)
                    .collect();
            n.sort();
            n
        };

        rename_cart_exfat(&vol, 0, part_bytes, "a.z64", "A.Z64").expect("case-only rename");
        assert_eq!(names("/"), ["A.Z64", "Saves", "b.z64"]);

        let mut quiet = |_: &str| {};
        remove_cart_path_exfat_unified(vol.clone(), 0, part_bytes, "Saves/game.sav", &mut quiet)
            .expect("delete in a folder");
        assert!(names("/Saves").is_empty());
        remove_cart_path_exfat_unified(vol.clone(), 0, part_bytes, "b.z64", &mut quiet)
            .expect("delete in the root");
        assert_eq!(names("/"), ["A.Z64", "Saves"]);
        let back = read_file_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "A.Z64");
        assert_eq!(back.unwrap(), b"data");
    }

    /// #130: a card formatted without a partition table has boot code where an MBR keeps its
    /// first partition entry.
    #[test]
    fn detect_partition_superfloppy_boot_code_is_not_a_partition_entry() {
        let fat = fat_image();
        let (exfat, _, _) = exfat_image(&ExFatFormatOptions::new());
        for (label, image) in [("FAT", fat), ("exFAT", exfat)] {
            let mut s0 = image.lock().unwrap()[..512].to_vec();
            assert_eq!(&s0[510..], &[0x55, 0xAA], "{label}");
            s0[0x1BE..0x1CE].copy_from_slice(&[0xF4; 16]);
            let start =
                detect_partition_start(&s0, |_, _| panic!("{label}: no GPT here")).expect("detect");
            assert_eq!(start, 0, "{label}");
            assert!(partition_volume_bytes(&s0).unwrap() > 0, "{label}");
        }
    }

    #[test]
    fn detect_partition_ignores_an_empty_mbr_entry() {
        let mut s0 = [0u8; 512];
        s0[0x1C6..0x1CA].copy_from_slice(&63u32.to_le_bytes());
        s0[510] = 0x55;
        s0[511] = 0xAA;
        assert_eq!(detect_partition_start(&s0, |_, _| Ok(())).unwrap(), 0);
    }

    #[test]
    fn ram_disk_fat32_list_read_write_roundtrip() {
        let arc = Arc::new(std::sync::Mutex::new(vec![0u8; FAT32_IMAGE_BYTES]));
        {
            let mut d = RamPartitionDisk::new_writable(Arc::clone(&arc));
            fatfs::format_volume(&mut d, FormatVolumeOptions::new()).expect("format FAT32");
        }

        let entries =
            list_dir_fat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/").expect("list");
        assert!(!entries.iter().any(|e| e.name == "hello.txt"));

        write_file_fat_streaming_impl(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            "hello.txt",
            &mut Cursor::new(&b"roundtrip payload"[..]),
            &mut |_| true,
        )
        .expect("write");

        let entries =
            list_dir_fat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/").expect("list2");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello.txt"));

        let bytes = read_file_fat(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "hello.txt",
        )
        .expect("read");
        assert_eq!(bytes, b"roundtrip payload");
    }

    /// The streaming read must be safe against a sink that writes short or gives up: partial
    /// writes must not drop bytes, and a sink that refuses more must surface as an error rather
    /// than a silently truncated file. Today's sink is a `BufWriter<File>`, which does neither —
    /// but the export path's correctness should not rest on that.
    #[test]
    fn fat_streaming_read_survives_a_short_writing_sink() {
        /// Accepts at most 7 bytes per call, and stops accepting after `limit` bytes in total.
        struct ShortSink {
            got: Vec<u8>,
            limit: usize,
        }
        impl Write for ShortSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.got.len() >= self.limit {
                    return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "full"));
                }
                let n = buf.len().min(7).min(self.limit - self.got.len());
                self.got.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let arc = Arc::new(std::sync::Mutex::new(vec![0u8; FAT32_IMAGE_BYTES]));
        {
            let mut d = RamPartitionDisk::new_writable(Arc::clone(&arc));
            fatfs::format_volume(&mut d, FormatVolumeOptions::new()).expect("format FAT32");
        }
        // Larger than one write() so the short-write loop actually runs.
        let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        write_file_fat_streaming_impl(
            RamPartitionDisk::new_writable(Arc::clone(&arc)),
            "rom.z64",
            &mut Cursor::new(&payload[..]),
            &mut |_| true,
        )
        .expect("write");

        let mut sink = ShortSink {
            got: Vec::new(),
            limit: usize::MAX,
        };
        super::read_file_fat_streaming(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "rom.z64",
            &mut sink,
            &mut |_| true,
            Some(payload.len() as u64),
        )
        .expect("stream");
        assert_eq!(sink.got, payload, "short writes must not lose bytes");

        // A sink that stops accepting is an error, not a silent truncation.
        let mut stubborn = ShortSink {
            got: Vec::new(),
            limit: 100,
        };
        let err = super::read_file_fat_streaming(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "rom.z64",
            &mut stubborn,
            &mut |_| true,
            Some(payload.len() as u64),
        )
        .expect_err("a sink that refuses more bytes must fail the stream");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::WriteZero | std::io::ErrorKind::Interrupted
        ));

        // Cancelling from the progress callback is how a released stream aborts the reader.
        let mut cancelled = ShortSink {
            got: Vec::new(),
            limit: usize::MAX,
        };
        let err = super::read_file_fat_streaming(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "rom.z64",
            &mut cancelled,
            &mut |_| false,
            Some(payload.len() as u64),
        )
        .expect_err("cancelling must not report success");
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn ram_disk_exfat_list_read_after_hadris_write() {
        let arc = Arc::new(std::sync::Mutex::new(vec![0u8; EXFAT_IMAGE_BYTES]));
        let volume_bytes = EXFAT_IMAGE_BYTES as u64;
        {
            let disk = RamPartitionDisk::new_writable(Arc::clone(&arc));
            let opts = ExFatFormatOptions::new();
            let fs = format_exfat(disk, volume_bytes, &opts).expect("format exFAT");
            let root = fs.root_dir();
            let entry = fs.create_file(&root, "hello.txt").expect("create");
            let mut w = fs.write_file(&entry).expect("write handle");
            w.write_all(b"exfat hello").expect("write bytes");
            w.finish().expect("finish");
        }

        let entries =
            list_dir_exfat(RamPartitionDisk::new_readonly(Arc::clone(&arc)), "/").expect("list");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello.txt"));

        let bytes = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "hello.txt",
        )
        .expect("read");
        assert_eq!(bytes, b"exfat hello");
    }

    #[test]
    fn ram_disk_exfat_write_streaming_roundtrip() {
        let arc = Arc::new(std::sync::Mutex::new(vec![0u8; EXFAT_IMAGE_BYTES]));
        let part_bytes = EXFAT_IMAGE_BYTES as u64;
        let vol = ExfatVolumeSource::Ram {
            buf: Arc::clone(&arc),
        };
        {
            let disk = RamPartitionDisk::new_writable(Arc::clone(&arc));
            let opts = ExFatFormatOptions::new();
            format_exfat(disk, part_bytes, &opts).expect("format exFAT");
        }

        write_file_exfat_streaming(
            &vol,
            0,
            part_bytes,
            "stream.txt",
            b"streaming roundtrip".len() as u64,
            &mut Cursor::new(&b"streaming roundtrip"[..]),
            &mut |_| true,
        )
        .expect("write stream");

        let bytes = read_file_exfat(
            RamPartitionDisk::new_readonly(Arc::clone(&arc)),
            "stream.txt",
        )
        .expect("read");
        assert_eq!(bytes, b"streaming roundtrip");
    }

    #[test]
    fn detect_partition_legacy_mbr_first_lba() {
        let mut s0 = [0u8; 512];
        s0[0x1C2] = 0x0C;
        s0[0x1C6..0x1CA].copy_from_slice(&63u32.to_le_bytes());
        s0[510] = 0x55;
        s0[511] = 0xAA;
        let r = detect_partition_start(&s0, |_, _| Ok(())).expect("detect");
        assert_eq!(r, 63);
    }

    #[test]
    fn detect_partition_gpt_first_partition_lba() {
        let mut img = vec![0u8; 512 * 4];
        img[0x1C2] = 0xEE;
        img[0x1C6..0x1CA].copy_from_slice(&1u32.to_le_bytes());
        img[510] = 0x55;
        img[511] = 0xAA;
        let s1 = &mut img[512..1024];
        s1[0..8].copy_from_slice(b"EFI PART");
        s1[0x48..0x50].copy_from_slice(&2u64.to_le_bytes());
        s1[0x50..0x54].copy_from_slice(&128u32.to_le_bytes());
        s1[0x54..0x58].copy_from_slice(&128u32.to_le_bytes());
        let s2 = &mut img[1024..1536];
        s2[0x20..0x28].copy_from_slice(&8888u64.to_le_bytes());

        let s0 = &img[..512];
        let img_ref = img.clone();
        let r = detect_partition_start(s0, |lba, buf| {
            let off = (lba as usize) * 512;
            if off + 512 <= img_ref.len() {
                buf.copy_from_slice(&img_ref[off..off + 512]);
            } else {
                buf.fill(0);
            }
            Ok(())
        })
        .expect("detect");
        assert_eq!(r, 8888);
    }

    /// Stable SHA-256 of the first 512 bytes after `fatfs::format_volume` with a fixed volume ID
    /// (guards against accidental boot-sector layout drift).
    #[test]
    fn fat32_boot_sector_sha256_golden() {
        let arc = Arc::new(std::sync::Mutex::new(vec![0u8; FAT32_IMAGE_BYTES]));
        {
            let mut d = RamPartitionDisk::new_writable(Arc::clone(&arc));
            fatfs::format_volume(&mut d, FormatVolumeOptions::new().volume_id(0xABCDEF01))
                .expect("format FAT32");
        }
        let g = arc.lock().unwrap();
        let digest = Sha256::digest(&g[..512]);
        let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex.as_str(),
            "6ce18c8f7c6734f451389e4b134c84c3ea87aa98e89495b8d457b120d2ac10a9"
        );
    }
}
