//! Persistent settings + ring-buffer log for developer visibility (cart serial / SD sessions).
//!
//! Settings are stored under the OS config dir as `multi64/xfer64-settings.json` (legacy: `cart-explorer-settings.json`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const MAX_LINES: usize = 4000;
pub const EXPLORER_DEV_LOG_EVENT: &str = "explorer-dev-log";

fn default_cart_device() -> String {
    "auto".to_string()
}

/// `auto` — probe serial (SC64 identify, then EverDrive: edlink Gen3 ED64 or legacy `usb64` test). `sc64` — SummerCart64. `ed64_beta` — EverDrive; [`ed64_rom_linear_base`] overrides SD base when set (optional for edlink ED64 — default FCI `0x10000000`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorerSettingsSnapshot {
    #[serde(default)]
    pub developer_mode: bool,
    #[serde(default = "default_cart_device")]
    pub cart_device: String,
    /// Optional COM override (empty = auto). Persisted for CLI + shell integration.
    #[serde(default)]
    pub preferred_com: Option<String>,
    /// Cart folder (relative to SD root) used by `xfer64 upload` when `--to` is omitted.
    #[serde(default)]
    pub quick_upload_cart_path: String,
    /// Default for upload picker / Send to: replace files that already exist on the cart.
    #[serde(default)]
    pub quick_upload_overwrite: bool,
    /// EverDrive SD linear base for LBA 0 (`base + LBA·512` on usb64; same numeric base for edlink FCI when set). Optional when the cart speaks edlink Gen3 ED64.
    #[serde(default)]
    pub ed64_rom_linear_base: Option<u32>,
}

pub struct ExplorerSettingsState {
    inner: Mutex<ExplorerSettingsSnapshot>,
}

impl ExplorerSettingsState {
    pub fn load() -> Self {
        Self {
            inner: Mutex::new(read_settings_file()),
        }
    }

    pub fn snapshot(&self) -> ExplorerSettingsSnapshot {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_and_save(&self, s: ExplorerSettingsSnapshot) -> Result<(), String> {
        write_settings_file(&s)?;
        if let Ok(mut g) = self.inner.lock() {
            *g = s;
        }
        Ok(())
    }
}

impl Default for ExplorerSettingsSnapshot {
    fn default() -> Self {
        Self {
            developer_mode: false,
            cart_device: default_cart_device(),
            preferred_com: None,
            quick_upload_cart_path: String::new(),
            quick_upload_overwrite: false,
            ed64_rom_linear_base: None,
        }
    }
}

fn settings_dir() -> Result<PathBuf, String> {
    let mut dir = dirs::config_dir().ok_or("no config directory")?;
    dir.push("multi64");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(settings_dir()?.join("xfer64-settings.json"))
}

fn read_settings_file() -> ExplorerSettingsSnapshot {
    let Ok(base) = settings_dir() else {
        return ExplorerSettingsSnapshot::default();
    };
    let primary = base.join("xfer64-settings.json");
    let legacy = base.join("cart-explorer-settings.json");
    let from_file = |p: &PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    };
    if let Some(s) = from_file(&primary) {
        return s;
    }
    if let Some(s) = from_file(&legacy) {
        if let Ok(json) = serde_json::to_string_pretty(&s) {
            let _ = std::fs::write(&primary, json);
        }
        return s;
    }
    ExplorerSettingsSnapshot::default()
}

fn write_settings_file(s: &ExplorerSettingsSnapshot) -> Result<(), String> {
    let path = settings_path()?;
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[derive(Clone)]
pub struct ExplorerDevLog {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    /// When `None` (CLI / headless), no events are emitted to the webview.
    app: Option<AppHandle>,
    enabled: Mutex<bool>,
    lines: Mutex<Vec<String>>,
}

impl ExplorerDevLog {
    pub fn new(app: AppHandle) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                app: Some(app),
                enabled: Mutex::new(false),
                lines: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Headless upload (no webview; no Tauri events).
    pub fn new_without_app() -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                app: None,
                enabled: Mutex::new(false),
                lines: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn set_developer_mode(&self, on: bool) {
        if let Ok(mut g) = self.inner.enabled.lock() {
            *g = on;
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.enabled.lock().map(|g| *g).unwrap_or(false)
    }

    /// Append a line when developer mode is enabled; emit to [`EXPLORER_DEV_LOG_EVENT`].
    pub fn log(&self, msg: impl Into<String>) {
        let enabled = self.inner.enabled.lock().map(|g| *g).unwrap_or(false);
        if !enabled {
            return;
        }
        let text = msg.into();
        let ts = log_timestamp_ms();
        let line = format!("[{ts}] {text}");
        if let Ok(mut g) = self.inner.lines.lock() {
            if g.len() >= MAX_LINES {
                let drain = g.len() - MAX_LINES + 1;
                g.drain(0..drain);
            }
            g.push(line.clone());
        }
        if let Some(ref app) = self.inner.app {
            let _ = app.emit(EXPLORER_DEV_LOG_EVENT, line);
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.inner
            .lines
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lines.lock() {
            g.clear();
        }
    }
}

fn log_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[tauri::command]
pub fn explorer_get_settings(
    state: tauri::State<'_, ExplorerSettingsState>,
) -> ExplorerSettingsSnapshot {
    state.snapshot()
}

#[tauri::command]
pub fn explorer_set_settings(
    state: tauri::State<'_, ExplorerSettingsState>,
    dev: tauri::State<'_, ExplorerDevLog>,
    settings: ExplorerSettingsSnapshot,
) -> Result<(), String> {
    let mut merged = state.snapshot();
    merged.developer_mode = settings.developer_mode;
    merged.cart_device = settings.cart_device;
    merged.ed64_rom_linear_base = settings.ed64_rom_linear_base;
    state.set_and_save(merged)?;
    dev.set_developer_mode(settings.developer_mode);
    if settings.developer_mode {
        dev.log("settings saved (developer mode on)");
    }
    Ok(())
}

/// Persisted when the SD pane navigates so shell `upload` can target the same folder.
#[tauri::command]
pub fn explorer_set_quick_upload_cart_path(
    state: tauri::State<'_, ExplorerSettingsState>,
    path: String,
) -> Result<(), String> {
    let mut merged = state.snapshot();
    merged.quick_upload_cart_path = path.trim().replace('\\', "/");
    state.set_and_save(merged)?;
    Ok(())
}

/// Persisted default for the upload picker “overwrite” checkbox (Send to / quick upload UI).
#[tauri::command]
pub fn explorer_set_quick_upload_overwrite(
    state: tauri::State<'_, ExplorerSettingsState>,
    overwrite: bool,
) -> Result<(), String> {
    let mut merged = state.snapshot();
    merged.quick_upload_overwrite = overwrite;
    state.set_and_save(merged)?;
    Ok(())
}

#[tauri::command]
pub fn explorer_dev_log_get(dev: tauri::State<'_, ExplorerDevLog>) -> Vec<String> {
    dev.lines()
}

#[tauri::command]
pub fn explorer_dev_log_clear(dev: tauri::State<'_, ExplorerDevLog>) {
    dev.clear();
}

#[tauri::command]
pub fn explorer_open_dev_shell(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;

    let w = app
        .get_webview_window("dev-shell")
        .ok_or("dev-shell window missing (check tauri.conf.json)")?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

/// Hides the developer log window so the user can dismiss it if OS chrome misbehaves.
#[tauri::command]
pub fn explorer_close_dev_shell(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;

    if let Some(w) = app.get_webview_window("dev-shell") {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}
