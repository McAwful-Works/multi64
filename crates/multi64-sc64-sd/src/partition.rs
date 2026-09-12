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
    ExFatDir, ExFatFileEntry, ExFatFs, FileAttributes as ExFatFileAttributes,
    RawFileDirectoryEntry, RawFileNameEntry, RawStreamExtensionEntry,
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
const EXFAT_ENTRY_END_OR_FREE: u8 = 0x00;
const EXFAT_ENTRY_DELETED_FILE: u8 = 0x05;
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

    /// Read an entire file from the SD (FAT or exFAT).
    pub fn read_file_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        let disk = Sc64PartitionDisk::new(
            self.link.clone(),
            self.partition_start_sector,
            self.partition_bytes,
        );
        if self.exfat {
            read_file_exfat(PartitionDiskUnion::Sc64(disk), path)
        } else {
            read_file_fat(disk, path)
        }
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

    /// Copy a file or directory from the SD to a host path (export).
    pub fn copy_cart_entry_to_host(&self, cart_path: &str, dest: &Path) -> io::Result<()> {
        self.copy_cart_entry_to_host_with_progress(cart_path, dest, false, |_| true)
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

    /// Copy a file or directory from the PC into a folder on the SD (FAT or exFAT).
    pub fn import_from_pc(&self, src: &Path, cart_parent: &str, dest_name: &str) -> io::Result<()> {
        self.import_from_pc_with_progress(src, cart_parent, dest_name, false, |_| true)
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
            self.write_cart_file_streaming(&cart_root, &mut reader, &mut progress)?;
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
        let fs = FileSystem::new(disk, FsOptions::new())?;
        let normalized = path.trim().replace('\\', "/");
        let parts: Vec<&str> = normalized
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let mut dir = fs.root_dir();
        for p in parts {
            dir = match dir.open_dir(p) {
                Ok(d) => d,
                Err(_) => dir.create_dir(p)?,
            };
        }
        Ok(())
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

    fn write_cart_file_streaming(
        &self,
        rel_path: &str,
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
                data,
                progress,
            )
        } else {
            self.write_file_fat_streaming(rel_path, data, progress)
        };
        if let Err(e) = r {
            if e.kind() == io::ErrorKind::Interrupted {
                let _ = self.remove_cart_path(rel_path);
            }
            return Err(e);
        }
        Ok(())
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

    pub fn read_file_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        let disk = SectorPartitionDisk::new(
            self.link.clone(),
            self.partition_start_sector,
            self.partition_bytes,
        );
        if self.exfat {
            read_file_exfat(PartitionDiskUnion::Ed64(disk), path)
        } else {
            read_file_fat(disk, path)
        }
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

    pub fn copy_cart_entry_to_host(&self, cart_path: &str, dest: &Path) -> io::Result<()> {
        self.copy_cart_entry_to_host_with_progress(cart_path, dest, false, |_| true)
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

    pub fn import_from_pc(
        &self,
        _src: &Path,
        _cart_parent: &str,
        _dest_name: &str,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive linear ROM session is read-only from the PC in this build.",
        ))
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
            "EverDrive linear ROM session is read-only from the PC in this build.",
        ))
    }

    pub fn remove_cart_path(&self, _path: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive linear ROM session is read-only from the PC in this build.",
        ))
    }

    pub fn remove_cart_path_traced(
        &self,
        _path: &str,
        _trace: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive linear ROM session is read-only from the PC in this build.",
        ))
    }

    pub fn mkdir_cart(&self, _path: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive linear ROM session is read-only from the PC in this build.",
        ))
    }

    pub fn rename_cart(&self, _from: &str, _to: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "EverDrive linear ROM session is read-only from the PC in this build.",
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
    let mut dir = fs.root_dir();
    for p in parents {
        dir = match dir.open_dir(p) {
            Ok(d) => d,
            Err(_) => dir.create_dir(p)?,
        };
    }
    if dir.open_file(name).is_ok() {
        dir.remove(name)?;
    }
    let mut f = dir.create_file(name)?;
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
    Ok(())
}

