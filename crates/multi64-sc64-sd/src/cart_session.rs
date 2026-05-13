//! Unified cart SD session for tooling that supports both SummerCart64 and (optional) EverDrive linear `RomRead`.

#[cfg(feature = "ed64")]
use crate::partition::Ed64SdSession;
use crate::partition::Sc64SdSession;
use crate::SessionEntry;
use std::io;
use std::path::Path;

/// Active USB SD session: SC64 or EverDrive experimental linear mapping.
pub enum CartSession {
    Sc64(Sc64SdSession),
    #[cfg(feature = "ed64")]
    Ed64(Ed64SdSession),
}

impl CartSession {
    pub fn close(&self) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.close(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.close(),
        }
    }

    pub fn flush_serial(&self) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.flush_serial(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.flush_serial(),
        }
    }

    pub fn list_dir(&self, path: &str) -> io::Result<Vec<SessionEntry>> {
        match self {
            Self::Sc64(s) => s.list_dir(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.list_dir(path),
        }
    }

    pub fn is_exfat(&self) -> bool {
        match self {
            Self::Sc64(s) => s.is_exfat(),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.is_exfat(),
        }
    }

    pub fn cart_path_entry_kind(&self, path: &str) -> io::Result<Option<bool>> {
        match self {
            Self::Sc64(s) => s.cart_path_entry_kind(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.cart_path_entry_kind(path),
        }
    }

    pub fn total_bytes_for_cart_entry(&self, path: &str) -> io::Result<u64> {
        match self {
            Self::Sc64(s) => s.total_bytes_for_cart_entry(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.total_bytes_for_cart_entry(path),
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
        }
    }

    pub fn remove_cart_path(&self, path: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.remove_cart_path(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.remove_cart_path(path),
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
        }
    }

    pub fn mkdir_cart(&self, path: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.mkdir_cart(path),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.mkdir_cart(path),
        }
    }

    pub fn rename_cart(&self, from: &str, to: &str) -> io::Result<()> {
        match self {
            Self::Sc64(s) => s.rename_cart(from, to),
            #[cfg(feature = "ed64")]
            Self::Ed64(s) => s.rename_cart(from, to),
        }
    }
}
