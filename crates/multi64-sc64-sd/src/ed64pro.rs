//! EverDrive-64 PRO SD access at **file level**, over edlink Gen3 (`multi64-ed64pro-link`).
//!
//! **Experimental; never run against a cart.** The SC64 and X-series sessions read sectors and mount
//! FAT on the host. The PRO's microcontroller owns the file system instead and serves file commands,
//! so every [`crate::CartSession`] operation here maps onto those commands. Behaviour mirrors
//! [`crate::Sc64SdSession`] — recursion, `skip_existing`, cancellation and partial-file cleanup — so
//! Xfer64 can treat both carts the same. See workspace `docs/spec/ed64-pro-usb-host.md`.

use crate::partition::{cart_path_parts, SessionEntry};
use multi64_ed64pro_link::{
    dir_option, open_mode, Ed64Pro, Error as LinkError, FileInfo, Transport,
};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Bytes per file-read or file-write transfer. Small enough to finish well inside the link's 2 s
/// timeout on a slow connection, large enough that transfer overhead stays negligible.
const CHUNK: usize = 64 * 1024;
/// FAT attribute bit for hidden entries.
const ATTR_HIDDEN: u8 = 0x02;

/// An open file-level session with an EverDrive-64 PRO.
pub struct Ed64ProSdSession<T: Transport = Box<dyn serialport::SerialPort>> {
    dev: Mutex<Ed64Pro<T>>,
}

impl Ed64ProSdSession {
    /// Open the port, run the edlink handshake, and initialise the SD file system.
    pub fn open(port_name: &str) -> io::Result<Self> {
        let dev = Ed64Pro::open(port_name).map_err(link_err)?;
        Self::from_device(dev)
    }
}

impl<T: Transport> Ed64ProSdSession<T> {
    /// Wrap an already-connected device and initialise its file system (`FS_SCMD_INIT`).
    pub fn from_device(mut dev: Ed64Pro<T>) -> io::Result<Self> {
        dev.fs_init().map_err(link_err)?;
        Ok(Self {
            dev: Mutex::new(dev),
        })
    }

    pub fn into_device(self) -> Ed64Pro<T> {
        self.dev.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    /// Nothing to release: neither Krikzz source locks the card to the PC. Idempotent.
    pub fn close(&self) -> io::Result<()> {
        Ok(())
    }

    /// Every command already flushes the port before waiting for its reply.
    pub fn flush_serial(&self) -> io::Result<()> {
        Ok(())
    }

    /// Always `false`: the host never mounts the volume, so it cannot tell. See [`Self::fs_label`].
    pub fn is_exfat(&self) -> bool {
        false
    }

    /// Volume label for the file list; the host does not know the card's file system.
    pub fn fs_label(&self) -> &'static str {
        "EverDrive-64 PRO"
    }

    /// List a directory (`/`, `/folder`, `folder`). Entry paths have no leading slash, like SC64.
    pub fn list_dir(&self, path: &str) -> io::Result<Vec<SessionEntry>> {
        let dir = pro_path(path);
        let infos = self
            .dev()?
            .dir_list(&dir, dir_option::SORTED)
            .map_err(link_err)?;
        Ok(infos
            .into_iter()
            .filter(|i| i.name != "." && i.name != "..")
            .map(|i| entry(&dir, i))
            .collect())
    }

    /// `Some(true)` for a directory, `Some(false)` for a file, `None` if absent.
    pub fn cart_path_entry_kind(&self, path: &str) -> io::Result<Option<bool>> {
        Ok(self.find_entry(path)?.map(|e| e.is_dir))
    }

    /// Total bytes of a file, or of every file under a directory (for progress).
    pub fn total_bytes_for_cart_entry(&self, cart_path: &str) -> io::Result<u64> {
        let e = self.find_entry(cart_path)?.ok_or_else(not_found)?;
        if e.is_dir {
            self.dir_total_bytes(&e.path)
        } else {
            Ok(e.size)
        }
    }

