//! Headless `xfer64 upload` for scripts; shared import path for the upload-picker window.

use crate::cancel::ExplorerCancelState;
use crate::cart_serial_sd::{self, ExplorerCartSerialState};
use crate::copy_plan;
use crate::daemon;
use crate::dev_log::{ExplorerDevLog, ExplorerSettingsState};
use crate::progress::emit_explorer_progress_full;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

fn path_label_for_progress_msg(path: &str) -> String {
    Path::new(path.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.trim().to_string())
}

/// Result of [`run_headless_import_upload`] (picker UI and CLI share the same import loop).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadImportSummary {
    pub uploaded: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone)]
pub struct UploadCliArgs {
    pub com: Option<String>,
    pub cart_parent: Option<String>,
    pub overwrite: bool,
    /// Show a GUI message when done (headless CLI only).
    pub notify: bool,
    /// Open minimal UI to choose cart folder (starts Tauri; does not run headless import here).
    pub picker: bool,
    pub paths: Vec<String>,
}

pub fn parse_upload_args() -> Result<UploadCliArgs, String> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("missing subcommand")?;
    if first != "upload" {
        return Err(format!("unknown subcommand: {first} (expected upload)"));
    }
    let mut com = None;
    let mut cart_parent = None;
    let mut overwrite = false;
    let mut notify = false;
    let mut picker = false;
    let mut paths = Vec::new();
    while let Some(a) = args.next() {
        if a == "--help" || a == "-h" {
            return Err(
                "usage: xfer64 upload [options] <files...>\n\
                 \n\
                 Upload files from the PC to the flash cart’s SD card over USB serial.\n\
                 \n\
                 --com COM        Serial port (default: settings file, or MULTI64_XFER64_COM, or auto)\n\
                 --to PATH        Cart folder relative to SD root (headless only; default: quick-upload path or root)\n\
                 --overwrite, -y  Replace existing files on the cart\n\
                 --notify           When headless: show a message when finished\n\
                 --picker           Choose destination folder in a small window (Send to / shell)\n"
                    .into(),
            );
        }
        if a == "--com" {
            com = Some(args.next().ok_or("--com requires a value")?);
        } else if a == "--to" {
            cart_parent = Some(args.next().ok_or("--to requires a value")?);
        } else if a == "--overwrite" || a == "-y" {
            overwrite = true;
        } else if a == "--notify" {
            notify = true;
        } else if a == "--picker" {
            picker = true;
        } else {
            paths.push(a);
        }
    }
    Ok(UploadCliArgs {
        com,
        cart_parent,
        overwrite,
        notify,
        picker,
        paths,
    })
}

