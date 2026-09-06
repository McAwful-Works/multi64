//! Flash-cart SD over **USB serial** + host FAT/exFAT ([`multi64_sc64_sd`]).
//!
//! # Protocols
//!
//! - **SummerCart64** — `SD_READ` / `MEMORY_READ` via `multi64-sc64-link`.
//! - **EverDrive X-series** — experimental **`RomRead`** linear map when `ed64RomLinearBase` is set
//!   (`docs/spec/ed64-sd-usb-host.md`). Not Windows mass storage.
//!
//! # Layout
//!
//! - **[`ExplorerCartSerialState`]** — COM preference, probe cache, directory list cache.
//! - **[`with_session`]** — resolves cart role, opens [`CartSession`](multi64_sc64_sd::CartSession), runs work, closes.
//! - **Tauri commands** — `cart_serial_*` IPC (list, copy, mkdir, …).
//!
//! Maintainer map: workspace `docs/spec/xfer64-cart-serial.md`.

use crate::cancel::ExplorerCancelState;
use crate::cart_probe::DetectedCartKind;
use crate::copy_plan::{self, InteractiveCopyStep};
use crate::dev_log::{ExplorerDevLog, ExplorerSettingsSnapshot, ExplorerSettingsState};
use crate::explorer::ExplorerPathCache;
use crate::progress::{emit_explorer_progress, emit_explorer_progress_full};
use multi64_ed64_link as ed64_link;
use multi64_sc64_sd::{cart_path_parts, CartSession, Ed64SdSession, Sc64SdSession};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri::Manager;
use tauri::State;

fn map_cart_io_error(path_display: impl std::fmt::Display, e: io::Error) -> String {
    if e.kind() == io::ErrorKind::Interrupted {
        "Cancelled".to_string()
    } else {
        format!("{path_display}: {e}")
    }
}

fn map_usb_io_path(path: &Path, e: io::Error) -> String {
    map_cart_io_error(path.display(), e)
}

/// Last path segment for progress UI (cart-relative paths use `/`).
fn path_label_for_progress_msg(path: &str) -> String {
    Path::new(path.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.trim().to_string())
}

