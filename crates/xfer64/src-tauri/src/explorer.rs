//! Windows folder access for the Xfer64 PC pane.

use crate::cancel::ExplorerCancelState;
use crate::copy_plan::{self, InteractiveCopyStep};
use crate::progress::{emit_explorer_progress, emit_explorer_progress_full};
use multi64_sc64_sd::write_host_file;
use serde::Serialize;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::AppHandle;
use tauri::Manager;
use tauri::State;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

#[cfg(windows)]
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;

fn is_hidden_fs(meta: &std::fs::Metadata, name: &str) -> bool {
    #[cfg(windows)]
    {
        let _ = name;
        meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        name.starts_with('.')
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: Option<u64>,
    pub hidden: bool,
}

fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified().ok().and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
    })
}

/// Directories first, then case-insensitive name — one `to_lowercase` per entry (not per comparison).
fn sort_fs_entries_dirs_then_name_case_insensitive(out: &mut Vec<FsEntry>) {
    if out.len() <= 1 {
        return;
    }
    let mut decorated: Vec<(bool, String, FsEntry)> = out
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

/// Cached sorted listing for the Windows pane (`fs_list_dir_page`). Cleared on PC FS mutations and when the app explicitly refreshes.
pub struct ExplorerPathCache {
    pc_list: Mutex<Option<(String, Vec<FsEntry>)>>,
}

impl ExplorerPathCache {
    pub fn new() -> Self {
        Self {
            pc_list: Mutex::new(None),
        }
    }

    pub fn invalidate_pc(&self) {
        if let Ok(mut g) = self.pc_list.lock() {
            *g = None;
        }
    }
}

impl Default for ExplorerPathCache {
    fn default() -> Self {
        Self::new()
    }
}

fn read_pc_dir_sorted(p: &Path) -> Result<Vec<FsEntry>, String> {
    let rd = std::fs::read_dir(p).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for e in rd {
        // A single unreadable entry must not fail the whole listing. metadata() follows links,
        // so a shortcut whose target is gone errors here; fall back to describing the link
        // itself, and skip only entries we cannot stat at all.
        let Ok(e) = e else { continue };
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => match std::fs::symlink_metadata(e.path()) {
                Ok(m) => m,
                Err(_) => continue,
            },
        };
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path().to_string_lossy().into_owned();
        let hidden = is_hidden_fs(&meta, &name);
        out.push(FsEntry {
            name,
            path,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified_ms: modified_ms(&meta),
            hidden,
        });
    }
    sort_fs_entries_dirs_then_name_case_insensitive(&mut out);
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsListDirPage {
    pub entries: Vec<FsEntry>,
    pub total: usize,
    pub offset: usize,
}

/// Paged directory read with in-Rust cache: subsequent pages for the same path reuse the sorted list (one `read_dir` per refresh).
#[tauri::command]
pub fn fs_list_dir_page(
    cache: State<'_, ExplorerPathCache>,
    path: String,
    offset: u32,
    limit: u32,
    fresh: bool,
) -> Result<FsListDirPage, String> {
    let p = PathBuf::from(path.trim());
    if !p.is_dir() {
        return Err("Not a directory or path does not exist.".into());
    }
    let key = p.to_string_lossy().into_owned();
    let off = offset as usize;
    let lim = if limit == 0 {
        2000usize
    } else {
        (limit as usize).clamp(1, 8192)
    };

    let need_load = {
        let lock = cache.pc_list.lock().map_err(|e| e.to_string())?;
        match lock.as_ref() {
            None => true,
            Some((k, _)) if k != &key => true,
            Some(_) if fresh && off == 0 => true,
            _ => false,
        }
    };

    if need_load {
        let entries = read_pc_dir_sorted(&p)?;
        let mut g = cache.pc_list.lock().map_err(|e| e.to_string())?;
        *g = Some((key, entries));
    }

    let lock = cache.pc_list.lock().map_err(|e| e.to_string())?;
    let (_, entries) = lock
        .as_ref()
        .ok_or_else(|| "Internal: PC list cache empty.".to_string())?;
    let total = entries.len();
    let page: Vec<FsEntry> = entries.iter().skip(off).take(lim).cloned().collect();
    Ok(FsListDirPage {
        entries: page,
        total,
        offset: off,
    })
}

/// Metadata for a single existing file or folder (PC pane — properties / context menu).
#[tauri::command]
pub fn fs_path_info(path: String) -> Result<FsEntry, String> {
    let p = PathBuf::from(path.trim());
    let meta = fs::metadata(&p).map_err(|e| e.to_string())?;
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.trim().to_string());
    let path_str = p.to_string_lossy().into_owned();
    let hidden = is_hidden_fs(&meta, &name);
    Ok(FsEntry {
        name,
        path: path_str,
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_ms: modified_ms(&meta),
        hidden,
    })
}

#[tauri::command]
pub fn fs_mkdir(path: String, cache: State<'_, ExplorerPathCache>) -> Result<(), String> {
    std::fs::create_dir_all(PathBuf::from(path.trim())).map_err(|e| e.to_string())?;
    cache.invalidate_pc();
    Ok(())
}