/// Headless import used by CLI and by the upload-picker window.
/// When `app` is set, emits [`crate::progress::EXPLORER_PROGRESS_EVENT`] (bytes done / total) like the main explorer.
///
/// `cancel` is supplied by the caller so a UI can actually stop the transfer. It used to be
/// created here, which made it unshared and therefore inert: the picker window offered a Cancel
/// button that nothing could act on. The CLI has no UI to cancel from and passes a fresh one.
pub fn run_headless_import_upload(
    paths: Vec<PathBuf>,
    cart_parent: String,
    overwrite: bool,
    notify_on_success: bool,
    com_override: Option<String>,
    app: Option<AppHandle>,
    cancel: ExplorerCancelState,
) -> Result<UploadImportSummary, String> {
    let settings = ExplorerSettingsState::load();
    let snap = settings.snapshot();
    let st = ExplorerCartSerialState::new();
    if let Some(ref c) = com_override {
        *st.preferred_com.lock().map_err(|e| e.to_string())? = Some(c.trim().to_string());
    } else if let Ok(c) = std::env::var("MULTI64_XFER64_COM") {
        let t = c.trim();
        if !t.is_empty() {
            *st.preferred_com.lock().map_err(|e| e.to_string())? = Some(t.to_string());
        }
    } else if let Some(ref c) = snap.preferred_com {
        *st.preferred_com.lock().map_err(|e| e.to_string())? = Some(c.clone());
    }

    let cart_parent = cart_parent.trim().replace('\\', "/");

    if paths.is_empty() {
        return Err("No files to upload.".into());
    }
    for p in &paths {
        if !p.exists() {
            return Err(format!("Not found: {}", p.display()));
        }
    }

    let listen =
        std::env::var("MULTI64_DAEMON_LISTEN").unwrap_or_else(|_| "http://127.0.0.1:38765".into());
    let probe = daemon::explorer_daemon_probe_snapshot(&st, &snap, &listen)?;
    let released = probe.needs_yield && probe.up;
    if released {
        daemon::explorer_daemon_release_listen(&listen)?;
    }

    let dev = ExplorerDevLog::new_without_app();
    let notify_env = std::env::var("MULTI64_XFER64_UPLOAD_NOTIFY").unwrap_or_default();
    let notify_ok =
        notify_on_success || notify_env == "1" || notify_env.eq_ignore_ascii_case("true");

    let result = cart_serial_sd::with_session(&dev, "cli_upload", &st, &snap, |session| {
        let plan = copy_plan::build_pc_import_plan(session, &paths, &cart_parent)?;
        if plan.is_empty() {
            return Err("Nothing to copy (paths missing or not supported).".into());
        }
        let total_bytes: u64 = plan
            .iter()
            .filter(|s| s.mode == "import")
            .map(|s| s.bytes)
            .sum();
        let t_total = total_bytes.max(1);

        if let Some(ref app) = app {
            emit_explorer_progress_full(
                app,
                0,
                t_total,
                Some("Uploading to the SD card…".into()),
                None,
                None,
            );
        }

        let mut uploaded = 0u32;
        let mut skipped = 0u32;
        let mut done_base = 0u64;
        for step in plan {
            if cancel.is_cancelled() {
                return Err("Cancelled".into());
            }
            if step.mode != "import" {
                continue;
            }
            let src = PathBuf::from(
                step.src_pc
                    .as_ref()
                    .ok_or("internal: import step missing src_pc")?,
            );
            let cart_path = step
                .cart_path
                .as_ref()
                .ok_or("internal: import step missing cart_path")?;
            // Directory steps carry mode "import" too, so they must be handled before the file
            // path below -- otherwise import_pc_file_to_cart_in_session rejects them with
            // "Source is not a file." and the whole upload aborts on the first subfolder.
            if step.is_dir {
                match session.cart_path_entry_kind(cart_path) {
                    Ok(Some(true)) => {}
                    Ok(Some(false)) => {
                        return Err(
                            "Cannot copy folder over an existing file on the cart.".to_string()
                        )
                    }
                    _ => session
                        .mkdir_cart(cart_path)
                        .map_err(|e| format!("{cart_path}: {e}"))?,
                }
                continue;
            }
            // Re-check on the live session: the plan is built once, so later steps can target the
            // same cart path as an earlier import (same basename from different PC paths) and
            // would still carry `conflict_if_exists: false` from plan time.
            match session
                .cart_path_entry_kind(cart_path)
                .map_err(|e| e.to_string())?
            {
                Some(true) => {
                    return Err("Cannot copy file over an existing folder on the cart.".into());
                }
                Some(false) if !overwrite => {
                    eprintln!("skip (exists on cart): {cart_path}");
                    skipped += 1;
                    done_base += step.bytes;
                    if let Some(ref app) = app {
                        let label = path_label_for_progress_msg(cart_path);
                        emit_explorer_progress_full(
                            app,
                            done_base,
                            t_total,
                            Some(format!("Skipped (exists): \"{label}\"")),
                            None,
                            None,
                        );
                    }
                    continue;
                }
                _ => {}
            }
            let label = path_label_for_progress_msg(cart_path);
            let msg = format!("Uploading to the SD card — \"{label}\"…");

            if let Some(ref app) = app {
                emit_explorer_progress_full(app, done_base, t_total, Some(msg.clone()), None, None);
                cart_serial_sd::import_pc_file_to_cart_in_session(
                    session, &src, cart_path, overwrite, &cancel, app, done_base, t_total,
                )?;
                uploaded += 1;
                done_base += step.bytes;
                emit_explorer_progress_full(app, done_base, t_total, Some(msg), None, None);
            } else {
                cart_serial_sd::import_pc_file_to_cart_in_session_silent(
                    session, &src, cart_path, overwrite, &cancel,
                )?;
                uploaded += 1;
                done_base += step.bytes;
            }
            eprintln!("uploaded: {}  ->  {cart_path}", src.display());
        }
        Ok(UploadImportSummary { uploaded, skipped })
    });

    // Report a failed resume rather than discarding it: multi64d stays released and silently
    // ignores WebSocket writes, so a live L3 session dies with no diagnostic.
    if released {
        if let Err(e) = daemon::explorer_daemon_resume_listen(&listen) {
            eprintln!("warning: {e}");
            if let Some(ref app) = app {
                emit_explorer_progress_full(
                    app,
                    0,
                    1,
                    Some(format!(
                        "Upload finished, but the Multi64 bridge could not be resumed: {e}"
                    )),
                    None,
                    None,
                );
            }
        }
    }

    let summary = result?;
    if notify_ok {
        let description = if summary.uploaded == 0 && summary.skipped > 0 {
            if summary.skipped == 1 {
                "Nothing uploaded — that file is already on the SD card. Use --overwrite (-y) to replace it."
                    .to_string()
            } else {
                format!(
                    "Nothing uploaded — {} files are already on the SD card. Use --overwrite (-y) to replace them.",
                    summary.skipped
                )
            }
        } else if summary.skipped > 0 {
            format!(
                "Upload finished: {} uploaded, {} skipped (already on the SD card).",
                summary.uploaded, summary.skipped
            )
        } else {
            "Upload finished.".to_string()
        };
        let _ = rfd::MessageDialog::new()
            .set_title("Xfer64 upload")
            .set_description(description)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }
    Ok(summary)
}

pub fn run_cli_upload_from_args(args: UploadCliArgs) -> Result<(), String> {
    if args.picker {
        return Err("internal: picker mode must launch the app, not headless import".into());
    }
    let settings = ExplorerSettingsState::load();
    let snap = settings.snapshot();
    let cart_parent = args
        .cart_parent
        .unwrap_or_else(|| snap.quick_upload_cart_path.clone())
        .trim()
        .replace('\\', "/");

    let paths: Vec<PathBuf> = args.paths.iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        return Err(
            "No files given. Example: xfer64 upload \"C:\\\\roms\\\\game.z64\"\n\
             Run: xfer64 upload --help"
                .into(),
        );
    }

    let com = args.com.clone();
    run_headless_import_upload(
        paths,
        cart_parent,
        args.overwrite,
        args.notify,
        com,
        None,
        ExplorerCancelState::default(),
    )?;
    Ok(())
}

/// Run from `main` when argv is `xfer64 upload ...` (headless only). Does not start the Tauri UI.
pub fn run_cli_upload() -> Result<(), String> {
    let args = parse_upload_args()?;
    run_cli_upload_from_args(args)
}
