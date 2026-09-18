//! Multi64 Test — run the end-to-end suite and show pass/fail.
//!
//! The suite itself lives in `multi64_test_connector::suite`, so this app and the
//! `multi64-test-connector suite` command run exactly the same checks. This file is only the
//! window: it starts a run, forwards each result to the page as it completes, and hands back the
//! summary.
//!
//! Everything is **linked, not spawned**. The earlier connector GUI shelled out to binaries in the
//! workspace's `target/` directory, with the path baked in at compile time, so it only ever worked
//! on a machine that had built the repo. This app is meant to be handed to someone along with the
//! Multi64 installer and the ROM, so it carries its own copy of every check, including the
//! direct-serial ones.

use multi64_test_connector::suite::{
    daemon_info, run_suite, CheckResult, DaemonInfo, SuiteOptions, SuiteSummary,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Emitted once per check, as it finishes, so a run fills in live rather than appearing at the end.
const CHECK_EVENT: &str = "suite-check";

/// What the page needs to fill its fields in before a run.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Defaults {
    ws_url: String,
    base_url: String,
    port: String,
    /// The ROM version this build was made alongside, from the test ROM's own header (see
    /// `build.rs`). Empty when it could not be read, in which case the check is skipped rather
    /// than asserted against a guess.
    expected_rom: String,
    /// Every phase a run reports, in order, so the page can lay the whole structure out before the
    /// run starts instead of growing headings as results arrive.
    phases: Vec<String>,
}

#[tauri::command]
fn defaults() -> Defaults {
    Defaults {
        ws_url: multi64_test_connector::suite::DEFAULT_WS_URL.into(),
        base_url: multi64_test_connector::suite::DEFAULT_BASE_URL.into(),
        port: multi64_test_connector::suite::DEFAULT_PORT.into(),
        expected_rom: env!("MULTI64_EXPECTED_ROM").into(),
        phases: multi64_test_connector::suite::PHASES
            .iter()
            .map(|p| (*p).to_string())
            .collect(),
    }
}

/// Ask the daemon which port it is actually using, so the page can fill that in rather than
/// offering a guess the tester would have to notice was wrong.
#[tauri::command]
async fn probe_daemon(base_url: String) -> Result<DaemonInfo, String> {
    // ureq is blocking, and this runs on Tauri's async runtime.
    tokio::task::spawn_blocking(move || daemon_info(&base_url))
        .await
        .map_err(|e| format!("probe task: {e}"))?
}

#[tauri::command]
async fn run(
    app: AppHandle,
    ws_url: String,
    base_url: String,
    port: String,
    expected_rom: String,
    skip_serial: bool,
) -> Result<SuiteSummary, String> {
    let opts = SuiteOptions {
        ws_url,
        base_url,
        port,
        // An empty box means "do not assert a version", which the suite reports as a skip. Blank
        // and absent are the same thing here, and neither is a pass.
        expect_rom: Some(expected_rom.trim().to_string()).filter(|s| !s.is_empty()),
        skip_serial,
        recv_timeout_secs: 5.0,
    };

    let mut on_result = |r: CheckResult| {
        // A failed emit means the window has gone; the run finishing is still worth doing, because
        // the serial port must be handed back either way.
        let _ = app.emit(CHECK_EVENT, &r);
    };

    run_suite(&opts, &mut on_result).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run_app() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![defaults, probe_daemon, run])
        .run(tauri::generate_context!())
        .expect("error while running Multi64 Test");
}