    /// Copy a file or directory tree to the PC, reporting bytes copied (delta) to `progress`.
    /// Returning `false` from `progress` aborts with [`io::ErrorKind::Interrupted`].
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
        let e = self.find_entry(cart_path)?.ok_or_else(not_found)?;
        if e.is_dir {
            return self.copy_dir_to_host(&e.path, dest, skip_existing, &mut progress);
        }
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
        self.copy_file_to_host(&e.path, dest, &mut progress)
    }

    /// Copy a PC file or folder into `cart_parent/dest_name`, reporting bytes written (delta).
    /// With `skip_existing`, files already on the cart are left alone.
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
        let cart_root = join(&pro_path(cart_parent), dest_name);
        if meta.is_file() {
            match self.cart_path_entry_kind(&cart_root)? {
                Some(true) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy file over existing folder on the cart",
                    ));
                }
                Some(false) if skip_existing => return Ok(()),
                _ => {}
            }
            return self.write_file_from_pc(src, &cart_root, &mut progress);
        }
        if meta.is_dir() {
            if let Some(false) = self.cart_path_entry_kind(&cart_root)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cannot copy folder over existing file on the cart",
                ));
            }
            self.mkdir_cart(&cart_root)?;
            return self.import_dir(src, &cart_root, skip_existing, &mut progress);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported source type",
        ))
    }

    pub fn remove_cart_path(&self, path: &str) -> io::Result<()> {
        self.remove_cart_path_traced(path, &mut |_| {})
    }

    /// Delete a file, or a directory and everything under it (children first).
    pub fn remove_cart_path_traced(
        &self,
        path: &str,
        trace: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        if pro_path(path).is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to delete the SD card root",
            ));
        }
        let e = self.find_entry(path)?.ok_or_else(not_found)?;
        trace(&format!(
            "remove: EverDrive-64 PRO file commands, path={}",
            e.path
        ));
        self.remove_entry(&e, trace)
    }

    /// Create a directory, and any missing parents. Succeeds if it already exists.
    pub fn mkdir_cart(&self, path: &str) -> io::Result<()> {
        let mut prefix = String::new();
        for part in pro_path(path).split('/').filter(|s| !s.is_empty()) {
            prefix = join(&prefix, part);
            match self.cart_path_entry_kind(&prefix)? {
                Some(true) => {}
                Some(false) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("a file named {prefix} already exists on the cart"),
                    ));
                }
                None => self.dev()?.dir_make(&prefix).map_err(link_err)?,
            }
        }
        Ok(())
    }

    /// Rename a file or folder within its folder, by copying it to the new name and deleting the
    /// original: neither Krikzz source defines a rename command for the PRO.
    ///
    /// Each file goes through a temporary file on the PC, since the link holds one open file at a
    /// time, and is deleted from the cart only once its copy is written and has the right size. An
    /// interrupted rename therefore leaves every file under at least one of the two names, though a
    /// folder can be left split between them. It takes as long as downloading and re-uploading the
    /// entry, and a name differing only in letter case is refused: FAT names ignore case, so the copy
    /// would land on the original.
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
        if name_from == name_to {
            return Ok(());
        }
        if name_from.eq_ignore_ascii_case(&name_to) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The EverDrive-64 PRO renames by copying, so it cannot change only the letter case of a name.",
            ));
        }
        let source = self.find_entry(from)?.ok_or_else(not_found)?;
        if self.find_entry(to)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{name_to} already exists on the cart"),
            ));
        }
        self.move_entry(&source, &join(&pro_path(&parent_from), &name_to))
    }

    // -- helpers ------------------------------------------------------------------------------

    fn dev(&self) -> io::Result<MutexGuard<'_, Ed64Pro<T>>> {
        self.dev.lock().map_err(|e| io::Error::other(e.to_string()))
    }

    fn find_entry(&self, path: &str) -> io::Result<Option<SessionEntry>> {
        let (parent, name) = cart_path_parts(path);
        if name.is_empty() {
            return Ok(None);
        }
        let list = match self.list_dir(&parent) {
            Ok(l) => l,
            // The link cannot tell "no such folder" from other refusals, so a non-root parent the
            // cart will not list is treated as absent — the trade-off `Sc64SdSession` makes too.
            // Transport failures (timeouts, a dropped port) keep their own kinds and propagate.
            Err(e) if !parent.is_empty() && e.kind() == io::ErrorKind::Other => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(list
            .into_iter()
            .find(|e| e.name == name || e.name.eq_ignore_ascii_case(&name)))
    }

    fn dir_total_bytes(&self, dir: &str) -> io::Result<u64> {
        let mut total = 0;
        for e in self.list_dir(dir)? {
            total += if e.is_dir {
                self.dir_total_bytes(&e.path)?
            } else {
                e.size
            };
        }
        Ok(total)
    }

    fn copy_dir_to_host(
        &self,
        cart_dir: &str,
        dest_dir: &Path,
        skip_existing: bool,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        std::fs::create_dir_all(dest_dir)?;
        for e in self.list_dir(cart_dir)? {
            let sub = dest_dir.join(&e.name);
            if e.is_dir {
                if sub.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy cart folder over existing file on the PC",
                    ));
                }
                self.copy_dir_to_host(&e.path, &sub, skip_existing, progress)?;
            } else {
                if skip_existing && sub.exists() {
                    continue;
                }
                self.copy_file_to_host(&e.path, &sub, progress)?;
            }
        }
        Ok(())
    }

    fn copy_file_to_host(
        &self,
        cart_file: &str,
        dest: &Path,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        let mut dev = self.dev()?;
        dev.file_open(&pro_path(cart_file), open_mode::READ)
            .map_err(link_err)?;
        let result = (|| -> io::Result<()> {
            let mut remaining = dev.file_available().map_err(link_err)?;
            let mut out = BufWriter::new(std::fs::File::create(dest)?);
            let mut buf = vec![0u8; CHUNK];
            while remaining > 0 {
                let n = remaining.min(CHUNK as u64) as usize;
                dev.file_read(&mut buf[..n]).map_err(link_err)?;
                out.write_all(&buf[..n])?;
                remaining -= n as u64;
                if !progress(n as u64) {
                    return Err(cancelled());
                }
            }
            out.flush()
        })();
        let closed = dev.file_close().map_err(link_err);
        match result {
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    let _ = std::fs::remove_file(dest);
                }
                Err(e)
            }
            Ok(()) => closed,
        }
    }

    fn import_dir(
        &self,
        src_dir: &Path,
        cart_base: &str,
        skip_existing: bool,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        for entry in std::fs::read_dir(src_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let cart_sub = join(cart_base, &name);
            if entry.metadata()?.is_dir() {
                if let Some(false) = self.cart_path_entry_kind(&cart_sub)? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "cannot copy folder over existing file on the cart",
                    ));
                }
                self.mkdir_cart(&cart_sub)?;
                self.import_dir(&entry.path(), &cart_sub, skip_existing, progress)?;
            } else {
                match self.cart_path_entry_kind(&cart_sub)? {
                    Some(true) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "cannot copy file over existing folder on the cart",
                        ));
                    }
                    Some(false) if skip_existing => continue,
                    _ => {}
                }
                self.write_file_from_pc(&entry.path(), &cart_sub, progress)?;
            }
        }
        Ok(())
    }

    fn write_file_from_pc(
        &self,
        src: &Path,
        cart_path: &str,
        progress: &mut impl FnMut(u64) -> bool,
    ) -> io::Result<()> {
        let mut reader = BufReader::new(std::fs::File::open(src)?);
        let path = pro_path(cart_path);
        let mut dev = self.dev()?;
        dev.file_open(
            &path,
            open_mode::WRITE | open_mode::CREATE_ALWAYS | open_mode::MAKE_PATH,
        )
        .map_err(link_err)?;
        let result = (|| -> io::Result<()> {
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = read_full(&mut reader, &mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                dev.file_write(&buf[..n]).map_err(link_err)?;
                if !progress(n as u64) {
                    return Err(cancelled());
                }
            }
        })();
        let closed = dev.file_close().map_err(link_err);
        match result {
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    let _ = dev.delete(&path);
                }
                Err(e)
            }
            Ok(()) => closed,
        }
    }

    /// Move an entry to `dest`, where nothing exists yet: copy it, then delete the original. A
    /// folder moves child by child, so each file is deleted as soon as its own copy is in place.
    fn move_entry(&self, e: &SessionEntry, dest: &str) -> io::Result<()> {
        if e.is_dir {
            self.dev()?.dir_make(dest).map_err(link_err)?;
            for child in self.list_dir(&e.path)? {
                self.move_entry(&child, &join(dest, &child.name))?;
            }
        } else {
            self.copy_file_within_cart(e, dest)?;
        }
        self.dev()?.delete(&e.path).map_err(link_err)
    }

    /// Copy a cart file to a new cart path through a temporary file on the PC, and check the copy's
    /// size before reporting success. A failed copy is deleted; the original is never touched.
    fn copy_file_within_cart(&self, e: &SessionEntry, dest: &str) -> io::Result<()> {
        let temp = HostTemp::new();
        let copied = self
            .copy_file_to_host(&e.path, &temp.0, &mut |_| true)
            .and_then(|()| self.write_file_from_pc(&temp.0, dest, &mut |_| true))
            .and_then(|()| match self.find_entry(dest)? {
                Some(c) if !c.is_dir && c.size == e.size => Ok(()),
                _ => Err(io::Error::other(format!(
                    "the copy of {} on the cart does not match the original, which was kept",
                    e.path
                ))),
            });
        if copied.is_err() {
            if let Ok(mut dev) = self.dev() {
                let _ = dev.delete(dest);
            }
        }
        copied
    }

    fn remove_entry(&self, e: &SessionEntry, trace: &mut dyn FnMut(&str)) -> io::Result<()> {
        if e.is_dir {
            for child in self.list_dir(&e.path)? {
                self.remove_entry(&child, trace)?;
            }
        }
        trace(&format!("delete {}", e.path));
        self.dev()?.delete(&e.path).map_err(link_err)
    }
}