fn map_usb_io_cart(path: &str, e: io::Error) -> String {
    map_cart_io_error(path, e)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbFsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: Option<u64>,
    pub hidden: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDirPageResult {
    pub entries: Vec<UsbFsEntry>,
    pub total: usize,
    pub offset: usize,
    pub exfat: bool,
}

/// Directories first, then case-insensitive name — one `to_lowercase` per entry (matches PC pane sort).
/// Cached cart directory list: path key, entries, exFAT volume flag.
pub type CartDirListCache = Option<(String, Vec<UsbFsEntry>, bool)>;

fn sort_usb_entries_dirs_then_name_case_insensitive(out: &mut Vec<UsbFsEntry>) {
    if out.len() <= 1 {
        return;
    }
    let mut decorated: Vec<(bool, String, UsbFsEntry)> = out
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

pub struct ExplorerCartSerialState {
    /// When set, this COM port is used; when unset, [`cart_serial_suggest_port`] is used.
    pub preferred_com: Mutex<Option<String>>,
    /// Shared with spawned copy/delete tasks so auto-detect matches the main thread.
    pub probe_cache: Arc<Mutex<Option<(String, DetectedCartKind)>>>,
    /// Cached sorted listing for [`cart_serial_list_dir_page`] (same path + multiple chunks = one SD read per refresh).
    pub cart_list_cache: Arc<Mutex<CartDirListCache>>,
}

impl ExplorerCartSerialState {
    pub fn new() -> Self {
        Self {
            preferred_com: Mutex::new(None),
            probe_cache: Arc::new(Mutex::new(None)),
            cart_list_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn invalidate_probe_cache(&self) {
        if let Ok(mut g) = self.probe_cache.lock() {
            *g = None;
        }
    }

    pub fn invalidate_cart_list_cache(&self) {
        if let Ok(mut g) = self.cart_list_cache.lock() {
            *g = None;
        }
    }
}

/// Shown when EverDrive is selected but no linear ROM address is configured for SD access.
const ED64_BETA_SD_MSG: &str = "EverDrive needs a linear ROM address for SD access. Open Settings → EverDrive (advanced), then Scan for SD base or enter an address. \
Until then, only USB cart detection works—the SD card is not available here.";

const AUTO_DETECT_FAIL: &str = "Could not auto-detect the cart on this serial port. \
Choose SummerCart64 or EverDrive (beta) in Settings, or select another COM port.";

#[derive(Clone, Copy)]
enum CartSdRole {
    /// SummerCart64 — full USB SD session.
    Sc64,
    /// EverDrive with `ed64_rom_linear_base` set (experimental `RomRead` sector mapping).
    Ed64Linear,
    /// EverDrive without a configured linear base.
    Ed64NoLinear,
}

fn cart_mode(settings: &ExplorerSettingsSnapshot) -> &str {
    settings.cart_device.trim()
}

fn is_auto(settings: &ExplorerSettingsSnapshot) -> bool {
    let m = cart_mode(settings);
    m.is_empty() || m == "auto"
}

fn is_ed64_beta_setting(settings: &ExplorerSettingsSnapshot) -> bool {
    cart_mode(settings) == "ed64_beta"
}

fn probe_with_cache(
    st: &ExplorerCartSerialState,
    port: &str,
    force: bool,
) -> Result<DetectedCartKind, String> {
    let port = port.to_string();
    if !force {
        let g = st.probe_cache.lock().map_err(|e| e.to_string())?;
        if let Some((ref p, k)) = *g {
            if p == &port {
                return Ok(k);
            }
        }
    }
    let k = crate::cart_probe::probe_serial_cart(&port);
    if let Ok(mut g) = st.probe_cache.lock() {
        *g = Some((port, k));
    }
    Ok(k)
}

fn effective_sd_role(
    settings: &ExplorerSettingsSnapshot,
    st: &ExplorerCartSerialState,
) -> Result<CartSdRole, String> {
    let base = settings.ed64_rom_linear_base;
    match cart_mode(settings) {
        "ed64_beta" => Ok(if base.is_some() {
            CartSdRole::Ed64Linear
        } else {
            CartSdRole::Ed64NoLinear
        }),
        "sc64" => Ok(CartSdRole::Sc64),
        m if m.is_empty() || m == "auto" => {
            let port = resolve_port(st, settings)?;
            let k = probe_with_cache(st, &port, false)?;
            match k {
                DetectedCartKind::Sc64 => Ok(CartSdRole::Sc64),
                DetectedCartKind::Ed64Beta => Ok(if base.is_some() {
                    CartSdRole::Ed64Linear
                } else {
                    CartSdRole::Ed64NoLinear
                }),
                DetectedCartKind::Unknown => Err(AUTO_DETECT_FAIL.to_string()),
            }
        }
        _ => Ok(CartSdRole::Sc64),
    }
}

#[tauri::command]
pub fn cart_serial_list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// Prefer a COM port that matches the selected cart device (USB product/manufacturer), else a single USB serial port.
#[tauri::command]
pub fn cart_serial_suggest_port(settings: State<'_, ExplorerSettingsState>) -> Option<String> {
    suggest_port(&settings.snapshot())
}

fn usb_looks_ed64(prod: &str, man: &str) -> bool {
    prod.contains("everdrive")
        || prod.contains("ed64")
        || prod.contains("krikzz")
        || man.contains("krikzz")
        || man.contains("everdrive")
}

fn usb_looks_sc64(prod: &str, man: &str) -> bool {
    prod.contains("sc64")
        || prod.contains("summercart")
        || prod.contains("summer")
        || man.contains("summer")
}

fn suggest_port(settings: &ExplorerSettingsSnapshot) -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    let mode = cart_mode(settings);
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(u) = &p.port_type {
            let prod = u.product.as_deref().unwrap_or("").to_ascii_lowercase();
            let man = u.manufacturer.as_deref().unwrap_or("").to_ascii_lowercase();
            let matches = if mode == "ed64_beta" {
                usb_looks_ed64(&prod, &man)
            } else if mode == "sc64" {
                usb_looks_sc64(&prod, &man)
            } else {
                usb_looks_ed64(&prod, &man) || usb_looks_sc64(&prod, &man)
            };
            if matches {
                return Some(p.port_name.clone());
            }
        }
    }
    let usb: Vec<_> = ports
        .iter()
        .filter(|p| matches!(p.port_type, serialport::SerialPortType::UsbPort(_)))
        .collect();
    if usb.len() == 1 {
        return Some(usb[0].port_name.clone());
    }
    None
}

/// Auto mode: rank COM ports by USB metadata hints, then probe each for SC64/ED64 wire signatures.
/// Returns the first positively identified cart port.
fn detect_best_auto_port(
    st: &ExplorerCartSerialState,
    settings: &ExplorerSettingsSnapshot,
) -> Result<Option<String>, String> {
    let ports = serialport::available_ports().map_err(|e| e.to_string())?;
    if ports.is_empty() {
        return Ok(None);
    }
    let mut scored: Vec<(i32, String)> = ports
        .iter()
        .map(|p| {
            let mut score = 0i32;
            if let serialport::SerialPortType::UsbPort(u) = &p.port_type {
                let prod = u.product.as_deref().unwrap_or("").to_ascii_lowercase();
                let man = u.manufacturer.as_deref().unwrap_or("").to_ascii_lowercase();
                if usb_looks_sc64(&prod, &man) {
                    score += 40;
                }
                if usb_looks_ed64(&prod, &man) {
                    score += 40;
                }
                // Any USB serial device is a better auto candidate than unknown non-USB.
                score += 10;
            }
            (score, p.port_name.clone())
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    for (_, port) in scored {
        let kind = probe_with_cache(st, &port, true)?;
        if matches!(kind, DetectedCartKind::Sc64 | DetectedCartKind::Ed64Beta) {
            return Ok(Some(port));
        }
    }

    // If no signature match is found, fall back to metadata heuristic.
    Ok(suggest_port(settings))
}

/// Exposed for Xfer64 ↔ multi64d coordination ([`crate::daemon`]).
pub fn resolve_com_port(
    st: &ExplorerCartSerialState,
    settings: &ExplorerSettingsSnapshot,
) -> Result<String, String> {
    resolve_port(st, settings)
}

/// Ensures cart session close runs on panic (SD teardown + serial flush), not only on normal return.
struct SdSessionCloseGuard {
    session: Option<CartSession>,
    dev: ExplorerDevLog,
    context: &'static str,
}

impl Drop for SdSessionCloseGuard {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            if let Err(e) = s.close() {
                self.dev.log(format!(
                    "{}: SD session close dropped after panic or incomplete path: {e}",
                    self.context
                ));
            }
        }
    }
}

impl SdSessionCloseGuard {
    fn new(session: CartSession, dev: ExplorerDevLog, context: &'static str) -> Self {
        Self {
            session: Some(session),
            dev,
            context,
        }
    }

    fn session(&self) -> &CartSession {
        self.session.as_ref().expect("SD session")
    }

    fn close_explicit(mut self) -> Result<(), String> {
        let s = self.session.take().expect("SD session");
        s.close().map_err(|e| e.to_string())
    }
}

/// One cart file → PC path (used by export copy one + batch).
#[allow(clippy::too_many_arguments)] // Progress/cancel wiring matches Tauri copy commands.
fn export_cart_file_to_pc_in_session(
    session: &CartSession,
    cart_path: &str,
    dest: &Path,
    overwrite: bool,
    cancel: &ExplorerCancelState,
    app: &AppHandle,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    if dest.exists() && dest.is_dir() {
        return Err("Cannot copy file over an existing folder on the PC.".into());
    }
    if dest.exists() && dest.is_file() && !overwrite {
        return Err("Destination exists and overwrite is false.".into());
    }
    let mut acc = 0u64;
    session
        .copy_cart_entry_to_host_with_progress(cart_path, dest, false, |d| {
            acc += d;
            emit_explorer_progress(app, progress_done_base + acc, progress_total);
            !cancel.is_cancelled()
        })
        .map_err(|e| map_usb_io_cart(cart_path, e))
}

/// One PC file → cart path (used by import copy one + batch + CLI upload with progress).
#[allow(clippy::too_many_arguments)]
pub(crate) fn import_pc_file_to_cart_in_session(
    session: &CartSession,
    src: &Path,
    cart_dest_path: &str,
    overwrite: bool,
    cancel: &ExplorerCancelState,
    app: &AppHandle,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    if !src.is_file() {
        return Err("Source is not a file.".into());
    }
    let (parent, name) = cart_path_parts(cart_dest_path);
    match session
        .cart_path_entry_kind(cart_dest_path)
        .map_err(|e| e.to_string())?
    {
        Some(true) => {
            return Err("Cannot copy file over an existing folder on the cart.".into());
        }
        Some(false) if !overwrite => {
            return Err("Destination exists and overwrite is false.".into());
        }
        _ => {}
    }
    let mut acc = 0u64;
    session
        .import_from_pc_with_progress(src, &parent, &name, !overwrite, |d| {
            acc += d;
            emit_explorer_progress(app, progress_done_base + acc, progress_total);
            !cancel.is_cancelled()
        })
        .map_err(|e| map_usb_io_path(src, e))
}

/// PC → cart import without Tauri progress events (CLI / shell upload).
pub(crate) fn import_pc_file_to_cart_in_session_silent(
    session: &CartSession,
    src: &Path,
    cart_dest_path: &str,
    overwrite: bool,
    cancel: &ExplorerCancelState,
) -> Result<(), String> {
    if !src.is_file() {
        return Err("Source is not a file.".into());
    }
    let (parent, name) = cart_path_parts(cart_dest_path);
    match session
        .cart_path_entry_kind(cart_dest_path)
        .map_err(|e| e.to_string())?
    {
        Some(true) => {
            return Err("Cannot copy file over an existing folder on the cart.".into());
        }
        Some(false) if !overwrite => {
            return Err("Destination exists and overwrite is false.".into());
        }
        _ => {}
    }
    let mut acc = 0u64;
    session
        .import_from_pc_with_progress(src, &parent, &name, !overwrite, |d| {
            acc += d;
            !cancel.is_cancelled()
        })
        .map_err(|e| map_usb_io_path(src, e))
}

fn resolve_port(
    st: &ExplorerCartSerialState,
    settings: &ExplorerSettingsSnapshot,
) -> Result<String, String> {
    let pref = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    if let Some(p) = pref {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(p.to_string());
        }
    }
    let auto_port = if is_auto(settings) {
        detect_best_auto_port(st, settings)?
    } else {
        suggest_port(settings)
    };
    auto_port.ok_or_else(|| {
        if is_auto(settings) {
            "No matching cart port found: connect SC64/EverDrive or choose a COM port manually."
                .to_string()
        } else if is_ed64_beta_setting(settings) {
            "No serial port: plug in the EverDrive (USB) or choose a COM port.".to_string()
        } else {
            "No serial port: plug in your flash cart (USB) or choose a COM port.".to_string()
        }
    })
}

pub(crate) fn with_session<T, F>(
    dev: &ExplorerDevLog,
    context: &'static str,
    st: &ExplorerCartSerialState,
    settings: &ExplorerSettingsSnapshot,
    f: F,
) -> Result<T, String>
where
    F: FnOnce(&CartSession) -> Result<T, String>,
{
    let role = effective_sd_role(settings, st)?;
    if matches!(role, CartSdRole::Ed64NoLinear) {
        dev.log(format!(
            "{context}: skipped (EverDrive — no linear ROM base configured)"
        ));
        return Err(ED64_BETA_SD_MSG.to_string());
    }
    let port = resolve_port(st, settings)?;
    let session = match role {
        CartSdRole::Sc64 => {
            dev.log(format!("{context}: open SC64 SD session (port={port})"));
            CartSession::Sc64(Sc64SdSession::open(&port, 115200).map_err(|e| {
                let msg = e.to_string();
                dev.log(format!("{context}: SD session open FAILED: {msg}"));
                msg
            })?)
        }
        CartSdRole::Ed64Linear => {
            let base = settings.ed64_rom_linear_base.ok_or_else(|| {
                "internal: Ed64Linear role without ed64_rom_linear_base".to_string()
            })?;
            dev.log(format!(
                "{context}: open EverDrive SD session (port={port}, rom_linear_base=0x{base:08x})"
            ));
            CartSession::Ed64(Ed64SdSession::open(&port, 115200, base).map_err(|e| {
                let msg = e.to_string();
                dev.log(format!(
                    "{context}: EverDrive SD session open FAILED: {msg}"
                ));
                msg
            })?)
        }
        CartSdRole::Ed64NoLinear => unreachable!(),
    };
    let guard = SdSessionCloseGuard::new(session, dev.clone(), context);
    let out = f(guard.session());
    if let Err(e) = guard.close_explicit() {
        dev.log(format!(
            "{context}: SD session close FAILED: {e} (do not remove the card until close succeeds)"
        ));
        return if out.is_ok() {
            Err(format!(
                "SD session close failed (do not remove the card until this succeeds): {e}"
            ))
        } else {
            out
        };
    }
    dev.log(format!("{context}: SD session closed OK"));
    if let Err(ref err) = out {
        dev.log(format!("{context}: {err}"));
    }
    out
}

/// Reconstruct managed state for use on the blocking pool (see long-running commands below).
fn cart_serial_state_from_preferred(
    preferred_com: Option<String>,
    probe_cache: Arc<Mutex<Option<(String, DetectedCartKind)>>>,
    cart_list_cache: Arc<Mutex<CartDirListCache>>,
) -> ExplorerCartSerialState {
    ExplorerCartSerialState {
        preferred_com: Mutex::new(preferred_com),
        probe_cache,
        cart_list_cache,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbProbeStatus {
    pub resolved_port: String,
    pub mode: String,
    pub detected_kind: Option<String>,
    pub message: Option<String>,
}

/// Serial probe result for the UI (COM row hint). Manual modes skip the wire probe.
#[tauri::command]
pub fn cart_serial_probe_status(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
) -> Result<UsbProbeStatus, String> {
    let snap = settings.snapshot();
    let mode_raw = cart_mode(&snap).to_string();
    let mode = if mode_raw.is_empty() {
        "auto".to_string()
    } else {
        mode_raw.clone()
    };
    let port = resolve_port(&st, &snap)?;

    match mode.as_str() {
        "ed64_beta" => Ok(UsbProbeStatus {
            resolved_port: port,
            mode,
            detected_kind: Some("ed64".to_string()),
            message: Some("Manual: EverDrive (beta)".to_string()),
        }),
        "sc64" => Ok(UsbProbeStatus {
            resolved_port: port,
            mode,
            detected_kind: Some("sc64".to_string()),
            message: Some("Manual: SC64".to_string()),
        }),
        "auto" => {
            let k = probe_with_cache(&st, &port, true)?;
            let msg = if k == DetectedCartKind::Unknown {
                Some("Not detected — pick a manual cart type in Settings.".to_string())
            } else {
                None
            };
            Ok(UsbProbeStatus {
                resolved_port: port,
                mode,
                detected_kind: Some(k.as_str().to_string()),
                message: msg,
            })
        }
        _ => Ok(UsbProbeStatus {
            resolved_port: port,
            mode,
            detected_kind: Some("sc64".to_string()),
            message: None,
        }),
    }
}

#[tauri::command]
pub fn cart_serial_invalidate_probe_cache(
    st: State<'_, ExplorerCartSerialState>,
) -> Result<(), String> {
    st.invalidate_probe_cache();
    Ok(())
}

/// Hex strings for curated [`ed64_link::ED64_LINEAR_BASE_HINTS`] (UI: “addresses we try first”).
#[tauri::command]
pub fn cart_serial_ed64_linear_hint_bases() -> Vec<String> {
    ed64_link::ED64_LINEAR_BASE_HINTS
        .iter()
        .map(|v| format!("0x{v:08x}"))
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ed64LinearProbeResult {
    /// Bases whose first `RomRead` sector looked like disk sector 0 (MBR / FAT / exFAT heuristics).
    pub candidates: Vec<u32>,
    /// Distinct addresses tried (hints ∪ grid).
    pub bases_checked: usize,
    /// Same as [`cart_serial_ed64_linear_hint_bases`], for convenience after a scan.
    pub hint_bases_hex: Vec<String>,
}

/// Read sector 0 at many candidate ROM addresses over USB serial; returns bases that look like a boot sector.
/// Requires an EverDrive on the resolved COM port and Settings set to Auto-detect (EverDrive found) or EverDrive (beta).
#[tauri::command]
pub fn cart_serial_probe_ed64_linear_base(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    cancel: State<'_, ExplorerCancelState>,
) -> Result<Ed64LinearProbeResult, String> {
    let snap = settings.snapshot();
    let port = resolve_port(&st, &snap)?;
    let kind = probe_with_cache(&st, &port, true)?;
    let mode = cart_mode(&snap);
    let allow = match mode {
        "ed64_beta" => true,
        "auto" => kind == DetectedCartKind::Ed64Beta,
        _ => false,
    };
    if !allow {
        return Err(
            "Connect an EverDrive, choose Auto-detect or EverDrive-64 X7 (beta) in Settings, then try again."
                .into(),
        );
    }
    cancel.reset();
    let cancel = ExplorerCancelState::clone(&cancel);
    let hint_bases_hex = ed64_link::ED64_LINEAR_BASE_HINTS
        .iter()
        .map(|v| format!("0x{v:08x}"))
        .collect();
    let preferred = snap.ed64_rom_linear_base;
    let (candidates, bases_checked) =
        ed64_link::probe_ed64_sd_linear_bases_with_cancel(&port, 115200, preferred, || {
            !cancel.is_cancelled()
        })
        .map_err(|e| map_cart_io_error("SD base scan", e))?;
    Ok(Ed64LinearProbeResult {
        candidates,
        bases_checked,
        hint_bases_hex,
    })
}

/// Remember optional COM override (empty string = auto-detect).
#[tauri::command]
pub fn cart_serial_set_preferred_com(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    port: String,
) -> Result<(), String> {
    let mut g = st.preferred_com.lock().map_err(|e| e.to_string())?;
    let p = port.trim();
    *g = if p.is_empty() {
        None
    } else {
        Some(p.to_string())
    };
    let mut snap = settings.snapshot();
    snap.preferred_com = (*g).clone();
    settings.set_and_save(snap)?;
    st.invalidate_probe_cache();
    st.invalidate_cart_list_cache();
    Ok(())
}

/// Paged SD directory listing with in-Rust cache: subsequent pages for the same path reuse the sorted list (one SD read per refresh).
#[tauri::command]
pub fn cart_serial_list_dir_page(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    path: String,
    offset: u32,
    limit: u32,
    fresh: bool,
) -> Result<ListDirPageResult, String> {
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    let path = path.trim().replace('\\', "/");
    let key = path.clone();
    let off = offset as usize;
    let lim = if limit == 0 {
        2000usize
    } else {
        (limit as usize).clamp(1, 8192)
    };

    let need_load = {
        let g = st.cart_list_cache.lock().map_err(|e| e.to_string())?;
        match g.as_ref() {
            None => true,
            Some((k, _, _)) if k != &key => true,
            Some(_) if fresh && off == 0 => true,
            _ => false,
        }
    };

    if need_load {
        dev.log(format!("cart_serial_list_dir_page load path={path:?}"));
        let path_for_closure = path.clone();
        let key_for_cache = key.clone();
        let (mapped, exfat) =
            with_session(&dev, "cart_serial_list_dir_page", &st, &snap, |session| {
                let entries = session
                    .list_dir(&path_for_closure)
                    .map_err(|e| e.to_string())?;
                let exfat = session.is_exfat();
                let mut mapped: Vec<UsbFsEntry> = entries
                    .into_iter()
                    .map(|e| UsbFsEntry {
                        name: e.name,
                        path: e.path,
                        is_dir: e.is_dir,
                        size: e.size,
                        modified_ms: None,
                        hidden: e.hidden,
                    })
                    .collect();
                sort_usb_entries_dirs_then_name_case_insensitive(&mut mapped);
                Ok((mapped, exfat))
            })?;
        let mut g = st.cart_list_cache.lock().map_err(|e| e.to_string())?;
        *g = Some((key_for_cache, mapped, exfat));
    }

    let g = st.cart_list_cache.lock().map_err(|e| e.to_string())?;
    let (_, entries, exfat) = g
        .as_ref()
        .ok_or_else(|| "Internal: cart list cache empty.".to_string())?;
    let total = entries.len();
    let page: Vec<UsbFsEntry> = entries.iter().skip(off).take(lim).cloned().collect();
    Ok(ListDirPageResult {
        entries: page,
        total,
        offset: off,
        exfat: *exfat,
    })
}

/// Metadata for one cart path (parent list + name match), for the properties dialog.
#[tauri::command]
pub fn cart_serial_path_info(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    path: String,
) -> Result<UsbFsEntry, String> {
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    let path = path.trim().replace('\\', "/");
    with_session(&dev, "cart_serial_path_info", &st, &snap, |session| {
        let (parent, name) = cart_path_parts(&path);
        if name.is_empty() {
            return Err("Choose a file or folder.".into());
        }
        let entries = session.list_dir(&parent).map_err(|e| e.to_string())?;
        let entry = entries
            .into_iter()
            .find(|e| e.name == name || e.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| "Path not found.".to_string())?;
        Ok(UsbFsEntry {
            name: entry.name,
            path: entry.path,
            is_dir: entry.is_dir,
            size: entry.size,
            modified_ms: None,
            hidden: entry.hidden,
        })
    })
}

#[tauri::command]
pub fn build_cart_export_plan(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    cart_paths: Vec<String>,
    to_pc_parent: String,
) -> Result<Vec<InteractiveCopyStep>, String> {
    let paths: Vec<String> = cart_paths
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let to_pc = to_pc_parent.trim().to_string();
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    with_session(&dev, "build_cart_export_plan", &st, &snap, |session| {
        copy_plan::build_cart_export_plan(session, &paths, Path::new(&to_pc))
    })
}

#[tauri::command]
pub fn build_cart_import_plan(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    cart_parent: String,
    from_pc_paths: Vec<String>,
) -> Result<Vec<InteractiveCopyStep>, String> {
    let paths: Vec<PathBuf> = from_pc_paths
        .into_iter()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.exists())
        .collect();
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let cart_parent = cart_parent.trim().to_string();
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    with_session(&dev, "build_cart_import_plan", &st, &snap, |session| {
        copy_plan::build_pc_import_plan(session, &paths, &cart_parent)
    })
}

#[allow(clippy::too_many_arguments)] // Tauri injects many State/handle parameters
#[tauri::command]
pub async fn cart_serial_export_copy_one(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    cart_path: String,
    dest_pc_path: String,
    overwrite: bool,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    let cart_path = cart_path.trim().to_string();
    if cart_path.is_empty() {
        return Ok(());
    }
    let dest = PathBuf::from(dest_pc_path.trim());
    let cancel = ExplorerCancelState::clone(&cancel);
    let preferred_com = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    let probe_cache = Arc::clone(&st.probe_cache);
    let cart_list_cache = Arc::clone(&st.cart_list_cache);
    let snap = settings.snapshot();
    let app_block = app.clone();
    let dev = (*dev).clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let st = cart_serial_state_from_preferred(preferred_com, probe_cache, cart_list_cache);
        with_session(&dev, "cart_serial_export_copy_one", &st, &snap, |session| {
            if cancel.is_cancelled() {
                return Err("Cancelled".into());
            }
            export_cart_file_to_pc_in_session(
                session,
                &cart_path,
                &dest,
                overwrite,
                &cancel,
                &app_block,
                progress_done_base,
                progress_total,
            )
        })
    })
    .await
    .map_err(|e| format!("export task: {e}"))?;
    if res.is_ok() {
        if let Some(cache) = app.try_state::<ExplorerPathCache>() {
            cache.invalidate_pc();
        }
    }
    res
}

#[allow(clippy::too_many_arguments)] // Tauri injects many State/handle parameters
#[tauri::command]
pub async fn cart_serial_import_copy_one(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    src_pc_path: String,
    cart_dest_path: String,
    overwrite: bool,
    progress_done_base: u64,
    progress_total: u64,
) -> Result<(), String> {
    let src = PathBuf::from(src_pc_path.trim());
    let cart_dest_path = cart_dest_path.trim().to_string();
    if cart_dest_path.is_empty() {
        return Ok(());
    }
    if !src.is_file() {
        return Err("Source is not a file.".into());
    }
    let cancel = ExplorerCancelState::clone(&cancel);
    let preferred_com = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    let probe_cache = Arc::clone(&st.probe_cache);
    let cart_list_cache = Arc::clone(&st.cart_list_cache);
    let snap = settings.snapshot();
    let app = app.clone();
    let dev = (*dev).clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let st = cart_serial_state_from_preferred(preferred_com, probe_cache, cart_list_cache);
        with_session(&dev, "cart_serial_import_copy_one", &st, &snap, |session| {
            if cancel.is_cancelled() {
                return Err("Cancelled".into());
            }
            import_pc_file_to_cart_in_session(
                session,
                &src,
                &cart_dest_path,
                overwrite,
                &cancel,
                &app,
                progress_done_base,
                progress_total,
            )
        })
    })
    .await
    .map_err(|e| format!("import task: {e}"))?;
    if res.is_ok() {
        st.invalidate_cart_list_cache();
    }
    res
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCopyBatchItem {
    pub cart_path: String,
    pub dest_pc_path: String,
    pub overwrite: bool,
    pub progress_done_base: u64,
    pub progress_message: String,
    pub bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCopyBatchItem {
    pub src_pc_path: String,
    pub cart_dest_path: String,
    pub overwrite: bool,
    pub progress_done_base: u64,
    pub progress_message: String,
    pub bytes: u64,
}

/// Multi-file cart → PC copy in **one** SD session (open/close once). Progress matches per-file `cart_serial_export_copy_one` behavior.
#[tauri::command]
pub async fn cart_serial_export_copy_batch(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    items: Vec<ExportCopyBatchItem>,
    progress_total: u64,
) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }
    let cancel = ExplorerCancelState::clone(&cancel);
    let preferred_com = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    let probe_cache = Arc::clone(&st.probe_cache);
    let cart_list_cache = Arc::clone(&st.cart_list_cache);
    let snap = settings.snapshot();
    let app_block = app.clone();
    let dev = (*dev).clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let st = cart_serial_state_from_preferred(preferred_com, probe_cache, cart_list_cache);
        with_session(
            &dev,
            "cart_serial_export_copy_batch",
            &st,
            &snap,
            |session| {
                for item in items {
                    if cancel.is_cancelled() {
                        return Err("Cancelled".into());
                    }
                    let cart_path = item.cart_path.trim().to_string();
                    if cart_path.is_empty() {
                        continue;
                    }
                    let dest = PathBuf::from(item.dest_pc_path.trim());
                    let base = item.progress_done_base;
                    emit_explorer_progress_full(
                        &app_block,
                        base,
                        progress_total,
                        Some(item.progress_message.clone()),
                        None,
                        None,
                    );
                    export_cart_file_to_pc_in_session(
                        session,
                        &cart_path,
                        &dest,
                        item.overwrite,
                        &cancel,
                        &app_block,
                        base,
                        progress_total,
                    )?;
                    emit_explorer_progress_full(
                        &app_block,
                        base + item.bytes,
                        progress_total,
                        Some(item.progress_message),
                        None,
                        None,
                    );
                }
                Ok(())
            },
        )
    })
    .await
    .map_err(|e| format!("export batch task: {e}"))?;
    if res.is_ok() {
        if let Some(cache) = app.try_state::<ExplorerPathCache>() {
            cache.invalidate_pc();
        }
    }
    res
}