#[tauri::command]
pub fn fs_rename(
    from: String,
    to: String,
    cache: State<'_, ExplorerPathCache>,
) -> Result<(), String> {
    rename_refusing_to_replace(Path::new(from.trim()), Path::new(to.trim()))?;
    cache.invalidate_pc();
    Ok(())
}

/// Rename `from` to `to`, refusing when something else already has the name `to`.
///
/// `std::fs::rename` replaces an existing file on Windows without a word, so a rename onto a name
/// already in the folder used to delete that file. A name that differs only in letter case still
/// renames: Windows matches names case-insensitively, so `to` then "exists" as `from` itself.
fn rename_refusing_to_replace(from: &Path, to: &Path) -> Result<(), String> {
    if fs::symlink_metadata(to).is_ok() && !same_entry(from, to) {
        let name = to
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| to.display().to_string());
        return Err(format!(
            "\"{name}\" already exists in this folder. Choose another name."
        ));
    }
    fs::rename(from, to).map_err(|e| e.to_string())
}

/// Whether `a` and `b` name the same file or folder, as `a.z64` and `A.z64` do on Windows, or a
/// path through a `subst` drive, a junction or an 8.3 short name does with the path it stands
/// for: both are resolved to the file system's own final path and compared. Two hard links to one
/// file resolve to two paths, and are not caught.
pub(crate) fn same_entry(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn map_pc_copy_err(e: io::Error, ctx: &Path) -> String {
    if e.kind() == io::ErrorKind::Interrupted {
        "Cancelled".to_string()
    } else {
        format!("{}: {e}", ctx.display())
    }
}

fn copy_file_with_progress(
    src: &Path,
    dst: &Path,
    cancel: &ExplorerCancelState,
    mut on_delta: impl FnMut(u64),
) -> Result<(), String> {
    // The frontend drops a copy into the folder the file is already in, but compares path
    // strings, so another spelling of that folder got through. The file then came back from a
    // temporary copy of itself: the same bytes, but a new file in its place, so whatever belonged
    // to the file rather than its contents, such as alternate data streams or another hard link
    // to it, did not carry over. Before writes went through a temporary file, it was emptied.
    if same_entry(src, dst) {
        return Err(format!(
            "{} is the file being copied: a file cannot be copied onto itself.",
            dst.display()
        ));
    }
    let mut r = fs::File::open(src).map_err(|e| map_pc_copy_err(e, src))?;
    let mut buf = vec![0u8; 256 * 1024];
    // A read error belongs to `src`; anything else that fails, to `dst`.
    let mut src_err: Option<io::Error> = None;
    // Into a temporary file that replaces `dst` only once complete, as exports from the cart do:
    // a cancelled or failed Replace keeps the original.
    let res = write_host_file(dst, |w| loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) => {
                src_err = Some(e);
                return Err(io::Error::other("source read failed"));
            }
        };
        w.write_all(&buf[..n])?;
        on_delta(n as u64);
    });
    match (res, src_err) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(e)) => Err(map_pc_copy_err(e, src)),
        (Err(e), None) => Err(map_pc_copy_err(e, dst)),
    }
}

#[tauri::command]
pub fn build_fs_copy_plan(
    dest_dir: String,
    src_paths: Vec<String>,
) -> Result<Vec<InteractiveCopyStep>, String> {
    let dest = PathBuf::from(dest_dir.trim());
    let paths: Vec<PathBuf> = src_paths
        .iter()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.exists())
        .collect();
    copy_plan::build_fs_copy_plan(&paths, &dest)
}

fn fs_copy_one_file_sync(
    app: AppHandle,
    cancel: ExplorerCancelState,
    src: String,
    dest: String,
    overwrite: bool,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    let src = PathBuf::from(src.trim());
    let dest = PathBuf::from(dest.trim());
    if cancel.is_cancelled() {
        return Err("Cancelled".into());
    }
    if !src.is_file() {
        return Err("Source is not a file.".into());
    }
    if dest.exists() && dest.is_dir() {
        return Err("Cannot copy file over an existing folder.".into());
    }
    if dest.exists() && dest.is_file() && !overwrite {
        return Err("Destination exists and overwrite is false.".into());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut acc = progress_done_base;
    copy_file_with_progress(&src, &dest, &cancel, |n| {
        acc += n;
        emit_explorer_progress(&app, acc, progress_total);
    })?;
    Ok(())
}

/// Copy a single PC file into a destination path (used by interactive copy).
#[tauri::command]
pub async fn fs_copy_one_file(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    src: String,
    dest: String,
    overwrite: bool,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    let cancel = ExplorerCancelState::clone(&cancel);
    let app_block = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        fs_copy_one_file_sync(
            app_block,
            cancel,
            src,
            dest,
            overwrite,
            progress_done_base,
            progress_total,
        )
    })
    .await
    .map_err(|e| format!("copy task: {e}"))?;
    if res.is_ok() {
        if let Some(cache) = app.try_state::<ExplorerPathCache>() {
            cache.invalidate_pc();
        }
    }
    res
}

