//! Windows folder access for the Xfer64 PC pane.

use crate::cancel::ExplorerCancelState;
use crate::copy_plan::{self, InteractiveCopyStep};
use crate::progress::{emit_explorer_progress, emit_explorer_progress_full};
use serde::Serialize;
use std::fs;
use std::io::{self, Read, Write};
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
        let e = e.map_err(|e| e.to_string())?;
        let meta = e.metadata().map_err(|e| e.to_string())?;
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

#[tauri::command]
pub fn fs_list_dir(path: String) -> Result<Vec<FsEntry>, String> {
    let p = PathBuf::from(path.trim());
    if !p.is_dir() {
        return Err("Not a directory or path does not exist.".into());
    }
    read_pc_dir_sorted(&p)
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
    std::fs::rename(PathBuf::from(from.trim()), PathBuf::from(to.trim()))
        .map_err(|e| e.to_string())?;
    cache.invalidate_pc();
    Ok(())
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
    let mut r = fs::File::open(src).map_err(|e| map_pc_copy_err(e, src))?;
    let mut w = fs::File::create(dst).map_err(|e| map_pc_copy_err(e, dst))?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel.is_cancelled() {
            drop(w);
            let _ = fs::remove_file(dst);
            return Err("Cancelled".into());
        }
        let n = r.read(&mut buf).map_err(|e| map_pc_copy_err(e, src))?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])
            .map_err(|e| map_pc_copy_err(e, dst))?;
        on_delta(n as u64);
    }
    Ok(())
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
    emit_explorer_progress_full(&app, done, total, message, None, None);
}

#[tauri::command]
pub fn fs_remove(path: String, cache: State<'_, ExplorerPathCache>) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if meta.is_dir() {
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
