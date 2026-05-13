//! Test ROM connector GUI — talks to **`multi64d`** only (no tray, no bundled daemon).
//! Serial **RAW_ECHO** tools (`sc64-echo-test`, `sc64-l3-framing-e2e`) spawn workspace `target/*` binaries.

use multi64_test_connector::{
    run_connector_command, run_controller_poll, run_listen, run_ws_raw_echo, ConnectorCommand,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

struct ListenState {
    stop: Arc<AtomicBool>,
}

impl Default for ListenState {
    fn default() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct ControllerPollState {
    stop: Arc<AtomicBool>,
}

impl Default for ControllerPollState {
    fn default() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

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
    state.stop.store(false, Ordering::SeqCst);
    let stop = Arc::clone(&state.stop);
    let app = app.clone();
    tokio::spawn(async move {
        let mut log = move |line: String| {
            let _ = app.emit("listen-log", line);
        };
        let _ = run_listen(&url, duration_secs, Some(stop), &mut log).await;
    });
    Ok(())
}

#[tauri::command]
fn listen_stop(state: tauri::State<'_, ListenState>) {
    state.stop.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn controller_poll_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, ControllerPollState>,
    url: String,
    recv_timeout_secs: f64,
    interval_ms: u64,
) -> Result<(), String> {
    state.stop.store(false, Ordering::SeqCst);
    let stop = Arc::clone(&state.stop);
    let app = app.clone();
    tokio::spawn(async move {
        let mut log = move |line: String| {
            let _ = app.emit("controller-poll-log", line);
        };
        let _ =
            run_controller_poll(&url, recv_timeout_secs, interval_ms, Some(stop), &mut log).await;
    });
    Ok(())
}

#[tauri::command]
fn controller_poll_stop(state: tauri::State<'_, ControllerPollState>) {
    state.stop.store(true, Ordering::SeqCst);
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