fn write_file_exfat_streaming(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    rel_path: &str,
    data: &mut impl Read,
    progress: &mut impl FnMut(u64) -> bool,
) -> io::Result<()> {
    let disk = vol.partition_disk_rw(part_start, part_bytes);
    let fs = ExFatFs::open(disk).map_err(exfat_err)?;
    let parts: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    let (name, parents) = parts.split_last().unwrap();
    let mut dir = fs.root_dir();
    let mut path_prefix = String::new();
    for p in parents {
        let next_path = if path_prefix.is_empty() {
            p.to_string()
        } else {
            format!("{path_prefix}/{p}")
        };
        dir = match dir.open_dir(p) {
            Ok(d) => {
                path_prefix = next_path;
                d
            }
            Err(_) => {
                let new_dir = fs.create_dir(&dir, p).map_err(exfat_err)?;
                exfat_zero_fat_for_nofatchain_entry(&fs, vol, part_start, part_bytes, &next_path)?;
                path_prefix = next_path;
                new_dir
            }
        };
    }
    if let Some(old) = dir.find(name).map_err(exfat_err)? {
        if old.is_directory() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a directory exists with this name",
            ));
        }
        let mut trace_quiet = |_s: &str| {};
        exfat_delete_entry_resolved(
            &fs,
            rel_path,
            &old,
            vol,
            part_start,
            part_bytes,
            &mut trace_quiet,
        )?;
    }
    let entry = fs.create_file(&dir, name).map_err(exfat_err)?;
    let mut w = fs.write_file(&entry).map_err(exfat_err)?;
    let mut buf = vec![0u8; STREAM_CHUNK];
    loop {
        let n = data.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(exfat_err)?;
        if !progress(n as u64) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
    }
    w.finish().map_err(exfat_err)?;
    let entry_after = fs.open_path(rel_path).map_err(exfat_err)?;
    exfat_sync_stream_data_length_to_valid(
        &fs,
        rel_path,
        &entry_after,
        vol,
        part_start,
        part_bytes,
    )?;
    let info = fs.info().clone();
    let entry_final = fs.open_path(rel_path).map_err(exfat_err)?;
    let (need_fat_zero, first_c, count_c) = if entry_final.no_fat_chain
        && entry_final.first_cluster >= 2
        && !entry_final.is_directory()
    {
        let cs = info.bytes_per_cluster as u64;
        let stream_len = entry_final.data_length.max(entry_final.valid_data_length);
        let mut n = ((stream_len + cs - 1) / cs) as u32;
        if n == 0 {
            n = 1;
        }
        (true, entry_final.first_cluster, n)
    } else {
        (false, 0, 0)
    };
    drop(fs);
    if need_fat_zero
        && exfat_should_zero_fat_for_nofatchain_range(
            vol, part_start, part_bytes, &info, first_c, count_c,
        )?
    {
        exfat_clear_fat_contiguous_range(vol, part_start, part_bytes, &info, first_c, count_c)?;
    }
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

