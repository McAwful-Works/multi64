//! User-requested cancellation for long Xfer64 operations.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::State;

#[derive(Clone)]
pub struct ExplorerCancelState {
    inner: Arc<AtomicBool>,
}

impl Default for ExplorerCancelState {
    fn default() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ExplorerCancelState {
    pub fn reset(&self) {
        self.inner.store(false, Ordering::SeqCst);
    }

    pub fn request(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

#[tauri::command]
pub fn explorer_cancel_operation(state: State<'_, ExplorerCancelState>) {
    state.request();
}

/// Clears the cancel flag before a new multi-step copy (interactive per-file flow).
#[tauri::command]
pub fn explorer_reset_cancel(state: State<'_, ExplorerCancelState>) {
    state.reset();
}
