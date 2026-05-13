//! Minimal window: choose cart folder, then run the same import as headless `upload`.

use crate::cli_upload::{run_headless_import_upload, UploadImportSummary};
use std::path::PathBuf;
use tauri::{AppHandle, State};

#[derive(Clone)]
pub struct UploadPickerState {
    pub pc_paths: Vec<PathBuf>,
}

#[tauri::command]
pub fn upload_picker_get_paths(state: State<UploadPickerState>) -> Vec<String> {
    state
        .pc_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[tauri::command]
pub async fn upload_picker_run(
    app: AppHandle,
    cart_parent: String,
    overwrite: bool,
    state: State<'_, UploadPickerState>,
) -> Result<UploadImportSummary, String> {
    let paths = state.pc_paths.clone();
    if paths.is_empty() {
        return Err("No files to upload.".into());
    }
    let inner = tauri::async_runtime::spawn_blocking(move || {
        run_headless_import_upload(paths, cart_parent, overwrite, false, None, Some(app))
    })
    .await
    .map_err(|e| format!("upload task: {e}"))?;
    inner
}

#[tauri::command]
pub fn upload_picker_close(app: AppHandle) {
    app.exit(0);
}