/// A cart path as the PRO expects it: forward slashes, no leading or trailing slash.
fn pro_path(path: &str) -> String {
    path.trim().replace('\\', "/").trim_matches('/').to_string()
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn entry(parent: &str, info: FileInfo) -> SessionEntry {
    let is_dir = info.is_dir();
    SessionEntry {
        path: join(parent, &info.name),
        is_dir,
        size: if is_dir { 0 } else { u64::from(info.size) },
        hidden: info.attributes & ATTR_HIDDEN != 0 || info.name.starts_with('.'),
        name: info.name,
    }
}

/// Map link errors onto `io::Error`. Cart-reported refusals become [`io::ErrorKind::Other`], which
/// `find_entry` relies on to tell them apart from transport failures.
fn link_err(e: LinkError) -> io::Error {
    match e {
        LinkError::Io(inner) => inner,
        other @ LinkError::PathTooLong(_) => {
            io::Error::new(io::ErrorKind::InvalidInput, other.to_string())
        }
        other => io::Error::other(other.to_string()),
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "cart path not found")
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
}

/// Fill `buf` unless the reader ends first; returns bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// A path under the system temp directory for one file in transit, removed on drop.
struct HostTemp(PathBuf);

impl HostTemp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
            "multi64-ed64pro-rename-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        )))
    }
}

