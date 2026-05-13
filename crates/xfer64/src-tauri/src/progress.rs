//! Progress events for long Xfer64 operations (multi-file copy, delete, etc.).

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const EXPLORER_PROGRESS_EVENT: &str = "explorer-progress";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorerProgressPayload {
    /// Bytes completed / total bytes for the current operation (copy, etc.), or item counts for delete.
    pub done: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Ask the UI to reload the cart pane (preserves selection when in progress).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_cart: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_pc: Option<bool>,
}

pub fn emit_explorer_progress(app: &AppHandle, done: u64, total: u64) {
    emit_explorer_progress_full(app, done, total, None, None, None);
}

pub fn emit_explorer_progress_full(
    app: &AppHandle,
    done: u64,
    total: u64,
    message: Option<String>,
    refresh_cart: Option<bool>,
    refresh_pc: Option<bool>,
) {
    let _ = app.emit(
        EXPLORER_PROGRESS_EVENT,
        ExplorerProgressPayload {
            done,
            total,
            message,
            refresh_cart,
            refresh_pc,
        },
    );
}
