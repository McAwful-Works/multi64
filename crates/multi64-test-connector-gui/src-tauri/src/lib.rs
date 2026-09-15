//! Test ROM connector GUI — talks to **`multi64d`** only (no tray, no bundled daemon).
//! Serial **RAW_ECHO** tools (`sc64-echo-test`, `sc64-l3-framing-e2e`) spawn workspace `target/*` binaries.

use multi64_test_connector::{
    run_connector_command, run_controller_poll, run_listen, run_ws_raw_echo, ConnectorCommand,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

fn target_exe(tool: &str) -> Result<PathBuf, String> {
    let root = workspace_root();
    let ext = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    for profile in ["release", "debug"] {
        let p = root
            .join("target")
            .join(profile)
            .join(format!("{tool}{ext}"));
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "Could not find {tool}{ext} in target/release or target/debug under {}. Build with: cargo build -p {tool}",
        root.display()
    ))
}

/// The stop flag of one kind of long-running task (Listen, or Controller poll).
///
/// Each run gets a flag of its own (#150). One shared flag, reset to false on every Start, let a
/// Stop then Start inside the listener's 100 ms check or the poller's sleep un-stop the old run,
/// which then kept running beside the new one.
struct RunSlot {
    current: Mutex<Arc<AtomicBool>>,
}

impl Default for RunSlot {
    fn default() -> Self {
        Self {
            current: Mutex::new(Arc::new(AtomicBool::new(false))),
        }
    }
}

impl RunSlot {
    /// A fresh stop flag for a run about to start. The run it replaces, if still going, is stopped.
    fn start(&self) -> Arc<AtomicBool> {
        let fresh = Arc::new(AtomicBool::new(false));
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::mem::replace(&mut *current, Arc::clone(&fresh));
        previous.store(true, Ordering::SeqCst);
        fresh
    }

    /// Ask the current run to stop.
    fn stop(&self) {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        current.store(true, Ordering::SeqCst);
    }
}

/// Log why a run failed. Its errors used to be discarded, so a refused connection or a bad hello
/// made Start look like it did nothing (#150). A run logs its own success.
fn report_run_end<E: std::fmt::Display>(
    what: &str,
    result: Result<(), E>,
    log: &mut impl FnMut(String),
) {
    if let Err(e) = result {
        log(format!("{what} failed: {e:#}"));
    }
}

#[derive(Default)]
struct ListenState(RunSlot);

#[derive(Default)]
struct ControllerPollState(RunSlot);

#[tauri::command]
async fn run_command(
    url: String,
    recv_timeout_secs: f64,
    command: ConnectorCommand,
) -> Result<String, String> {
    let mut out = String::new();
    let mut log = |line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    run_connector_command(&url, recv_timeout_secs, &command, &mut log)
        .await
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[tauri::command]
async fn listen_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, ListenState>,
    url: String,
    duration_secs: f64,
) -> Result<(), String> {
    let stop = state.0.start();
    let app = app.clone();
    tokio::spawn(async move {
        let mut log = move |line: String| {
            let _ = app.emit("listen-log", line);
        };
        let result = run_listen(&url, duration_secs, Some(stop), &mut log).await;
        report_run_end("Listen", result, &mut log);
    });
    Ok(())
}

#[tauri::command]
fn listen_stop(state: tauri::State<'_, ListenState>) {
    state.0.stop();
}

#[tauri::command]
async fn controller_poll_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, ControllerPollState>,
    url: String,
    recv_timeout_secs: f64,
    interval_ms: u64,
) -> Result<(), String> {
    let stop = state.0.start();
    let app = app.clone();
    tokio::spawn(async move {
        let mut log = move |line: String| {
            let _ = app.emit("controller-poll-log", line);
        };
        let result =
            run_controller_poll(&url, recv_timeout_secs, interval_ms, Some(stop), &mut log).await;
        report_run_end("Controller poll", result, &mut log);
    });
    Ok(())
}

#[tauri::command]
fn controller_poll_stop(state: tauri::State<'_, ControllerPollState>) {
    state.0.stop();
}