/// Multi-file PC → cart copy in **one** SD session.
#[tauri::command]
pub async fn cart_serial_import_copy_batch(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    items: Vec<ImportCopyBatchItem>,
    progress_total: u64,
) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }
    let cancel = ExplorerCancelState::clone(&cancel);
    let preferred_com = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    let probe_cache = Arc::clone(&st.probe_cache);
    let cart_list_cache = Arc::clone(&st.cart_list_cache);
    let snap = settings.snapshot();
    let app_block = app.clone();
    let dev = (*dev).clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let st = cart_serial_state_from_preferred(preferred_com, probe_cache, cart_list_cache);
        with_session(
            &dev,
            "cart_serial_import_copy_batch",
            &st,
            &snap,
            |session| {
                for item in items {
                    if cancel.is_cancelled() {
                        return Err("Cancelled".into());
                    }
                    let src = PathBuf::from(item.src_pc_path.trim());
                    let cart_dest_path = item.cart_dest_path.trim().to_string();
                    if cart_dest_path.is_empty() {
                        continue;
                    }
                    if !src.is_file() {
                        return Err("Source is not a file.".into());
                    }
                    let base = item.progress_done_base;
                    emit_explorer_progress_full(
                        &app_block,
                        base,
                        progress_total,
                        Some(item.progress_message.clone()),
                        None,
                        None,
                    );
                    import_pc_file_to_cart_in_session(
                        session,
                        &src,
                        &cart_dest_path,
                        item.overwrite,
                        &cancel,
                        &app_block,
                        base,
                        progress_total,
                    )?;
                    emit_explorer_progress_full(
                        &app_block,
                        base + item.bytes,
                        progress_total,
                        Some(item.progress_message),
                        None,
                        None,
                    );
                }
                Ok(())
            },
        )
    })
    .await
    .map_err(|e| format!("import batch task: {e}"))?;
    if res.is_ok() {
        st.invalidate_cart_list_cache();
    }
    res
}

