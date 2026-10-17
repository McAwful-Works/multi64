//! Writing a file on the PC so that a failure or a cancel never costs the file already there.
//!
//! Every export (cart → PC) writes through [`write_host_file`]: the bytes go to a temporary file
//! beside the destination, which replaces the destination only once it is complete and on disk.
//! Until then the destination is untouched, so a cancelled or failed **Replace** leaves the
//! original as it was, and a new file leaves nothing. The cart side does the same for imports
//! (#200).

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Buffer between the copy loop and the temporary file.
const BUF_BYTES: usize = 512 * 1024;

/// Write `dest` from `write`, replacing any file already there only once the new one is complete.
///
/// `write` gets a buffered writer over a temporary file in `dest`'s folder, named
/// `.<name>.<pid>-<n>.partial`. When it returns `Ok`, the buffer is flushed, the file is synced,
/// and it is renamed over `dest`. When anything fails — `write` itself (a cancel is an
/// [`io::ErrorKind::Interrupted`] error from it), the flush, the sync or the rename — the temporary
/// file is removed, `dest` is left exactly as it was, and the error is returned.
///
/// A crash or power loss part-way can leave the temporary file behind, never a truncated `dest`.
pub fn write_host_file(
    dest: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let tmp = temp_path_beside(dest)?;
    let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let result = fill_and_flush(file, write)
        .and_then(|f| f.sync_all())
        .and_then(|()| std::fs::rename(&tmp, dest));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Run `write` into a buffer over `inner`, then flush it, so a write that only reaches `inner` at
/// the flush (a file smaller than the buffer reaches it nowhere else) still reports its error.
fn fill_and_flush<W: Write>(
    inner: W,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<W> {
    let mut out = BufWriter::with_capacity(BUF_BYTES, inner);
    write(&mut out)?;
    out.into_inner().map_err(|e| e.into_error())
}

fn temp_path_beside(dest: &Path) -> io::Result<PathBuf> {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = dest.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a file path: {}", dest.display()),
        )
    })?;
    let tmp_name = format!(
        ".{}.{}-{}.partial",
        name.to_string_lossy(),
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    Ok(dest.with_file_name(tmp_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "multi64-host-file-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn names(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_completed_write_replaces_the_destination() {
        let dir = Scratch::new("ok");
        let dest = dir.0.join("game.z64");
        std::fs::write(&dest, b"old contents, longer than the new").unwrap();
        write_host_file(&dest, |w| w.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(dir.names(), ["game.z64"], "no temporary file is left");
    }

    /// A cancel part-way through a Replace used to truncate the original first and then delete it.
    #[test]
    fn a_cancelled_replace_leaves_the_original_and_no_temporary_file() {
        let dir = Scratch::new("cancel");
        let dest = dir.0.join("game.z64");
        std::fs::write(&dest, b"the original").unwrap();
        let err = write_host_file(&dest, |w| {
            w.write_all(&[0u8; 4096])?;
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert_eq!(std::fs::read(&dest).unwrap(), b"the original");
        assert_eq!(dir.names(), ["game.z64"]);
    }

    #[test]
    fn a_failed_new_file_leaves_nothing() {
        let dir = Scratch::new("fail");
        let dest = dir.0.join("game.z64");
        let err = write_host_file(&dest, |w| {
            w.write_all(b"part")?;
            Err(io::Error::other("serial read failed"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "serial read failed");
        assert!(dir.names().is_empty(), "{:?}", dir.names());
    }

    /// A writer that accepts nothing: every byte only reaches it at the flush.
    struct RefusesWrites;

    impl Write for RefusesWrites {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A file smaller than the buffer reaches the disk only at the flush; its error must be the
    /// caller's, not dropped with the buffer.
    #[test]
    fn an_error_at_the_final_flush_is_reported() {
        let err = fill_and_flush(RefusesWrites, |w| w.write_all(b"a small save file"))
            .err()
            .expect("the flush error is returned");
        assert_eq!(err.to_string(), "disk full");
    }
}