/// Match a list_dir [`SessionEntry::name`] against the last segment of a cart path.
/// exFAT is case-insensitive (see hadris `ExFatDir::find`); FAT is often treated as case-insensitive on Windows.
fn cart_entry_name_matches(list_name: &str, path_last_component: &str) -> bool {
    list_name == path_last_component || list_name.eq_ignore_ascii_case(path_last_component)
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
            session.write_cart_file_streaming(&cart_sub, &mut reader, progress)?;
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

/// hadris [`hadris_fat::exfat::ExFatFileEntry::entry_offset`] is **bytes from the start of the
/// parent directory stream**, not a volume byte offset (`hadris` `exfat/dir.rs` sets
/// `entry_offset = dir_offset - entry_size`). We therefore try each aligned `u64` in the struct as:
/// 1) a **volume-absolute** byte offset, or 2) a **directory-relative** offset converted via
/// parent directory cluster chain (see [`exfat_dir_rel_to_volume_abs`]).
///
/// Call while the primary is still `0x85` on disk (before marking deleted).
///
/// # Safety
///
/// `ExFatFileEntry` is read as raw bytes for the duration of this function only.
fn exfat_entry_offset_via_disk_probe(
    fs: &ExFatFs<PartitionDiskUnion>,
    entry_path: &str,
    entry: &hadris_fat::exfat::ExFatFileEntry,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<u64> {
    const FILE_DIRECTORY: u8 = 0x85;
    const DELETED: u8 = 0x05;
    const STREAM_EXT: u8 = 0xC0;

    let (parent_path, _) = exfat_parent_path_and_name(entry_path);
    let (first_cluster, is_contiguous, _dir_size) =
        exfat_parent_dir_metadata_from_fs(fs, parent_path)?;
    let info = fs.info();

    let sz = std::mem::size_of::<hadris_fat::exfat::ExFatFileEntry>();
    let bytes = unsafe {
        std::slice::from_raw_parts(
            entry as *const hadris_fat::exfat::ExFatFileEntry as *const u8,
            sz,
        )
    };
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);

    let mut try_abs = |abs: u64| -> bool {
        if abs >= part_bytes || abs.saturating_add(64) > part_bytes {
            return false;
        }
        if disk.seek(SeekFrom::Start(abs)).is_err() {
            return false;
        }
        let mut primary = [0u8; 32];
        if disk.read_exact(&mut primary).is_err() {
            return false;
        }
        if primary[0] != FILE_DIRECTORY && primary[0] != DELETED {
            return false;
        }
        let sec = primary[1] as usize;
        if !(2..=18).contains(&sec) {
            return false;
        }
        if disk.seek(SeekFrom::Start(abs + 32)).is_err() {
            return false;
        }
        let mut stream = [0u8; 32];
        if disk.read_exact(&mut stream).is_err() {
            return false;
        }
        stream[0] == STREAM_EXT
    };

    for i in (0..=sz.saturating_sub(8)).step_by(8) {
        let cand = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        if try_abs(cand) {
            return Ok(cand);
        }
        if let Some(abs) = exfat_dir_rel_to_volume_abs(
            info,
            first_cluster,
            is_contiguous,
            cand,
            vol,
            part_start,
            part_bytes,
        )? {
            if try_abs(abs) {
                return Ok(abs);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "could not locate exFAT directory entry set offset (relative vs absolute)",
    ))
}

fn exfat_parent_path_and_name(path: &str) -> (&str, &str) {
    let path = path.trim().trim_start_matches('/');
    match path.rsplit_once('/') {
        None => ("", path),
        Some((p, n)) => (p, n),
    }
}

/// Parent directory stream: first cluster, contiguous flag, and allocated size (0 = unknown).
fn exfat_parent_dir_metadata_from_fs(
    fs: &ExFatFs<PartitionDiskUnion>,
    parent_path: &str,
) -> io::Result<(u32, bool, u64)> {
    if parent_path.is_empty() {
        let info = fs.info();
        return Ok((info.root_cluster, true, 0));
    }
    let e = fs.open_path(parent_path).map_err(exfat_err)?;
    if !e.is_directory() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exFAT parent path is not a directory",
        ));
    }
    Ok((e.first_cluster, e.no_fat_chain, e.data_length))
}