#[cfg(test)]
mod batch_item_serde_tests {
    use super::*;

    #[test]
    fn export_copy_batch_item_deserializes_camel_case() {
        let j = r#"{"cartPath":"/a.bin","destPcPath":"C:\\x\\a.bin","overwrite":false,"progressDoneBase":0,"progressMessage":"m","bytes":99}"#;
        let v: ExportCopyBatchItem = serde_json::from_str(j).unwrap();
        assert_eq!(v.cart_path, "/a.bin");
        assert_eq!(v.bytes, 99);
    }

    #[test]
    fn import_copy_batch_item_deserializes_camel_case() {
        let j = r#"{"srcPcPath":"C:\\s.bin","cartDestPath":"/b.bin","overwrite":true,"progressDoneBase":1,"progressMessage":"m","bytes":2}"#;
        let v: ImportCopyBatchItem = serde_json::from_str(j).unwrap();
        assert_eq!(v.cart_dest_path, "/b.bin");
    }
}

#[tauri::command]
pub async fn cart_serial_remove_cart(
    app: AppHandle,
    cancel: State<'_, ExplorerCancelState>,
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut paths: Vec<String> = paths
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // Delete deeper paths first when multiple selections overlap (e.g. `a/b` and `a`).
    paths.sort_by(|a, b| {
        let depth = |s: &str| s.chars().filter(|c| *c == '/' || *c == '\\').count();
        depth(b).cmp(&depth(a))
    });
    let total = paths.len() as u32;
    if total == 0 {
        return Ok(());
    }
    cancel.reset();
    let cancel = ExplorerCancelState::clone(&cancel);
    let preferred_com = st.preferred_com.lock().map_err(|e| e.to_string())?.clone();
    let probe_cache = Arc::clone(&st.probe_cache);
    let cart_list_cache = Arc::clone(&st.cart_list_cache);
    let snap = settings.snapshot();
    let app = app.clone();
    let dev = (*dev).clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let n = total as usize;
        let start_msg = if n == 1 {
            format!(
                "Deleting from SD card — \"{}\"…",
                path_label_for_progress_msg(paths.first().map(|s| s.as_str()).unwrap_or(""))
            )
        } else {
            format!("Deleting from SD card — {n} items…")
        };
        emit_explorer_progress_full(&app, 0, total as u64, Some(start_msg), None, None);
        let st = cart_serial_state_from_preferred(preferred_com, probe_cache, cart_list_cache);
        with_session(&dev, "cart_serial_remove_cart", &st, &snap, |session| {
            for (i, p) in paths.iter().enumerate() {
                if cancel.is_cancelled() {
                    return Err("Cancelled".into());
                }
                dev.log(format!("cart_serial_remove_cart: deleting {p:?}"));
                let r = if dev.is_enabled() {
                    let d = dev.clone();
                    session.remove_cart_path_traced(p, &mut |line| {
                        d.log(format!("cart_serial_remove_cart: {line}"));
                    })
                } else {
                    session.remove_cart_path(p)
                };
                r.map_err(|e| format!("{p}: {e}"))?;
                let done = i + 1;
                let rem = n.saturating_sub(done);
                let label = path_label_for_progress_msg(p);
                let msg = if n == 1 {
                    format!("Deleting from SD card — \"{label}\"…")
                } else {
                    format!("Deleting from SD card — \"{label}\" ({done} of {n}, {rem} left)")
                };
                emit_explorer_progress_full(
                    &app,
                    done as u64,
                    total as u64,
                    Some(msg),
                    Some(true),
                    None,
                );
            }
            Ok(())
        })
    })
    .await
    .map_err(|e| format!("delete task: {e}"))?;
    if res.is_ok() {
        st.invalidate_cart_list_cache();
    }
    res
}

#[tauri::command]
pub fn cart_serial_mkdir_cart(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    path: String,
) -> Result<(), String> {
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    with_session(&dev, "cart_serial_mkdir_cart", &st, &snap, |session| {
        session.mkdir_cart(&path).map_err(|e| e.to_string())
    })?;
    st.invalidate_cart_list_cache();
    Ok(())
}

#[tauri::command]
pub fn cart_serial_rename_cart(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    dev: State<'_, ExplorerDevLog>,
    from: String,
    to: String,
) -> Result<(), String> {
    let snap = settings.snapshot();
    let dev = (*dev).clone();
    with_session(&dev, "cart_serial_rename_cart", &st, &snap, |session| {
        session.rename_cart(&from, &to).map_err(|e| e.to_string())
    })?;
    st.invalidate_cart_list_cache();
    Ok(())
}