#[tauri::command]
async fn sc64_echo_test(
    port: String,
    baud: u32,
    payload: String,
    timeout_secs: u64,
) -> Result<String, String> {
    let exe = target_exe("sc64-echo-test")?;
    let out = tokio::task::spawn_blocking(move || {
        Command::new(exe)
            .arg("--port")
            .arg(port)
            .arg("--baud")
            .arg(baud.to_string())
            .arg("--payload")
            .arg(payload)
            .arg("--timeout-secs")
            .arg(timeout_secs.to_string())
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("sc64-echo-test: {e}"))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        combined.push_str(&err);
    }
    if !out.status.success() {
        return Err(combined);
    }
    Ok(combined)
}

#[tauri::command]
async fn sc64_l3_framing_e2e(
    port: String,
    baud: u32,
    timeout_secs: u64,
    large: bool,
) -> Result<String, String> {
    let exe = target_exe("sc64-l3-framing-e2e")?;
    let out = tokio::task::spawn_blocking(move || {
        let mut c = Command::new(exe);
        c.arg("--port")
            .arg(port)
            .arg("--baud")
            .arg(baud.to_string())
            .arg("--timeout-secs")
            .arg(timeout_secs.to_string());
        if large {
            c.arg("--large");
        }
        c.output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("sc64-l3-framing-e2e: {e}"))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        combined.push_str(&err);
    }
    if !out.status.success() {
        return Err(combined);
    }
    Ok(combined)
}

#[tauri::command]
async fn ws_raw_echo_run(
    url: String,
    recv_timeout_secs: f64,
    payload_text: String,
    json_ping: bool,
) -> Result<String, String> {
    let mut out = String::new();
    let payload = payload_text.into_bytes();
    let mut log = |line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    match run_ws_raw_echo(&url, recv_timeout_secs, &payload, json_ping, &mut log).await {
        Ok(()) => Ok(out),
        Err(e) => {
            out.push_str(&format!("error: {e:#}"));
            Err(out)
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ListenState::default())
        .manage(ControllerPollState::default())
        .invoke_handler(tauri::generate_handler![
            run_command,
            listen_start,
            listen_stop,
            controller_poll_start,
            controller_poll_stop,
            sc64_echo_test,
            sc64_l3_framing_e2e,
            ws_raw_echo_run
        ])
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #150: Stop then Start inside the listener's 100 ms check reset the one shared flag, so the
    /// old run never saw Stop and kept running beside the new one.
    #[test]
    fn a_quick_stop_then_start_still_stops_the_old_run() {
        let slot = RunSlot::default();
        let first = slot.start();
        slot.stop();
        let second = slot.start();
        assert!(
            first.load(Ordering::SeqCst),
            "the stopped run must stay stopped"
        );
        assert!(
            !second.load(Ordering::SeqCst),
            "the new run must not start stopped"
        );
        slot.stop();
        assert!(second.load(Ordering::SeqCst));
    }

    /// Start while a run is still going replaces it rather than running two side by side.
    #[test]
    fn starting_again_stops_the_run_it_replaces() {
        let slot = RunSlot::default();
        let first = slot.start();
        let second = slot.start();
        assert!(first.load(Ordering::SeqCst));
        assert!(!second.load(Ordering::SeqCst));
    }

    /// #150: a refused connection or a bad hello used to end the task with nothing logged.
    #[test]
    fn a_failed_run_says_why() {
        let mut lines = Vec::new();
        report_run_end(
            "Listen",
            Err::<(), _>("connect WebSocket ws://127.0.0.1:38765/ws: refused"),
            &mut |l| lines.push(l),
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("Listen failed: "), "{lines:?}");
        assert!(lines[0].contains("refused"), "{lines:?}");

        let mut lines = Vec::new();
        report_run_end("Listen", Ok::<(), &str>(()), &mut |l| lines.push(l));
        assert!(lines.is_empty(), "the run logs its own success: {lines:?}");
    }
}