/// Map hadris directory-stream byte offset to volume byte offset (cluster heap).
fn exfat_dir_rel_to_volume_abs(
    info: &hadris_fat::exfat::ExFatInfo,
    first_cluster: u32,
    is_contiguous: bool,
    rel: u64,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<Option<u64>> {
    let cs = info.bytes_per_cluster as u64;
    if cs == 0 || !info.is_valid_cluster(first_cluster) {
        return Ok(None);
    }
    let mut remain = rel;
    let mut c = first_cluster;
    let mut disk = vol.partition_disk_ro(part_start, part_bytes);
    while remain >= cs {
        remain -= cs;
        if is_contiguous {
            c = match c.checked_add(1) {
                Some(n) => n,
                None => return Ok(None),
            };
            if !info.is_valid_cluster(c) {
                return Ok(None);
            }
        } else {
            let next = exfat_read_fat_entry_inner(&mut disk, info, c)?;
            if next == 0 || next >= 0xFFFFFFF8 {
                return Ok(None);
            }
            if !info.is_valid_cluster(next) {
                return Ok(None);
            }
            c = next;
        }
    }
    Ok(Some(info.cluster_to_offset(c) + remain))
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

/// Zero FAT entries for a NoFatChain stream after hadris left EOC in the FAT (see
/// [`exfat_should_zero_fat_for_nofatchain_range`]). Skips if the FAT already looks like a real
/// fragment chain so we do not corrupt a file that fell back to FAT-linked clusters.
fn exfat_zero_fat_for_nofatchain_entry(
    fs: &ExFatFs<PartitionDiskUnion>,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    path: &str,
) -> io::Result<()> {
    let info = fs.info().clone();
    let e = fs.open_path(path).map_err(exfat_err)?;
    if !e.no_fat_chain || e.first_cluster < 2 {
        return Ok(());
    }
    let cs = info.bytes_per_cluster as u64;
    let stream_len = e.data_length.max(e.valid_data_length);
    let mut n = ((stream_len + cs - 1) / cs) as u32;
    if n == 0 {
        n = 1;
    }
    if !exfat_should_zero_fat_for_nofatchain_range(
        vol,
        part_start,
        part_bytes,
        &info,
        e.first_cluster,
        n,
    )? {
        return Ok(());
    }
    exfat_clear_fat_contiguous_range(vol, part_start, part_bytes, &info, e.first_cluster, n)
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
    const ENTRY_BYTES: u64 = 4;
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    for i in 0..cluster_count {
        let cluster = first_cluster.checked_add(i).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "exFAT FAT cluster overflow")
        })?;
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
            disk.write_all(&FREE.to_le_bytes())?;
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

/// hadris [`hadris_fat::exfat::ExFatFileWriter::finish`] calls [`hadris_fat::exfat::ExFatFs::update_entry_size`]
/// with **`data_length` = cluster-allocated size** and **`valid_data_length` = EOF**. Many hosts (Windows
/// Explorer) display stream **`DataLength`** as the file size, so the on-card file looks larger than
/// the source. For normal files, both lengths must match the logical size.
fn exfat_sync_stream_data_length_to_valid(
    fs: &ExFatFs<PartitionDiskUnion>,
    path: &str,
    entry: &hadris_fat::exfat::ExFatFileEntry,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<()> {
    const STREAM_EXT: u8 = 0xC0;

    let entry_off =
        exfat_entry_offset_via_disk_probe(fs, path, entry, vol, part_start, part_bytes)?;
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    disk.seek(SeekFrom::Start(entry_off))?;
    let mut primary = [0u8; 32];
    disk.read_exact(&mut primary)?;
    let secondary_count = primary[1] as usize;
    if secondary_count < 2 || secondary_count > 18 {
        return Ok(());
    }
    let total = 1 + secondary_count;
    let need = (total as u64).saturating_mul(32);
    if entry_off.saturating_add(need) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT entry set extends past partition",
        ));
    }
    let mut entries = vec![[0u8; 32]; total];
    entries[0] = primary;
    for i in 1..total {
        disk.seek(SeekFrom::Start(entry_off + (i as u64 * 32)))?;
        disk.read_exact(&mut entries[i])?;
    }
    if entries[1][0] != STREAM_EXT {
        return Ok(());
    }
    let valid = u64::from_le_bytes(entries[1][8..16].try_into().unwrap());
    let data = u64::from_le_bytes(entries[1][24..32].try_into().unwrap());
    if data == valid {
        return Ok(());
    }
    let valid_bytes: [u8; 8] = entries[1][8..16].try_into().unwrap();
    entries[1][24..32].copy_from_slice(&valid_bytes);
    let checksum = exfat_compute_entry_set_checksum(&entries);
    entries[0][2] = (checksum & 0xff) as u8;
    entries[0][3] = (checksum >> 8) as u8;
    for (i, slab) in entries.iter().enumerate() {
        disk.seek(SeekFrom::Start(entry_off + (i as u64 * 32)))?;
        disk.write_all(slab)?;
    }
    disk.flush()?;
    Ok(())
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
    entry_offset: u64,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    const DELETED_FILE: u8 = 0x05;

    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    disk.seek(SeekFrom::Start(entry_offset))?;
    let mut primary = [0u8; 32];
    disk.read_exact(&mut primary)?;
    let secondary_count = primary[1] as usize;
    if secondary_count < 2 || secondary_count > 18 {
        trace("exFAT: skip finalize delete (unexpected secondary_count)");
        return Ok(());
    }
    let total = 1 + secondary_count;
    let need = (total as u64).saturating_mul(32);
    if entry_offset.saturating_add(need) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT entry set extends past partition",
        ));
    }
    let mut entries = vec![[0u8; 32]; total];
    entries[0] = primary;
    for i in 1..total {
        disk.seek(SeekFrom::Start(entry_offset + (i as u64 * 32)))?;
        disk.read_exact(&mut entries[i])?;
    }

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
    for (i, slab) in entries.iter().enumerate() {
        disk.seek(SeekFrom::Start(entry_offset + (i as u64 * 32)))?;
        disk.write_all(slab)?;
    }
    disk.flush()?;
    trace("exFAT: finalized deleted entry set (stream zeroed, checksum, full write)");
    Ok(())
}