/// Emit progress for skipped files during interactive copy (JS-driven loop).
#[tauri::command]
pub fn explorer_emit_progress(app: AppHandle, done: u64, total: u64, message: Option<String>) {
    emit_explorer_progress_full(&app, done, total, message);
}

#[tauri::command]
pub fn fs_remove(path: String, cache: State<'_, ExplorerPathCache>) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    // symlink_metadata, not metadata: a shortcut whose target is missing would otherwise fail
    // here and could never be deleted, and following a link risks acting on the wrong thing.
    let meta = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        // Remove the link itself. Windows needs remove_dir for a directory link/junction.
        std::fs::remove_file(&p)
            .or_else(|_| std::fs::remove_dir(&p))
            .map_err(|e| e.to_string())?;
    } else if meta.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    cache.invalidate_pc();
    Ok(())
}

#[tauri::command]
pub fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn fs_parent(path: String) -> String {
    let p = PathBuf::from(path.trim());
    p.parent()
        .map(|x| x.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.trim().to_string())
}

#[tauri::command]
pub fn fs_user_dirs() -> serde_json::Value {
    let home = dirs::home_dir().map(|p| p.to_string_lossy().into_owned());
    let documents = dirs::document_dir().map(|p| p.to_string_lossy().into_owned());
    let desktop = dirs::desktop_dir().map(|p| p.to_string_lossy().into_owned());
    serde_json::json!({
        "home": home,
        "documents": documents,
        "desktop": desktop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "xfer64-explorer-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn names(&self) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Renaming onto a name the folder already has used to delete the file that had it.
    #[test]
    fn a_rename_onto_an_existing_name_is_refused_and_both_files_survive() {
        let dir = Scratch::new("rename-onto");
        let a = dir.0.join("a.z64");
        let b = dir.0.join("b.z64");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let err = rename_refusing_to_replace(&a, &b).unwrap_err();
        assert!(err.contains("\"b.z64\" already exists"), "{err}");
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"b");
    }

    /// Changing only the letter case is still a rename of the same file, on Windows too, where
    /// the new name already "exists".
    #[test]
    fn a_rename_that_changes_only_letter_case_goes_through() {
        let dir = Scratch::new("rename-case");
        let a = dir.0.join("game.z64");
        fs::write(&a, b"rom").unwrap();
        rename_refusing_to_replace(&a, &dir.0.join("GAME.z64")).unwrap();
        assert_eq!(dir.names(), ["GAME.z64"]);
        assert_eq!(fs::read(dir.0.join("GAME.z64")).unwrap(), b"rom");

        rename_refusing_to_replace(&dir.0.join("GAME.z64"), &dir.0.join("new.z64")).unwrap();
        assert_eq!(dir.names(), ["new.z64"]);
    }

    /// Choosing Replace and then Cancel used to truncate the existing file and then delete it.
    #[test]
    fn a_cancelled_replace_keeps_the_original() {
        let dir = Scratch::new("copy-cancel");
        let src = dir.0.join("src.z64");
        let dst = dir.0.join("dst.z64");
        // Several read chunks, so the cancel lands part-way.
        fs::write(&src, vec![7u8; 1024 * 1024]).unwrap();
        fs::write(&dst, b"the original").unwrap();
        let cancel = ExplorerCancelState::default();
        let err = copy_file_with_progress(&src, &dst, &cancel, |_| cancel.request()).unwrap_err();
        assert_eq!(err, "Cancelled");
        assert_eq!(fs::read(&dst).unwrap(), b"the original");
        assert_eq!(dir.names(), ["dst.z64", "src.z64"], "no temporary file");

        cancel.reset();
        copy_file_with_progress(&src, &dst, &cancel, |_| {}).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), vec![7u8; 1024 * 1024]);
    }

    /// A file is not copied onto itself, however the destination is spelled. `sub\..` stands in
    /// for a `subst` drive, a junction or an 8.3 name: a path string the frontend's comparison
    /// does not recognize as the file's own.
    ///
    /// Fail-first, demonstrated: without the check, the copy returned `Ok`, having replaced the
    /// file with a temporary copy of itself.
    #[test]
    fn a_file_is_not_copied_onto_itself_through_another_path() {
        let dir = Scratch::new("copy-self");
        fs::create_dir(dir.0.join("sub")).unwrap();
        let src = dir.0.join("game.z64");
        fs::write(&src, b"rom").unwrap();
        let alias = dir.0.join("sub").join("..").join("game.z64");
        let cancel = ExplorerCancelState::default();
        let err = copy_file_with_progress(&src, &alias, &cancel, |_| {}).unwrap_err();
        assert!(err.contains("cannot be copied onto itself"), "{err}");
        assert_eq!(fs::read(&src).unwrap(), b"rom");
        assert_eq!(dir.names(), ["game.z64", "sub"]);
    }
}
