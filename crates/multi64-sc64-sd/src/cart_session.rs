//! Unified cart SD session for tooling that supports SummerCart64, optionally the EverDrive `RomRead`
//! experiment (which reads cart ROM memory rather than the SD card), and optionally the EverDrive-64 PRO's
//! file-level access.

#[cfg(feature = "ed64pro")]
use crate::ed64pro::Ed64ProSdSession;
#[cfg(feature = "ed64")]
use crate::partition::Ed64SdSession;
use crate::partition::Sc64SdSession;
use crate::SessionEntry;
use std::io;
use std::path::Path;

/// Active USB SD session: SC64, the EverDrive `RomRead` experiment (not real SD access), or an
/// EverDrive-64 PRO (experimental file-level access).
///
/// Dropping the session releases it (see [`Sc64SdSession`]'s `Drop`), so no path can strand the
/// cart's SD card locked to the PC side. Call [`close`](Self::close) where the error matters.
pub enum CartSession {
    Sc64(Sc64SdSession),
    #[cfg(feature = "ed64")]
    Ed64(Ed64SdSession),
    #[cfg(feature = "ed64pro")]
    Ed64Pro(Ed64ProSdSession),
}

impl CartSession {
    /// Release the USB SD session, reporting failure. Idempotent, and also done on drop.
    pub fn close(&self) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.close(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.close(),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.close(),
        }
    }

    pub fn flush_serial(&self) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.flush_serial(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.flush_serial(),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.flush_serial(),
        }
    }

    pub fn list_dir(&self, path: &str) -> io::Result<Vec<SessionEntry>> {
        match self {
            Self::Sc64(s) => s.list_dir(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.list_dir(path),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.list_dir(path),
        }
    }

    pub fn is_exfat(&self) -> bool {
        match self {
            Self::Sc64(s) => s.is_exfat(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.is_exfat(),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.is_exfat(),
        }
    }

    /// Short volume label for a file list: `"FAT"`, `"exFAT"`, or a cart name when the host does not
    /// mount the volume itself.
    pub fn fs_label(&self) -> &'static str {
        match self {
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.fs_label(),
            _ => {
                if self.is_exfat() {
                    "exFAT"
                } else {
                    "FAT"
                }
            }
        }
    }

    /// Whether this is an EverDrive-64 PRO session, whose writes are experimental.
    pub fn is_ed64_pro(&self) -> bool {
        match self {
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(_) => true,
            _ => false,
        }
    }

    pub fn cart_path_entry_kind(&self, path: &str) -> io::Result<Option<bool>> {
        match self {
            Self::Sc64(s) => s.cart_path_entry_kind(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.cart_path_entry_kind(path),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.cart_path_entry_kind(path),
        }
    }

    pub fn total_bytes_for_cart_entry(&self, path: &str) -> io::Result<u64> {
        match self {
            Self::Sc64(s) => s.total_bytes_for_cart_entry(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.total_bytes_for_cart_entry(path),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.total_bytes_for_cart_entry(path),
        }
    }

    pub fn copy_cart_entry_to_host_with_progress<F>(
        &self,
        cart_path: &str,
        dest: &Path,
        skip_existing: bool,
        progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        match self {
            Self::Sc64(s) => {
                s.copy_cart_entry_to_host_with_progress(cart_path, dest, skip_existing, progress)
            }
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => {
                s.copy_cart_entry_to_host_with_progress(cart_path, dest, skip_existing, progress)
            }
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => {
                s.copy_cart_entry_to_host_with_progress(cart_path, dest, skip_existing, progress)
            }
        }
    }

    pub fn import_from_pc_with_progress<F>(
        &self,
        src: &Path,
        cart_parent: &str,
        dest_name: &str,
        skip_existing: bool,
        progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(u64) -> bool,
    {
        match self {
            Self::Sc64(s) => {
                s.import_from_pc_with_progress(src, cart_parent, dest_name, skip_existing, progress)
            }
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => {
                s.import_from_pc_with_progress(src, cart_parent, dest_name, skip_existing, progress)
            }
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => {
                s.import_from_pc_with_progress(src, cart_parent, dest_name, skip_existing, progress)
            }
        }
    }

    pub fn remove_cart_path(&self, path: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.remove_cart_path(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.remove_cart_path(path),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.remove_cart_path(path),
        }
    }

    pub fn remove_cart_path_traced(
        &self,
        path: &str,
        trace: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.remove_cart_path_traced(path, trace),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.remove_cart_path_traced(path, trace),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.remove_cart_path_traced(path, trace),
        }
    }

    pub fn mkdir_cart(&self, path: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.mkdir_cart(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.mkdir_cart(path),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.mkdir_cart(path),
        }
    }

    pub fn rename_cart(&self, from: &str, to: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.rename_cart(from, to),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.rename_cart(from, to),
            #[cfg(feature = "ed64pro")]
            Self::Ed64Pro(s) => s.rename_cart(from, to),
        }
    }
}