/// hadris `ExFatFs::delete` passes [`hadris_fat::exfat::ExFatFileEntry::entry_offset`] to
/// `write_at`, but entries from directory iteration store **directory-stream-relative** offsets
/// (`exfat/dir.rs`), while `create_file` sets **volume-absolute** offsets (`fs.rs`). Deletes of
/// files opened via `open_path` therefore write `0x05` to the wrong byte and never remove the
/// listing entry. We resolve the real volume offset with [`exfat_entry_offset_via_disk_probe`],
/// then free clusters, mark deleted, fix checksum, and sync the bitmap — matching hadris intent.
fn exfat_delete_entry_resolved(
    fs: &ExFatFs<PartitionDiskUnion>,
    entry_path: &str,
    entry: &hadris_fat::exfat::ExFatFileEntry,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    trace: &mut dyn FnMut(&str),
) -> io::Result<()> {
    if entry.is_directory() {
        let dir = fs.open_dir(entry_path).map_err(exfat_err)?;
        if let Some(item) = dir.entries().next() {
            item.map_err(exfat_err)?;
            return Err(io::Error::other("exFAT directory is not empty"));
        }
    }

    trace("exFAT: resolve entry set volume offset…");
    let entry_off =
        exfat_entry_offset_via_disk_probe(fs, entry_path, entry, vol, part_start, part_bytes)?;

    let info = fs.info();
    let cs = info.bytes_per_cluster as u64;
    if entry.first_cluster >= 2 {
        trace("exFAT: free file/directory clusters…");
        let cluster_count = if entry.no_fat_chain {
            let stream_len = entry.data_length.max(entry.valid_data_length);
            let mut n = ((stream_len + cs - 1) / cs) as u32;
            if n == 0 {
                n = 1;
            }
            n
        } else {
            0
        };
        fs.free_clusters(entry.first_cluster, cluster_count, entry.no_fat_chain)
            .map_err(exfat_err)?;
        trace("exFAT: free clusters OK");
        if entry.no_fat_chain && cluster_count > 0 {
            trace("exFAT: clear FAT for contiguous run…");
            exfat_clear_fat_contiguous_range(
                vol,
                part_start,
                part_bytes,
                info,
                entry.first_cluster,
                cluster_count,
            )?;
            trace("exFAT: contiguous FAT cleared OK");
        }
    }

    trace("exFAT: finalize deleted entry set (0x05, zero stream, checksum)…");
    exfat_finalize_deleted_entry_set(vol, part_start, part_bytes, entry_off, trace)?;
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
    let mut dir = fs.root_dir();
    let mut path_prefix = String::new();
    for p in parts {
        let next_path = if path_prefix.is_empty() {
            p.to_string()
        } else {
            format!("{path_prefix}/{p}")
        };
        dir = match dir.open_dir(p) {
            Ok(d) => {
                path_prefix = next_path;
                d
            }
            Err(_) => {
                let new_dir = fs.create_dir(&dir, p).map_err(exfat_err)?;
                exfat_zero_fat_for_nofatchain_entry(&fs, vol, part_start, part_bytes, &next_path)?;
                path_prefix = next_path;
                new_dir
            }
        };
    }
    fs.sync_bitmap().map_err(exfat_err)?;
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
    let fs = FileSystem::new(disk, FsOptions::new())?;
    let mut dir = fs.root_dir();
    let normalized = parent_path.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    for p in parts {
        dir = dir.open_dir(p)?;
    }
    dir.rename(name_from, &dir, name_to)?;
    Ok(())
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
    let old = fs.open_path(from_rel).map_err(exfat_err)?;
    let parent_dir = if parent_path.is_empty() {
        fs.root_dir()
    } else {
        fs.open_dir(&parent_path).map_err(exfat_err)?
    };
    let old_off =
        exfat_entry_offset_via_disk_probe(&fs, from_rel, &old, vol, part_start, part_bytes)?;
    let to_path = if parent_path.is_empty() {
        name_to.clone()
    } else {
        format!("{parent_path}/{name_to}")
    };
    if let Some(candidate) = parent_dir.find(&name_to).map_err(exfat_err)? {
        let cand_off = exfat_entry_offset_via_disk_probe(
            &fs, &to_path, &candidate, vol, part_start, part_bytes,
        )?;
        if cand_off != old_off {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a file or folder already exists with that name",
            ));
        }
    }
    exfat_validate_rename_name(&name_to)?;
    let new_slabs = exfat_build_rename_entry_set_bytes(&fs, &old, &name_to)?;
    let new_total = new_slabs.len();
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    disk.seek(SeekFrom::Start(old_off))?;
    let mut primary = [0u8; 32];
    disk.read_exact(&mut primary)?;
    let secondary_count = primary[1] as usize;
    if secondary_count < 2 || secondary_count > 18 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid exFAT secondary_count on disk",
        ));
    }
    let old_total = 1 + secondary_count;
    let info = fs.info().clone();
    let (dir_first, dir_contig, dir_size) = exfat_parent_dir_metadata_from_fs(&fs, &parent_path)?;
    if new_total == old_total {
        exfat_write_entry_slabs_at(vol, part_start, part_bytes, old_off, &new_slabs)?;
    } else if new_total < old_total {
        exfat_write_entry_slabs_at(vol, part_start, part_bytes, old_off, &new_slabs)?;
        let clear_from = old_off + (new_total as u64 * 32);
        let clear_len = (old_total - new_total) as u64 * 32;
        exfat_write_zero_range(vol, part_start, part_bytes, clear_from, clear_len)?;
    } else {
        let skip_end = old_off + (old_total as u64 * 32);
        let dest_off = exfat_find_free_entry_run_volume_offset(
            &info, dir_first, dir_contig, dir_size, new_total, old_off, skip_end, vol, part_start,
            part_bytes,
        )?;
        exfat_write_entry_slabs_at(vol, part_start, part_bytes, dest_off, &new_slabs)?;
        exfat_write_zero_range(vol, part_start, part_bytes, old_off, old_total as u64 * 32)?;
    }
    fs.sync_bitmap().map_err(exfat_err)?;
    drop(fs);
    vol.flush_serial()?;
    Ok(())
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
    entry: &ExFatFileEntry,
    new_name: &str,
) -> io::Result<Vec<[u8; 32]>> {
    exfat_validate_rename_name(new_name)?;
    let name_utf16: Vec<u16> = new_name.encode_utf16().collect();
    let name_len = name_utf16.len();
    let name_entry_count = (name_len + EXFAT_CHARS_PER_NAME_ENTRY - 1) / EXFAT_CHARS_PER_NAME_ENTRY;
    let secondary_count = (1 + name_entry_count) as u8;
    let (create_ts, create_10ms, create_utc) = entry.created.to_raw();
    let (modify_ts, modify_10ms, modify_utc) = entry.modified.to_raw();
    let (access_ts, _, access_utc) = entry.accessed.to_raw();
    let file_entry = RawFileDirectoryEntry {
        entry_type: EXFAT_ENTRY_FILE_DIRECTORY,
        secondary_count,
        set_checksum: U16::<LittleEndian>::new(0),
        file_attributes: U16::<LittleEndian>::new(entry.attributes.bits()),
        reserved1: U16::<LittleEndian>::new(0),
        create_timestamp: U32::<LittleEndian>::new(create_ts),
        last_modified_timestamp: U32::<LittleEndian>::new(modify_ts),
        last_accessed_timestamp: U32::<LittleEndian>::new(access_ts),
        create_10ms_increment: create_10ms,
        last_modified_10ms_increment: modify_10ms,
        create_utc_offset: create_utc,
        last_modified_utc_offset: modify_utc,
        last_accessed_utc_offset: access_utc,
        reserved2: [0; 7],
    };
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
    out.push(exfat_struct_to_entry_bytes(&file_entry));
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

fn exfat_write_entry_slabs_at(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offset: u64,
    slabs: &[[u8; 32]],
) -> io::Result<()> {
    let need = (slabs.len() as u64).saturating_mul(32);
    if offset.saturating_add(need) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT entry write past partition",
        ));
    }
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    for (i, slab) in slabs.iter().enumerate() {
        disk.seek(SeekFrom::Start(offset + (i as u64 * 32)))?;
        disk.write_all(slab)?;
    }
    disk.flush()?;
    Ok(())
}

