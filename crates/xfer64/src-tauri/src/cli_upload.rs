//! Headless `xfer64 upload` for scripts; shared import path for the upload-picker window.

use crate::cancel::ExplorerCancelState;
use crate::cart_serial_sd::{self, ExplorerCartSerialState};
use crate::copy_plan;
use crate::daemon;
use crate::dev_log::{ExplorerDevLog, ExplorerSettingsState};
use crate::progress::{emit_explorer_progress, emit_explorer_progress_full};
use std::path::{Path, PathBuf};
use tauri::AppHandle;

fn path_label_for_progress_msg(path: &str) -> String {
    Path::new(path.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.trim().to_string())
}

/// "1 file" / "3 files" — the frontend's `countNoun`, for messages written here.
fn count_files(n: u32) -> String {
    format!("{n} {}", if n == 1 { "file" } else { "files" })
}

/// The `--notify` dialog text for a successful upload. Keep in step with `upload-picker.js`.
fn upload_summary_description(summary: &UploadImportSummary) -> String {
    match (summary.uploaded, summary.skipped) {
        // An empty folder copies and skips nothing; "Uploaded 0 files" would read as a failure.
        (0, 0) => "Upload finished.".to_string(),
        (0, 1) => {
            "Nothing uploaded — that file is already on the cart. Use --overwrite (-y) to replace it."
                .to_string()
        }
        (0, skipped) => format!(
            "Nothing uploaded — {skipped} files are already on the cart. Use --overwrite (-y) to replace them."
        ),
        (uploaded, 0) => format!("Uploaded {} to cart.", count_files(uploaded)),
        (uploaded, skipped) => format!(
            "Uploaded {} to cart. {} skipped (already on the cart).",
            count_files(uploaded),
            count_files(skipped)
        ),
    }
}

/// Whether to pause multi64d before opening the cart's serial port: whenever it answers, as the
/// main window does. Its `serialActive` is not enough: a link another Xfer64 has already released,
/// or one that faulted, reports false, and skipping the release then also skipped the resume,
/// leaving the bridge released once both were done.
fn should_release(probe: &daemon::DaemonProbe) -> bool {
    probe.up
}

/// Run `op` with multi64d paused when `release_needed`, then resume it.
///
/// Returns `op`'s result and, separately, why the resume failed, if it did. A resume is attempted
/// after every release attempt, including one that failed or timed out: multi64d may still apply
/// a late release, and nothing else would resume it. `op` never runs without a release.
fn with_daemon_released<T>(
    release_needed: bool,
    release: impl FnOnce() -> Result<(), String>,
    op: impl FnOnce() -> Result<T, String>,
    resume: impl FnOnce() -> Result<(), String>,
) -> (Result<T, String>, Option<String>) {
    if !release_needed {
        return (op(), None);
    }
    if let Err(e) = release() {
        return (Err(e), resume().err());
    }
    let result = op();
    (result, resume().err())
}

/// The upload's outcome, with a failed resume attached rather than discarded: multi64d stays
/// released and silently ignores WebSocket writes, so a live L3 session dies with no diagnostic.
/// A successful upload carries it as [`UploadImportSummary::resume_warning`]; a failed one appends
/// it to the error after a blank line.
fn attach_resume_failure(
    result: Result<UploadImportSummary, String>,
    resume_error: Option<String>,
) -> Result<UploadImportSummary, String> {
    let warning = resume_error.map(|e| {
        format!(
            "The Multi64 bridge was paused for this upload and could not be resumed ({e}). Use Restart bridge in Multi64."
        )
    });
    match (result, warning) {
        (Ok(summary), warning) => Ok(UploadImportSummary {
            resume_warning: warning,
            ..summary
        }),
        (Err(e), Some(warning)) => Err(format!("{e}\n\n{warning}")),
        (Err(e), None) => Err(e),
    }
}

/// Result of [`run_headless_import_upload`] (picker UI and CLI share the same import loop).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadImportSummary {
    pub uploaded: u32,
    pub skipped: u32,
    /// Set when the upload succeeded but multi64d could not be resumed afterwards. Quick upload
    /// shows it as a warning after the summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_warning: Option<String>,
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
    /// Allow writing to an EverDrive-64 PRO (experimental), which a UI would otherwise confirm.
    pub allow_ed64pro_writes: bool,
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
    let mut allow_ed64pro_writes = false;
    let mut paths = Vec::new();
    while let Some(a) = args.next() {
        if a == "--help" || a == "-h" {
            return Err(
                "usage: xfer64 upload [options] <files...>\n\
                 \n\
                 Upload files from this PC to the N64 flash cart's SD card over USB serial.\n\
                 \n\
                 --com COM        Serial port (default: settings file, or MULTI64_XFER64_COM, or auto)\n\
                 --to PATH        Cart folder relative to the cart root (headless only; default: Quick upload folder or root)\n\
                 --overwrite, -y  Replace existing files on the cart\n\
                 --notify           When headless: show a message when finished\n\
                 --picker           Choose destination folder in a small window (Send to / shell)\n\
                 --experimental-ed64pro-writes  Allow writing to an EverDrive-64 PRO (experimental; never tested on a cart)\n"
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
        } else if a == "--experimental-ed64pro-writes" {
            allow_ed64pro_writes = true;
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
        allow_ed64pro_writes,
        paths,
    })
}