impl Drop for HostTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use multi64_ed64pro_link::fake::FakeEd64Pro;

    fn session(fake: FakeEd64Pro) -> Ed64ProSdSession<FakeEd64Pro> {
        Ed64ProSdSession::from_device(Ed64Pro::connect(fake).expect("handshake")).expect("fs init")
    }

    fn fake_of(s: Ed64ProSdSession<FakeEd64Pro>) -> FakeEd64Pro {
        s.into_device().into_inner()
    }

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "multi64-ed64pro-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn list_dir_reports_paths_without_a_leading_slash() {
        let s = session(
            FakeEd64Pro::new()
                .with_file("roms/Game.z64", b"n64")
                .with_file(".hidden", b"x"),
        );
        let root = s.list_dir("/").unwrap();
        let names: Vec<_> = root
            .iter()
            .map(|e| (e.path.as_str(), e.is_dir, e.hidden))
            .collect();
        assert_eq!(names, [("roms", true, false), (".hidden", false, true)]);
        let roms = s.list_dir("/roms/").unwrap();
        assert_eq!(roms[0].path, "roms/Game.z64");
        assert_eq!(roms[0].size, 3);
    }

    #[test]
    fn entry_kind_matches_case_insensitively_and_missing_parent_is_none() {
        let s = session(FakeEd64Pro::new().with_file("roms/Game.z64", b"n64"));
        assert_eq!(s.cart_path_entry_kind("ROMS").unwrap(), Some(true));
        assert_eq!(
            s.cart_path_entry_kind("roms/game.Z64").unwrap(),
            Some(false)
        );
        assert_eq!(s.cart_path_entry_kind("roms/nope.z64").unwrap(), None);
        assert_eq!(s.cart_path_entry_kind("missing/child").unwrap(), None);
        assert_eq!(s.cart_path_entry_kind("").unwrap(), None);
    }

    #[test]
    fn copies_a_file_and_a_tree_to_the_pc() {
        let big = pattern(CHUNK * 2 + 17);
        let s = session(
            FakeEd64Pro::new()
                .with_file("saves/a.eep", b"save-a")
                .with_file("saves/deep/b.srm", &big),
        );
        assert_eq!(
            s.total_bytes_for_cart_entry("saves").unwrap(),
            6 + big.len() as u64
        );
        let out = Scratch::new("export");

        let mut reported = 0u64;
        s.copy_cart_entry_to_host_with_progress("saves", &out.0.join("saves"), false, |n| {
            reported += n;
            true
        })
        .unwrap();
        assert_eq!(reported, 6 + big.len() as u64);
        assert_eq!(std::fs::read(out.0.join("saves/a.eep")).unwrap(), b"save-a");
        assert_eq!(std::fs::read(out.0.join("saves/deep/b.srm")).unwrap(), big);

        s.copy_cart_entry_to_host_with_progress(
            "saves/a.eep",
            &out.0.join("one.eep"),
            false,
            |_| true,
        )
        .unwrap();
        assert_eq!(std::fs::read(out.0.join("one.eep")).unwrap(), b"save-a");
    }

    #[test]
    fn cancelled_export_removes_the_partial_pc_file() {
        let s = session(FakeEd64Pro::new().with_file("big.z64", &pattern(CHUNK * 3)));
        let out = Scratch::new("export-cancel");
        let dest = out.0.join("big.z64");
        let err = s
            .copy_cart_entry_to_host_with_progress("big.z64", &dest, false, |_| false)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(!dest.exists());
    }

    #[test]
    fn imports_a_file_and_a_tree_then_reads_them_back() {
        let src = Scratch::new("import");
        let big = pattern(CHUNK + 5);
        std::fs::write(src.0.join("game.z64"), &big).unwrap();
        std::fs::create_dir_all(src.0.join("pack/sub")).unwrap();
        std::fs::write(src.0.join("pack/sub/x.txt"), b"xx").unwrap();
        std::fs::create_dir_all(src.0.join("pack/empty")).unwrap();

        let s = session(FakeEd64Pro::new());
        s.import_from_pc_with_progress(&src.0.join("game.z64"), "/roms", "game.z64", false, |_| {
            true
        })
        .unwrap();
        s.import_from_pc_with_progress(&src.0.join("pack"), "", "pack", false, |_| true)
            .unwrap();

        let fake = fake_of(s);
        assert_eq!(fake.file("roms/game.z64").unwrap(), big.as_slice());
        assert_eq!(fake.file("pack/sub/x.txt").unwrap(), b"xx");
        assert!(
            fake.is_dir("pack/empty"),
            "empty source folders are created"
        );
    }

    #[test]
    fn skip_existing_leaves_the_cart_file_alone() {
        let src = Scratch::new("skip");
        std::fs::write(src.0.join("a.z64"), b"new").unwrap();
        let s = session(FakeEd64Pro::new().with_file("a.z64", b"old"));
        s.import_from_pc_with_progress(&src.0.join("a.z64"), "", "a.z64", true, |_| true)
            .unwrap();
        assert_eq!(fake_of(s).file("a.z64").unwrap(), b"old");

        let s = session(FakeEd64Pro::new().with_file("a.z64", b"old"));
        s.import_from_pc_with_progress(&src.0.join("a.z64"), "", "a.z64", false, |_| true)
            .unwrap();
        assert_eq!(fake_of(s).file("a.z64").unwrap(), b"new");
    }

    #[test]
    fn cancelled_import_deletes_the_partial_cart_file() {
        let src = Scratch::new("import-cancel");
        std::fs::write(src.0.join("big.z64"), pattern(CHUNK * 3)).unwrap();
        let s = session(FakeEd64Pro::new());
        let err = s
            .import_from_pc_with_progress(&src.0.join("big.z64"), "", "big.z64", false, |_| false)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(!fake_of(s).exists("big.z64"));
    }

    #[test]
    fn import_refuses_a_file_over_a_folder() {
        let src = Scratch::new("conflict");
        std::fs::write(src.0.join("roms"), b"file").unwrap();
        let s = session(FakeEd64Pro::new().with_dir("roms"));
        let err = s
            .import_from_pc_with_progress(&src.0.join("roms"), "", "roms", false, |_| true)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn remove_deletes_a_tree_children_first_and_refuses_the_root() {
        let s = session(
            FakeEd64Pro::new()
                .with_file("old/a.z64", b"a")
                .with_file("old/sub/b.z64", b"b")
                .with_file("keep.z64", b"k"),
        );
        let mut steps = Vec::new();
        s.remove_cart_path_traced("/old", &mut |m| steps.push(m.to_string()))
            .unwrap();
        assert_eq!(
            steps.iter().filter(|m| m.starts_with("delete ")).count(),
            4,
            "{steps:?}"
        );
        assert_eq!(
            s.remove_cart_path("/").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            s.remove_cart_path("nope").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let fake = fake_of(s);
        assert!(!fake.exists("old"));
        assert!(fake.file("keep.z64").is_some());
    }

    #[test]
    fn mkdir_creates_parents_and_is_idempotent() {
        let s = session(FakeEd64Pro::new().with_file("taken", b"f"));
        s.mkdir_cart("/a/b/c").unwrap();
        s.mkdir_cart("a/b/c").unwrap();
        assert_eq!(
            s.mkdir_cart("taken/x").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(fake_of(s).is_dir("a/b/c"));
    }

    #[test]
    fn rename_copies_a_file_then_deletes_the_original() {
        let data = pattern(CHUNK * 2 + 3);
        let s = session(
            FakeEd64Pro::new()
                .with_file("roms/old.z64", &data)
                .with_file("roms/other.z64", b"o"),
        );
        s.rename_cart("/roms/old.z64", "/roms/new.z64").unwrap();
        let fake = fake_of(s);
        assert_eq!(fake.file("roms/new.z64").unwrap(), data.as_slice());
        assert!(!fake.exists("roms/old.z64"));
        assert_eq!(fake.file("roms/other.z64").unwrap(), b"o");
    }

    #[test]
    fn rename_moves_a_whole_folder() {
        let s = session(
            FakeEd64Pro::new()
                .with_file("saves/a.eep", b"save-a")
                .with_file("saves/deep/b.srm", &pattern(CHUNK + 1))
                .with_dir("saves/empty"),
        );
        s.rename_cart("saves", "backup").unwrap();
        let fake = fake_of(s);
        assert_eq!(fake.file("backup/a.eep").unwrap(), b"save-a");
        assert_eq!(fake.file("backup/deep/b.srm").unwrap(), pattern(CHUNK + 1));
        assert!(fake.is_dir("backup/empty"));
        assert!(!fake.exists("saves"));
    }

    #[test]
    fn rename_refuses_what_a_copy_cannot_do_and_leaves_the_cart_alone() {
        let s = session(
            FakeEd64Pro::new()
                .with_file("roms/a.z64", b"a")
                .with_file("roms/taken.z64", b"t"),
        );
        let kind = |from: &str, to: &str| s.rename_cart(from, to).unwrap_err().kind();
        assert_eq!(kind("roms/a.z64", "a.z64"), io::ErrorKind::InvalidInput);
        assert_eq!(
            kind("roms/a.z64", "roms/A.Z64"),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            kind("roms/a.z64", "roms/TAKEN.z64"),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(kind("roms/nope.z64", "roms/b.z64"), io::ErrorKind::NotFound);
        s.rename_cart("roms/a.z64", "roms/a.z64").unwrap();
        let fake = fake_of(s);
        assert_eq!(fake.file("roms/a.z64").unwrap(), b"a");
        assert_eq!(fake.file("roms/taken.z64").unwrap(), b"t");
    }
}