fn exfat_write_zero_range(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    offset: u64,
    len: u64,
) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    if offset.saturating_add(len) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT zero fill past partition",
        ));
    }
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    let zeros = [0u8; 32];
    let mut remain = len;
    let mut pos = offset;
    while remain > 0 {
        let n = remain.min(32);
        disk.seek(SeekFrom::Start(pos))?;
        disk.write_all(&zeros[..n as usize])?;
        pos += n;
        remain -= n;
    }
    disk.flush()?;
    Ok(())
}

fn exfat_next_cluster_in_chain(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    info: &hadris_fat::exfat::ExFatInfo,
    cluster: u32,
) -> io::Result<Option<u32>> {
    let mut disk = vol.partition_disk_ro(part_start, part_bytes);
    let next = exfat_read_fat_entry_inner(&mut disk, info, cluster)?;
    if next == 0xFFFF_FFFF || next >= 0xFFFFFFF8 {
        Ok(None)
    } else {
        Ok(Some(next))
    }
}

/// Find `slots_needed` consecutive free 32-byte directory slots; returns volume byte offset of
/// the first slot. `skip_start..skip_end` is treated as occupied (e.g. the entry being renamed).
fn exfat_find_free_entry_run_volume_offset(
    info: &hadris_fat::exfat::ExFatInfo,
    dir_first_cluster: u32,
    dir_is_contiguous: bool,
    dir_size: u64,
    slots_needed: usize,
    skip_start: u64,
    skip_end: u64,
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
) -> io::Result<u64> {
    let cluster_size = info.bytes_per_cluster;
    let mut current_cluster = dir_first_cluster;
    let mut consecutive_free = 0usize;
    let mut first_free_abs: u64 = 0;

    loop {
        let cluster_offset = info.cluster_to_offset(current_cluster);

        for entry_idx in 0..(cluster_size / 32) {
            let abs = cluster_offset + (entry_idx as u64 * 32);
            if abs >= skip_start && abs < skip_end {
                consecutive_free = 0;
                continue;
            }
            let ent = exfat_read_dir_entry_at(vol, part_start, part_bytes, abs)?;
            let entry_type_byte = ent[0];
            if entry_type_byte == EXFAT_ENTRY_END_OR_FREE
                || entry_type_byte == EXFAT_ENTRY_DELETED_FILE
            {
                if consecutive_free == 0 {
                    first_free_abs = abs;
                }
                consecutive_free += 1;
                if consecutive_free >= slots_needed {
                    return Ok(first_free_abs);
                }
            } else {
                consecutive_free = 0;
            }
        }

        if dir_is_contiguous {
            current_cluster = current_cluster.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "exFAT directory cluster overflow",
                )
            })?;
            if (current_cluster - dir_first_cluster) as u64 * cluster_size as u64 >= dir_size
                && dir_size > 0
            {
                break;
            }
        } else {
            match exfat_next_cluster_in_chain(vol, part_start, part_bytes, info, current_cluster)? {
                Some(next) => current_cluster = next,
                None => break,
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::StorageFull,
        "exFAT directory has no room for a longer name (rename)",
    ))
}