/// Headless import used by CLI and by the upload-picker window.
/// When `app` is set, emits [`crate::progress::EXPLORER_PROGRESS_EVENT`] (bytes done / total) like the main explorer.
///
/// `cancel` is supplied by the caller so a UI can actually stop the transfer. It used to be
/// created here, which made it unshared and therefore inert: the picker window offered a Cancel
/// button that nothing could act on. The CLI has no UI to cancel from and passes a fresh one.
///
/// `allow_ed64pro_writes` must be true to write to an EverDrive-64 PRO: the picker passes it after
/// asking the user, the CLI only with `--experimental-ed64pro-writes`.
///
/// `listen` is the multi64d address to pause and resume. The picker passes the one its window uses
/// (and its folder listing already paused), so it matches the main window; the CLI, which has no
/// window, passes [`daemon::default_listen`].
#[allow(clippy::too_many_arguments)]
pub fn run_headless_import_upload(
    paths: Vec<PathBuf>,
    cart_parent: String,
    overwrite: bool,
    notify_on_success: bool,
    com_override: Option<String>,
    listen: String,
    app: Option<AppHandle>,
    cancel: ExplorerCancelState,
    allow_ed64pro_writes: bool,
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

    let probe = daemon::explorer_daemon_probe_snapshot(&st, &snap, &listen)?;

    let dev = ExplorerDevLog::new_without_app();
    let notify_env = std::env::var("MULTI64_XFER64_UPLOAD_NOTIFY").unwrap_or_default();
    let notify_ok =
        notify_on_success || notify_env == "1" || notify_env.eq_ignore_ascii_case("true");

    let upload = || {
        cart_serial_sd::with_session(&dev, "cli_upload", &st, &snap, |session| {
            cart_serial_sd::require_ed64pro_write_consent_for(session, allow_ed64pro_writes)?;
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
                    Some("Uploading to cart…".into()),
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
                                Some(format!("Uploading to cart — skipping \"{label}\"…")),
                                None,
                                None,
                            );
                        }
                        continue;
                    }
                    _ => {}
                }
                let label = path_label_for_progress_msg(cart_path);
                let msg = format!("Uploading to cart — \"{label}\"…");

                if let Some(ref app) = app {
                    emit_explorer_progress_full(
                        app,
                        done_base,
                        t_total,
                        Some(msg.clone()),
                        None,
                        None,
                    );
                    cart_serial_sd::import_pc_file_to_cart_in_session(
                        session,
                        &src,
                        cart_path,
                        overwrite,
                        &cancel,
                        |written| emit_explorer_progress(app, done_base + written, t_total),
                    )?;
                    uploaded += 1;
                    done_base += step.bytes;
                    emit_explorer_progress_full(app, done_base, t_total, Some(msg), None, None);
                } else {
                    cart_serial_sd::import_pc_file_to_cart_in_session(
                        session,
                        &src,
                        cart_path,
                        overwrite,
                        &cancel,
                        |_| {},
                    )?;
                    uploaded += 1;
                    done_base += step.bytes;
                }
                eprintln!("uploaded: {}  ->  {cart_path}", src.display());
            }
            Ok(UploadImportSummary {
                uploaded,
                skipped,
                resume_warning: None,
            })
        })
    };

    let (result, resume_error) = with_daemon_released(
        should_release(&probe),
        || daemon::explorer_daemon_release_listen(&listen),
        upload,
        || daemon::explorer_daemon_resume_listen(&listen),
    );
    let summary = attach_resume_failure(result, resume_error)?;
    if let Some(warning) = &summary.resume_warning {
        eprintln!("warning: {warning}");
    }
    if notify_ok {
        let (description, level) = match &summary.resume_warning {
            Some(warning) => (
                format!("{}\n\n{warning}", upload_summary_description(&summary)),
                rfd::MessageLevel::Warning,
            ),
            None => (
                upload_summary_description(&summary),
                rfd::MessageLevel::Info,
            ),
        };
        let _ = rfd::MessageDialog::new()
            .set_title("Xfer64 — Quick upload")
            .set_description(description)
            .set_level(level)
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
        daemon::default_listen(),
        None,
        ExplorerCancelState::default(),
        args.allow_ed64pro_writes,
    )
    .map_err(|e| {
        if e.contains(cart_serial_sd::ED64PRO_WRITE_CONSENT_MARKER) {
            format!("{e}\nRe-run with --experimental-ed64pro-writes to allow it.")
        } else {
            e
        }
    })?;
    Ok(())
}

