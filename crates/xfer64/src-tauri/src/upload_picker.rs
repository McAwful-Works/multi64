//! Minimal window: choose cart folder, then run the same import as headless `upload`.

use crate::cancel::ExplorerCancelState;
use crate::cart_serial_sd::ED64PRO_WRITE_CONSENT_MARKER;
use crate::cli_upload::{run_headless_import_upload, UploadImportSummary};
use std::path::PathBuf;
use tauri::{AppHandle, State};

const ED64PRO_PICKER_WARNING: &str = "Writing to an EverDrive-64 PRO is experimental. Xfer64's support for it is ported from Krikzz's published sources and has never been tested on a real cart, so a write could fail partway or damage files on the SD card.\n\nBack up anything important on the SD card first.\n\nUpload to this cart anyway?\n\nChoose OK to allow writes to this cart, or Cancel to stop.";

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
    cancel: State<'_, ExplorerCancelState>,
) -> Result<UploadImportSummary, String> {
    let paths = state.pc_paths.clone();
    if paths.is_empty() {
        return Err("No files to upload.".into());
    }
    // Clear any flag left by an earlier operation, then share this token with the transfer so
    // explorer_cancel_operation can actually stop it.
    let cancel = ExplorerCancelState::clone(&cancel);
    cancel.reset();
    let inner = tauri::async_runtime::spawn_blocking(move || {
        let run = |allow_ed64pro_writes: bool| {
            run_headless_import_upload(
                paths.clone(),
                cart_parent.clone(),
                overwrite,
                false,
                None,
                Some(app.clone()),
                cancel.clone(),
                allow_ed64pro_writes,
            )
        };
        // An EverDrive-64 PRO refuses writes until the user agrees. The session is only known once
        // it opens, so try without consent, and ask only if the cart turns out to be a PRO.
        match run(false) {
            Err(e) if e.contains(ED64PRO_WRITE_CONSENT_MARKER) => {
                let answer = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Warning)
                    .set_title("Allow writes to the EverDrive-64 PRO?")
                    .set_description(ED64PRO_PICKER_WARNING)
                    // Plain OK / Cancel: rfd is built without `common-controls-v6`, so custom
                    // labels would fall back to OK anyway. The warning text says what OK does.
                    .set_buttons(rfd::MessageButtons::OkCancel)
                    .show();
                if answer == rfd::MessageDialogResult::Ok {
                    run(true)
                } else {
                    Err("Cancelled".into())
                }
            }
            other => other,
        }
    })
    .await
    .map_err(|e| format!("upload task: {e}"))?;
    inner
}

#[tauri::command]
pub fn upload_picker_close(app: AppHandle) {
    app.exit(0);
}