fn exfat_read_dir_entry_at(
    vol: &ExfatVolumeSource,
    part_start: u64,
    part_bytes: u64,
    abs: u64,
) -> io::Result<[u8; 32]> {
    if abs.saturating_add(32) > part_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exFAT directory read past partition",
        ));
    }
    let mut disk = vol.partition_disk_rw(part_start, part_bytes);
    disk.seek(SeekFrom::Start(abs))?;
    let mut b = [0u8; 32];
    disk.read_exact(&mut b)?;
    Ok(b)
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
    trace(&format!("exFAT: open_path({path:?})…"));
    let entry = fs.open_path(path).map_err(|e| {
        trace(&format!("exFAT: open_path FAILED: {e}"));
        exfat_err(e)
    })?;
    trace("exFAT: open_path OK");
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
    let entry = fs.open_path(path).map_err(|e| {
        trace(&format!("exFAT: open_path FAILED: {e}"));
        exfat_err(e)
    })?;

    if entry.is_directory() {
        trace(&format!("exFAT: node is directory path={path:?}"));
        let dir = fs.open_dir(path).map_err(|e| {
            trace(&format!("exFAT: open_dir FAILED: {e}"));
            exfat_err(e)
        })?;
        let mut children: Vec<String> = Vec::new();
        for item in dir.entries() {
            let ch = item.map_err(exfat_err)?;
            let n = ch.name.as_str();
            if n == "." || n == ".." {
                continue;
            }
            children.push(ch.name);
        }
        trace(&format!(
            "exFAT: directory children count={} names={children:?}",
            children.len()
        ));
        drop(dir);
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

fn read_file_exfat<D: Read + Write + Seek>(disk: D, path: &str) -> io::Result<Vec<u8>> {
    let fs = ExFatFs::open(disk).map_err(|e| io::Error::other(e.to_string()))?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let mut r = fs
        .open_file(trimmed)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut v = Vec::new();
    r.read_to_end(&mut v)?;
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

fn read_file_exfat_streaming<D: Read + Write + Seek>(
    disk: D,
    path: &str,
    mut out: impl Write,
    progress: &mut impl FnMut(u64) -> bool,
    max_bytes: Option<u64>,
) -> io::Result<()> {
    let fs = ExFatFs::open(disk).map_err(|e| io::Error::other(e.to_string()))?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let mut r = fs
        .open_file(trimmed)
        .map_err(|e| io::Error::other(e.to_string()))?;
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
        let n = r.read(&mut buf[..cap])?;
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

fn list_dir_exfat<D: Read + Write + Seek>(disk: D, path: &str) -> io::Result<Vec<SessionEntry>> {
    let fs = ExFatFs::open(disk).map_err(|e| io::Error::other(e.to_string()))?;
    let trimmed = path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches('/');
    let segs: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    let mut dir = fs.root_dir();
    if segs.is_empty() {
        return collect_hadris_exfat_entries(&dir, "");
    }

    for (i, seg) in segs.iter().enumerate() {
        if i + 1 == segs.len() {
            let sub = dir
                .open_dir(seg)
                .map_err(|e| io::Error::other(e.to_string()))?;
            let parent_prefix = segs.join("/");
            return collect_hadris_exfat_entries(&sub, &parent_prefix);
        }
        dir = dir
            .open_dir(seg)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    unreachable!("exFAT path navigation always returns when path segments are non-empty");
}

fn collect_hadris_exfat_entries<DATA: Read + Seek>(
    dir: &ExFatDir<'_, DATA>,
    prefix: &str,
) -> io::Result<Vec<SessionEntry>> {
    let mut out = Vec::new();
    let path_prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    for r in dir.entries() {
        let e = r.map_err(|e| io::Error::other(e.to_string()))?;
        let name = e.name.clone();
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = e.is_directory();
        let size = if is_dir { 0 } else { e.size() };
        let path = if path_prefix.is_empty() {
            name.clone()
        } else {
            format!("{path_prefix}{name}")
        };
        let hidden = e.attributes.contains(ExFatFileAttributes::HIDDEN) || name.starts_with('.');
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

fn legacy_mbr_first_partition_lba(sector0: &[u8]) -> u64 {
    if sector0.len() < 512 {
        return 0;
    }
    let lba = u32::from_le_bytes([
        sector0[0x1C6],
        sector0[0x1C7],
        sector0[0x1C8],
        sector0[0x1C9],
    ]);
    if lba != 0 {
        u64::from(lba)
    } else {
        0
    }
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
}

impl Read for PartitionDiskUnion {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Sc64(d) => d.read(buf),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.read(buf),
            Self::Ram(d) => d.read(buf),
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
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Sc64(d) => d.flush(),
            #[cfg(feature = "ed64")]
            Self::Ed64(d) => d.flush(),
            Self::Ram(d) => d.flush(),
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
        }
    }

    fn flush_serial(&self) -> io::Result<()> {
        match self {
            Self::Sc64 { link } => flush_link_serial(link),
            Self::Ram { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cart_rel_path_trimmed;

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
        detect_partition_start, list_dir_exfat, list_dir_fat, read_file_exfat, read_file_fat,
        write_file_exfat_streaming, write_file_fat_streaming_impl, ExfatVolumeSource,
    };
    use crate::mem_disk::RamPartitionDisk;
    use fatfs::FormatVolumeOptions;
    use hadris_fat::exfat::{format_exfat, ExFatFormatOptions};
    use sha2::{Digest, Sha256};
    use std::io::Cursor;
    use std::io::Write;
    use std::sync::Arc;

    const FAT32_IMAGE_BYTES: usize = 8 * 1024 * 1024;
    const EXFAT_IMAGE_BYTES: usize = 4 * 1024 * 1024;

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