/// Run from `main` when argv is `xfer64 upload ...` (headless only). Does not start the Tauri UI.
pub fn run_cli_upload() -> Result<(), String> {
    let args = parse_upload_args()?;
    run_cli_upload_from_args(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(uploaded: u32, skipped: u32) -> String {
        upload_summary_description(&UploadImportSummary {
            uploaded,
            skipped,
            resume_warning: None,
        })
    }

    /// A failed resume after Quick upload must reach the user as its own warning, not as a progress
    /// line the success message then replaces.
    #[test]
    fn a_failed_resume_is_kept_with_the_upload_outcome() {
        let ok = attach_resume_failure(
            Ok(UploadImportSummary {
                uploaded: 2,
                skipped: 0,
                resume_warning: None,
            }),
            Some("HTTP 500".to_string()),
        )
        .unwrap();
        assert_eq!(ok.uploaded, 2);
        let warning = ok.resume_warning.as_deref().unwrap();
        assert!(warning.contains("could not be resumed") && warning.contains("HTTP 500"));
        let json = serde_json::to_value(&ok).unwrap();
        assert_eq!(json["resumeWarning"], warning);

        let err =
            attach_resume_failure(Err("Cancelled".into()), Some("HTTP 500".into())).unwrap_err();
        assert!(err.starts_with("Cancelled\n\n") && err.contains("HTTP 500"));

        let clean = attach_resume_failure(
            Ok(UploadImportSummary {
                uploaded: 1,
                skipped: 0,
                resume_warning: None,
            }),
            None,
        )
        .unwrap();
        assert!(serde_json::to_value(&clean)
            .unwrap()
            .get("resumeWarning")
            .is_none());
    }

    /// The main window releases whenever multi64d answers; the CLI and Quick upload must too. A
    /// link another Xfer64 already released, or one that faulted, reports `serialActive: false`,
    /// and skipping the release then also skips the resume that would have restored the bridge.
    /// The probe carries neither `serialActive` nor a port, so only `up` can decide.
    #[test]
    fn a_reachable_daemon_is_released_even_with_no_active_link() {
        assert!(should_release(&daemon::DaemonProbe { up: true }));
        assert!(!should_release(&daemon::DaemonProbe { up: false }));
    }

    /// A release that failed may still apply later (a timed-out request the daemon finishes), so a
    /// resume is always attempted after one was tried, and the cart is never opened.
    #[test]
    fn resume_is_attempted_when_the_release_fails() {
        use std::cell::Cell;
        let ran = Cell::new(false);
        let resumed = Cell::new(0);
        let (result, warning) = with_daemon_released(
            true,
            || Err("POST http://127.0.0.1:38765/v1/serial/release: timeout".to_string()),
            || {
                ran.set(true);
                Ok(())
            },
            || {
                resumed.set(resumed.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("timeout"));
        assert!(warning.is_none(), "the resume itself succeeded");
        assert!(!ran.get(), "the cart must not be opened without a release");
        assert_eq!(resumed.get(), 1);
    }

    #[test]
    fn a_failed_resume_comes_back_beside_the_result() {
        let (result, warning) = with_daemon_released(
            true,
            || Ok(()),
            || Ok(7),
            || Err("connection refused".to_string()),
        );
        assert_eq!(result, Ok(7));
        assert!(warning.unwrap().contains("connection refused"));
    }

    #[test]
    fn nothing_is_released_or_resumed_when_the_daemon_is_down() {
        let (result, warning) =
            with_daemon_released(false, || panic!("released"), || Ok(1), || panic!("resumed"));
        assert_eq!(result, Ok(1));
        assert!(warning.is_none());
    }

    #[test]
    fn upload_summary_description_covers_each_case() {
        assert_eq!(describe(0, 0), "Upload finished.");
        assert_eq!(
            describe(0, 1),
            "Nothing uploaded — that file is already on the cart. Use --overwrite (-y) to replace it."
        );
        assert_eq!(
            describe(0, 3),
            "Nothing uploaded — 3 files are already on the cart. Use --overwrite (-y) to replace them."
        );
        assert_eq!(describe(1, 0), "Uploaded 1 file to cart.");
        assert_eq!(
            describe(2, 1),
            "Uploaded 2 files to cart. 1 file skipped (already on the cart)."
        );
    }
}
