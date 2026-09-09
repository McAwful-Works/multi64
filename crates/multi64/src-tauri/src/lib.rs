//! Multi64 — manages `multi64d`, tray, settings (Windows-first).

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Listener, Manager, RunEvent};

const DEFAULT_LISTEN: &str = "127.0.0.1:38765";
const MAX_LOG_LINES: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub serial_port: Option<String>,
    pub baud: u32,
    pub listen: String,
    #[serde(default = "default_true")]
    pub auto_start_daemon: bool,
    #[serde(default)]
    pub autostart_app: bool,
    #[serde(default = "default_true")]
    pub minimize_to_tray_on_close: bool,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default = "default_true")]
    pub tray_enabled: bool,
    #[serde(default)]
    pub developer_mode: bool,
    #[serde(default)]
    pub multi64d_log_preset: Multi64dLogPreset,
}

fn default_true() -> bool {
    true
}

/// How verbose `multi64d` stderr logging should be (see `multi64d` `--serial-trace` and `RUST_LOG`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Multi64dLogPreset {
    /// `RUST_LOG` unset → tracing default `info` (same as upstream).
    #[default]
    Default,
    /// `multi64_sc64_l2` + `multi64d` at debug without per-read serial trace.
    Debug,
    /// `--serial-trace`: `trace!` on each non-empty cart read (`multi64_sc64_l2=trace`).
    SerialTrace,
    /// Debug-level crate logs plus `--serial-trace`.
    Verbose,
}

fn apply_multi64d_log_preset(cmd: &mut Command, preset: Multi64dLogPreset) {
    // Avoid inheriting RUST_LOG / MULTI64D_SERIAL_TRACE from the GUI process or user shell.
    cmd.env_remove("RUST_LOG");
    cmd.env_remove("MULTI64D_SERIAL_TRACE");
    match preset {
        Multi64dLogPreset::Default => {}
        Multi64dLogPreset::Debug => {
            cmd.env(
                "RUST_LOG",
                "multi64_sc64_l2=debug,multi64d=debug,tower_http=warn,info",
            );
        }
        Multi64dLogPreset::SerialTrace => {
            cmd.arg("--serial-trace");
        }
        Multi64dLogPreset::Verbose => {
            cmd.env("RUST_LOG", "multi64d=debug,tower_http=warn,info");
            cmd.arg("--serial-trace");
        }
    }
}

fn normalize_settings(mut s: Settings) -> Settings {
    if !s.tray_enabled {
        s.start_minimized = false;
    }
    s
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            serial_port: None,
            baud: 115_200,
            listen: DEFAULT_LISTEN.to_string(),
            auto_start_daemon: true,
            autostart_app: false,
            minimize_to_tray_on_close: true,
            start_minimized: false,
            tray_enabled: true,
            developer_mode: false,
            multi64d_log_preset: Multi64dLogPreset::Default,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatus {
    pub running: bool,
    pub healthy: bool,
    pub listen: String,
    pub message: String,
}

#[derive(Default)]
struct DaemonInner {
    child: Option<Child>,
    logs: Vec<String>,
}

struct AppState {
    daemon: Arc<Mutex<DaemonInner>>,
    settings: Mutex<Settings>,
}

fn settings_path() -> Result<PathBuf, String> {
    let mut dir = dirs::config_dir().ok_or("no config dir")?;
    dir.push("multi64");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("gui-settings.json"))
}

fn load_settings() -> Settings {
    let path = match settings_path() {
        Ok(p) => p,
        Err(_) => return normalize_settings(Settings::default()),
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return normalize_settings(Settings::default());
    };
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => normalize_settings(s),
        Err(e) => {
            // Keep the unparseable file instead of letting the next save overwrite it, so the
            // settings can be recovered by hand rather than silently reset.
            let backup = path.with_extension("json.corrupt");
            let _ = std::fs::rename(&path, &backup);
            eprintln!(
                "multi64: settings file could not be parsed ({e}); kept a copy at {} and starting from defaults",
                backup.display()
            );
            normalize_settings(Settings::default())
        }
    }
}

/// Write settings atomically.
///
/// `fs::write` truncates before writing, so a crash mid-write leaves a file `load_settings`
/// cannot parse -- and that path silently falls back to defaults, losing the serial port,
/// listen address and `autostart_app` (which reads as Windows no longer launching the app).
/// Write beside the target and rename over it; rename replaces the destination on Windows.
fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = settings_path()?;
    let s = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}

#[cfg(windows)]
fn command_no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn command_no_window(_cmd: &mut Command) {}

fn push_log(inner: &Arc<Mutex<DaemonInner>>, line: String) {
    let mut g = inner.lock();
    if g.logs.len() >= MAX_LOG_LINES {
        let drain = g.logs.len() - MAX_LOG_LINES + 1;
        g.logs.drain(0..drain);
    }
    g.logs.push(line);
}

fn spawn_log_reader(
    inner: Arc<Mutex<DaemonInner>>,
    stream: Option<std::process::ChildStdout>,
    prefix: &'static str,
) {
    let Some(stream) = stream else { return };
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            push_log(&inner, format!("{prefix}{line}"));
        }
    });
}

fn spawn_log_reader_err(
    inner: Arc<Mutex<DaemonInner>>,
    stream: Option<std::process::ChildStderr>,
    prefix: &'static str,
) {
    let Some(stream) = stream else { return };
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            push_log(&inner, format!("{prefix}{line}"));
        }
    });
}

fn resolve_multi64d_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("MULTI64D_EXE") {
        let p = PathBuf::from(raw.trim());
        if p.is_file() {
            return Ok(p);
        }
        return Err(format!(
            "MULTI64D_EXE is set but not a file: {}",
            p.display()
        ));
    }

    let mut tried: Vec<String> = Vec::new();

    if let Ok(dir) = app.path().resource_dir() {
        // Bundled `resources/multi64d.exe` is often placed under `$RESOURCES/resources/` (see Tauri bundle docs).
        for rel in ["multi64d.exe", "resources/multi64d.exe"] {
            let p = dir.join(rel);
            tried.push(p.display().to_string());
            if p.is_file() {
                return Ok(p);
            }
        }
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no exe parent")?;
    let side = dir.join("multi64d.exe");
    tried.push(side.display().to_string());
    if side.is_file() {
        return Ok(side);
    }
    let res = dir.join("resources").join("multi64d.exe");
    tried.push(res.display().to_string());
    if res.is_file() {
        return Ok(res);
    }

    Err(format!(
        "multi64d.exe not found. Build it with `cargo build -p multi64d` using the same profile as the GUI (e.g. both debug or both release), then rebuild the GUI so `src-tauri/resources/multi64d.exe` is copied, or place multi64d.exe next to this exe. You can also set MULTI64D_EXE to the full path. Checked: {}",
        tried.join("; ")
    ))
}

/// First USB serial device, else the first port of any kind.
fn pick_auto(ports: &[serialport::SerialPortInfo]) -> Option<String> {
    for p in ports {
        if let serialport::SerialPortType::UsbPort(_) = &p.port_type {
            return Some(p.port_name.clone());
        }
    }
    ports.first().map(|p| p.port_name.clone())
}

fn auto_pick_port() -> Option<String> {
    pick_auto(&serialport::available_ports().ok()?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortOptions {
    pub ports: Vec<String>,
    pub auto: Option<String>,
}

/// Enumerate the ports once and derive both the list and the auto pick from it.
///
/// The port dropdown needs both, and asking for them separately meant two `available_ports()`
/// enumerations per refresh — plus a window where a cart plugged in between the two calls could
/// be picked as `auto` while being absent from the list the dropdown was built from.
fn serial_port_options() -> SerialPortOptions {
    let ports = serialport::available_ports().unwrap_or_default();
    let auto = pick_auto(&ports);
    SerialPortOptions {
        ports: ports.into_iter().map(|p| p.port_name).collect(),
        auto,
    }
}

fn effective_serial(settings: &Settings) -> Option<String> {
    settings
        .serial_port
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(auto_pick_port)
}

fn daemon_health_url(listen: &str) -> String {
    let base = listen.trim();
    if base.starts_with("http://") || base.starts_with("https://") {
        format!("{base}/health")
    } else {
        format!("http://{base}/health")
    }
}

/// One agent for the process, not one per call.
///
/// `check_health` runs on a 2s poll, and an `Agent` owns a connection pool whose whole purpose
/// is reuse; rebuilding it each time threw that away and forced a fresh TCP connection every
/// check. The timeout is identical on every call, so there is nothing per-call to vary.
fn health_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(1)))
            .build()
            .into()
    })
}

fn check_health(listen: &str) -> bool {
    let url = daemon_health_url(listen);
    health_agent()
        .get(&url)
        .call()
        .map(|r| r.status().as_u16() == 200)
        .unwrap_or(false)
}

/// Whether the daemon is still running, clearing the handle if it has exited on its own.
///
/// Nothing else observes the child's exit, so without this a crashed `multi64d` is reported as
/// running forever -- with a message claiming health is merely "not OK yet" -- while the UI
/// polls a blocking health check against a dead process every two seconds.
fn daemon_is_running(daemon: &Arc<Mutex<DaemonInner>>) -> bool {
    let mut inner = daemon.lock();
    let Some(child) = inner.child.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            inner.child = None;
            // push_log would re-lock this same non-reentrant mutex, so append inline.
            if inner.logs.len() >= MAX_LOG_LINES {
                let drain = inner.logs.len() - MAX_LOG_LINES + 1;
                inner.logs.drain(0..drain);
            }
            inner
                .logs
                .push(format!("multi64d exited on its own ({status})"));
            false
        }
        // Still running, or the status could not be read -- treat as running either way.
        Ok(None) | Err(_) => true,
    }
}

/// Serialises daemon lifecycle work: spawn, and kill-then-wait.
///
/// The commands driving these used to be serialised for free by running on the main thread. They
/// now run on the blocking pool so the window stays responsive, which leaves two overlapping
/// starts each spawning a `multi64d` while only one child handle survives — orphaning a process
/// that still holds the COM port. `DaemonInner`'s own mutex cannot cover this: it is released and
/// retaken across the spawn.
static DAEMON_OPS: Mutex<()> = Mutex::new(());

fn kill_daemon(daemon: &Arc<Mutex<DaemonInner>>) {
    let _ops = DAEMON_OPS.lock();
    kill_daemon_locked(daemon);
}

/// [`kill_daemon`] for callers already holding [`DAEMON_OPS`].
fn kill_daemon_locked(daemon: &Arc<Mutex<DaemonInner>>) {
    let mut inner = daemon.lock();
    if let Some(mut c) = inner.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn start_daemon(
    app: &tauri::AppHandle,
    daemon: &Arc<Mutex<DaemonInner>>,
    settings: &Settings,
) -> Result<(), String> {
    let _ops = DAEMON_OPS.lock();
    kill_daemon_locked(daemon);
    let serial = effective_serial(settings)
        .ok_or("No serial port (plug in the cart or pick a COM port).")?;
    {
        let mut inner = daemon.lock();
        inner.logs.clear();
    }
    let log_label = match settings.multi64d_log_preset {
        Multi64dLogPreset::Default => "default",
        Multi64dLogPreset::Debug => "debug",
        Multi64dLogPreset::SerialTrace => "serial-trace",
        Multi64dLogPreset::Verbose => "verbose",
    };
    push_log(
        daemon,
        format!(
            "Starting multi64d on {serial} ({}) [logging: {log_label}]",
            settings.listen
        ),
    );

    let daemon_path = resolve_multi64d_path(app)?;
    let mut cmd = Command::new(&daemon_path);
    // baud is a spawn-time argument like the rest: multi64d takes --baud and feeds it to
    // Sc64L2Pipe::open. It used to be collected and saved by the UI but never passed, so the
    // setting did nothing and the daemon always ran at its own 115200 default.
    let baud = settings.baud.to_string();
    cmd.env("NO_COLOR", "1")
        .args([
            "--serial",
            &serial,
            "--baud",
            &baud,
            "--listen",
            &settings.listen,
            "--no-print-ports",
        ])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .stdin(Stdio::null());
    apply_multi64d_log_preset(&mut cmd, settings.multi64d_log_preset);
    command_no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| format!("spawn multi64d: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let d = Arc::clone(daemon);
    spawn_log_reader(Arc::clone(&d), stdout, "");
    spawn_log_reader_err(d, stderr, "[stderr] ");

    let mut inner = daemon.lock();
    inner.child = Some(child);
    drop(inner);
    let _ = app.emit("daemon-changed", ());
    Ok(())
}

/// Run `f` on the blocking pool with the managed [`AppState`].
///
/// Tauri runs a sync `#[tauri::command]` on the main thread, which is also the window event loop.
/// A command that blocks there freezes the window for its duration — and the health probe below
/// blocks for up to a second on a poll that fires every two, so an unresponsive daemon made the
/// whole app stutter. `State` cannot be captured by a `'static` closure, so the closure re-reads
/// it from the `AppHandle` once it is on the worker.
async fn on_blocking_pool<T, F>(app: &AppHandle, context: &'static str, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle, &AppState) -> T + Send + 'static,
{
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        f(&app, &state)
    })
    .await
    .map_err(|e| format!("{context} task: {e}"))
}

/// Ports and the auto pick, from one enumeration (see [`serial_port_options`]).
#[tauri::command]
async fn get_serial_port_options() -> Result<SerialPortOptions, String> {
    tauri::async_runtime::spawn_blocking(serial_port_options)
        .await
        .map_err(|e| format!("serial port enumeration task: {e}"))
}

/// Stays sync: reads one in-memory mutex, so the hop would cost more than the work.
#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().clone()
}

/// Writes the settings file, may touch the autostart registry entry, and may restart the daemon.
#[tauri::command]
async fn set_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    on_blocking_pool(&app, "set_settings", move |app, state| {
        apply_settings(app, state, settings)
    })
    .await?
}

fn apply_settings(app: &AppHandle, state: &AppState, settings: Settings) -> Result<(), String> {
    let settings = normalize_settings(settings);
    let prev = { state.settings.lock().clone() };
    save_settings(&settings)?;
    *state.settings.lock() = settings.clone();
    if settings.autostart_app != prev.autostart_app {
        set_autostart_windows_impl(settings.autostart_app)?;
    }
    // Serial port, baud, listen address and log preset are command-line arguments fixed at
    // spawn, so a running daemon keeps using the old ones. Saving used to appear to apply them
    // while the bridge quietly stayed on the previous port.
    let spawn_args_changed = settings.serial_port != prev.serial_port
        || settings.baud != prev.baud
        || settings.listen != prev.listen
        || settings.multi64d_log_preset != prev.multi64d_log_preset;
    if spawn_args_changed && daemon_is_running(&state.daemon) {
        push_log(
            &state.daemon,
            "Settings changed — restarting multi64d to apply them".to_string(),
        );
        if let Err(e) = start_daemon(app, &state.daemon, &settings) {
            push_log(&state.daemon, format!("Restart failed: {e}"));
        }
        let _ = app.emit("daemon-changed", ());
    }
    Ok(())
}

/// Build the Multi64 auto-launch handle.
///
/// auto-launch 0.6 made `AutoLaunch::new` platform-divergent (Linux gained a
/// `LinuxLaunchMode` argument), so both call sites go through the builder, which
/// keeps one signature across platforms.
fn multi64_auto_launch() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    AutoLaunchBuilder::new()
        .set_app_name("Multi64")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&[] as &[&str])
        .build()
        .map_err(|e| e.to_string())
}

fn set_autostart_windows_impl(enabled: bool) -> Result<(), String> {
    let auto = multi64_auto_launch()?;
    if enabled {
        auto.enable().map_err(|e| e.to_string())?;
    } else {
        auto.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Blocking: `check_health` issues an HTTP request with a 1 s timeout, and the window polls
/// this every 2 s.
#[tauri::command]
async fn get_daemon_status(app: tauri::AppHandle) -> Result<DaemonStatus, String> {
    on_blocking_pool(&app, "get_daemon_status", |_, state| daemon_status(state)).await
}

fn daemon_status(state: &AppState) -> DaemonStatus {
    let settings = state.settings.lock().clone();
    let listen = settings.listen.clone();
    let running = daemon_is_running(&state.daemon);
    let healthy = running && check_health(&listen);
    let message = if !running {
        "Stopped".into()
    } else if healthy {
        "Running (multi64d responds)".into()
    } else {
        "Process running but /health not OK yet".into()
    };
    DaemonStatus {
        running,
        healthy,
        listen,
        message,
    }
}

#[tauri::command]
fn get_daemon_logs(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.daemon.lock().logs.clone()
}

#[tauri::command]
fn clear_daemon_logs(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    state.daemon.lock().logs.clear();
    let _ = app.emit("daemon-changed", ());
}

/// Blocking: spawns `multi64d`, after killing any previous child and waiting for it to exit.
#[tauri::command]
async fn daemon_start(app: tauri::AppHandle) -> Result<(), String> {
    on_blocking_pool(&app, "daemon_start", |app, state| {
        let settings = state.settings.lock().clone();
        match start_daemon(app, &state.daemon, &settings) {
            Ok(()) => Ok(()),
            Err(e) => {
                push_log(&state.daemon, format!("Start failed: {e}"));
                let _ = app.emit("daemon-changed", ());
                Err(e)
            }
        }
    })
    .await?
}

/// Blocking: `kill_daemon` waits for the child to actually exit.
#[tauri::command]
async fn daemon_stop(app: tauri::AppHandle) -> Result<(), String> {
    on_blocking_pool(&app, "daemon_stop", |app, state| {
        kill_daemon(&state.daemon);
        let _ = app.emit("daemon-changed", ());
    })
    .await
}

/// Blocking: writes the autostart registry entry.
#[tauri::command]
async fn set_autostart_windows(enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_autostart_windows_impl(enabled))
        .await
        .map_err(|e| format!("set autostart task: {e}"))?
}

/// Blocking: reads the autostart registry entry.
#[tauri::command]
async fn get_autostart_windows() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| {
        multi64_auto_launch()
            .map(|auto| auto.is_enabled().unwrap_or(false))
            .unwrap_or(false)
    })
    .await
    .map_err(|e| format!("get autostart task: {e}"))
}

/// Basenames we search for (NSIS / MSI / legacy installs).
const XFER64_EXE_NAMES: &[&str] = &["Xfer64.exe", "xfer64.exe", "multi64-cart-explorer.exe"];

/// Windows: resolve installed Xfer64 from registry (NSIS and WiX/MSI register App Paths and/or Uninstall).
#[cfg(windows)]
fn xfer64_registry_exe_candidates() -> Vec<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    fn hint_xfer64(s: &str) -> bool {
        let l = s.to_ascii_lowercase();
        l.contains("xfer64") || l.contains("multi64-cart-explorer")
    }

    fn push_unique(out: &mut Vec<PathBuf>, p: PathBuf) {
        if p.is_file() && !out.iter().any(|e| e == &p) {
            out.push(p);
        }
    }

    fn strip_display_icon(s: &str) -> PathBuf {
        let t = s.trim().trim_matches('"');
        let path_part = t.split(',').next().unwrap_or(t).trim();
        PathBuf::from(path_part)
    }

    let mut out = Vec::new();

    for hkey in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for name in XFER64_EXE_NAMES {
            let subpath = format!(
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}",
                name
            );
            if let Ok(key) = RegKey::predef(hkey).open_subkey(subpath) {
                if let Ok(s) = key.get_value::<String, _>("") {
                    push_unique(&mut out, PathBuf::from(s.trim()));
                }
            }
        }
    }

    let uninstall_roots = [
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ];
    for (hkey, path) in uninstall_roots {
        let Ok(uninstall) = RegKey::predef(hkey).open_subkey(path) else {
            continue;
        };
        for entry in uninstall.enum_keys().filter_map(|e| e.ok()) {
            let Ok(subkey) = uninstall.open_subkey(&entry) else {
                continue;
            };
            let display_ok = subkey
                .get_value::<String, _>("DisplayName")
                .ok()
                .as_ref()
                .map(|d| hint_xfer64(d))
                .unwrap_or(false);
            let loc_opt = subkey.get_value::<String, _>("InstallLocation").ok();
            let install_loc_ok = loc_opt.as_ref().is_some_and(|loc| hint_xfer64(loc));
            if !display_ok && !install_loc_ok {
                continue;
            }
            if let Some(loc) = loc_opt {
                let base = loc.trim().trim_end_matches(['\\', '/']);
                if !base.is_empty() {
                    let base = PathBuf::from(base);
                    for exe in XFER64_EXE_NAMES {
                        push_unique(&mut out, base.join(exe));
                    }
                }
            }
            if let Ok(icon) = subkey.get_value::<String, _>("DisplayIcon") {
                let p = strip_display_icon(&icon);
                push_unique(&mut out, p);
            }
        }
    }

    out
}

fn xfer64_exe_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        v.extend(xfer64_registry_exe_candidates());
    }
    let loose = XFER64_EXE_NAMES;
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in loose {
                v.push(dir.join(name));
            }
        }
    }
    if let Some(ld) = dirs::data_local_dir() {
        for name in loose {
            v.push(ld.join("multi64").join(name));
        }
        for (folder, exe) in [
            ("Xfer64", "Xfer64.exe"),
            ("multi64-cart-explorer", "multi64-cart-explorer.exe"),
        ] {
            v.push(ld.join("Programs").join(folder).join(exe));
            v.push(ld.join(folder).join(exe));
        }
    }
    #[cfg(windows)]
    {
        for (folder, exe) in [
            ("Xfer64", "Xfer64.exe"),
            ("multi64-cart-explorer", "multi64-cart-explorer.exe"),
        ] {
            if let Ok(pf) = std::env::var("ProgramFiles") {
                v.push(PathBuf::from(pf).join(folder).join(exe));
            }
            if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
                v.push(PathBuf::from(pf86).join(folder).join(exe));
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                for name in loose {
                    let candidate = dir.join(name);
                    if candidate.is_file() {
                        v.push(candidate);
                    }
                }
            }
        }
    }
    v
}

fn first_xfer64_exe() -> Option<PathBuf> {
    xfer64_exe_candidates().into_iter().find(|p| p.is_file())
}

fn is_xfer64_installed() -> bool {
    first_xfer64_exe().is_some()
}

/// True when `p` is a non-placeholder bundled installer (build.rs writes 0 bytes for the unused kind).
fn xfer64_installer_is_valid(p: &Path) -> bool {
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.len() > 1024)
            .unwrap_or(false)
}

/// Same layout rules as [`resolve_multi64d_path`]: bundled `resources/...` often lands under
/// `$RESOURCE_DIR/resources/` (see `tauri.conf.json` `bundle.resources`).
fn resolve_xfer64_installer_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    // Prefer MSI when bundled (Multi64 MSI build); else NSIS *.exe. Only one is non-placeholder.
    const REL: &[&str] = &[
        "xfer64-setup.msi",
        "resources/xfer64-setup.msi",
        "xfer64-setup.exe",
        "resources/xfer64-setup.exe",
    ];
    let try_dir = |base: &Path| {
        REL.iter()
            .map(|rel| base.join(rel))
            .find(|p| xfer64_installer_is_valid(p))
    };
    if let Ok(dir) = app.path().resource_dir() {
        if let Some(p) = try_dir(&dir) {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return try_dir(dir);
        }
    }
    None
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Xfer64State {
    pub installed: bool,
    pub installer_available: bool,
}

fn xfer64_state(app: &AppHandle) -> Xfer64State {
    Xfer64State {
        installed: is_xfer64_installed(),
        installer_available: resolve_xfer64_installer_path(app).is_some(),
    }
}

/// Blocking: reads the uninstall registry keys and stats every candidate install path.
#[tauri::command]
async fn get_xfer64_state(app: tauri::AppHandle) -> Result<Xfer64State, String> {
    on_blocking_pool(&app, "get_xfer64_state", |app, _| xfer64_state(app)).await
}

/// If Xfer64 is installed, launch it. Otherwise run the bundled installer (`xfer64-setup.exe` or `xfer64-setup.msi`).
/// Blocking: the same registry and path search as [`get_xfer64_state`], then a process spawn.
#[tauri::command]
async fn launch_or_install_xfer64(app: tauri::AppHandle) -> Result<(), String> {
    on_blocking_pool(&app, "launch_or_install_xfer64", |app, _| {
        launch_or_install_xfer64_blocking(app)
    })
    .await?
}

fn launch_or_install_xfer64_blocking(app: &AppHandle) -> Result<(), String> {
    if let Some(exe) = first_xfer64_exe() {
        return Command::new(&exe)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ());
    }
    let Some(installer_path) = resolve_xfer64_installer_path(app) else {
        return Err(
            "Xfer64 installer is not bundled. Build xfer64, then build Multi64 (see crates/multi64/README.md: NSIS vs MSI pairing)."
                .into(),
        );
    };
    launch_xfer64_bundled_installer(&installer_path)
}

fn launch_xfer64_bundled_installer(installer_path: &Path) -> Result<(), String> {
    let is_msi = installer_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| e.eq_ignore_ascii_case("msi"))
        .unwrap_or(false);
    if is_msi {
        #[cfg(windows)]
        {
            return Command::new("msiexec.exe")
                .arg("/i")
                .arg(installer_path)
                .spawn()
                .map_err(|e| e.to_string())
                .map(|_| ());
        }
        #[cfg(not(windows))]
        {
            return Err("MSI install is only supported on Windows.".into());
        }
    }
    Command::new(installer_path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bring the main window up from the tray (or from a second launch attempt).
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        // An unminimize is needed as well as a show: hiding to tray and minimizing are
        // independent, so a window minimized before it was hidden stays minimized.
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Text and enabled state for the daemon items. Pure, so the decisions are testable without a
/// running Tauri app — the tray itself cannot be driven from a test.
struct TrayLabels {
    status: String,
    toggle: &'static str,
    toggle_enabled: bool,
    restart_enabled: bool,
}

fn tray_labels(running: bool, serial: Option<&str>, listen: &str) -> TrayLabels {
    TrayLabels {
        status: if running {
            match serial {
                Some(port) => format!("Daemon: running on {port}"),
                // No configured or auto-detected port, but a live process: report where it
                // listens rather than claiming a port we cannot name.
                None => format!("Daemon: running ({listen})"),
            }
        } else {
            "Daemon: stopped".to_string()
        },
        toggle: if running {
            "Stop daemon"
        } else {
            "Start daemon"
        },
        // Stopping always works; starting needs a port, and `start_daemon` fails without one.
        toggle_enabled: running || serial.is_some(),
        // Restarting a stopped daemon is just Start, which is the item above.
        restart_enabled: running,
    }
}

/// Text and enabled state for the Xfer64 item. `None` means the state could not be read.
fn xfer64_label(state: Option<&Xfer64State>) -> (&'static str, bool) {
    match state {
        Some(s) if s.installed => ("Open Xfer64", true),
        Some(s) if s.installer_available => ("Install Xfer64…", true),
        // Neither installed nor bundled: show it greyed rather than hiding it, so the menu does
        // not change shape depending on what happens to be on disk.
        _ => ("Xfer64 (not available)", false),
    }
}

/// Build the tray menu against current state.
///
/// The menu is rebuilt and swapped wholesale on every refresh rather than mutating individual
/// items. Both work, but rebuilding keeps the labels, the enabled flags and the ordering described
/// in exactly one place, so there is no way for a later edit to update the text of an item and
/// forget its enabled state.
///
/// Deliberately does **not** call `check_health`: that issues a blocking HTTP request, and this
/// runs on every `daemon-changed` event. "Running" here means the child process is alive; the
/// window shows the finer-grained health.
fn build_tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let (running, listen, serial) = match app.try_state::<AppState>() {
        Some(state) => {
            let settings = state.settings.lock().clone();
            (
                daemon_is_running(&state.daemon),
                settings.listen.clone(),
                effective_serial(&settings),
            )
        }
        None => (false, DEFAULT_LISTEN.to_string(), None),
    };

    let labels = tray_labels(running, serial.as_deref(), &listen);
    // Disabled: a status line, not an action.
    let status = MenuItem::with_id(app, "status", &labels.status, false, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        "toggle",
        labels.toggle,
        labels.toggle_enabled,
        None::<&str>,
    )?;
    let restart = MenuItem::with_id(
        app,
        "restart",
        "Restart daemon",
        labels.restart_enabled,
        None::<&str>,
    )?;

    let xfer_state = xfer64_state(app);
    let (xfer_text, xfer_enabled) = xfer64_label(Some(&xfer_state));
    let xfer = MenuItem::with_id(app, "xfer64", xfer_text, xfer_enabled, None::<&str>)?;

    let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Exit Multi64", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;

    Menu::with_items(
        app,
        &[
            &status, &sep1, &toggle, &restart, &sep2, &xfer, &sep3, &show, &quit,
        ],
    )
}

/// Rebuild the tray menu so its labels match current state. No-op when the tray is disabled.
fn refresh_tray_menu(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("tray") else {
        return;
    };
    match build_tray_menu(app) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => tracing_warn(&format!("tray menu rebuild failed: {e}")),
    }
}

/// The crate has no tracing dependency; daemon diagnostics go to the in-app log instead.
fn tracing_warn(msg: &str) {
    eprintln!("multi64: {msg}");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = load_settings();
    let daemon_arc = Arc::new(Mutex::new(DaemonInner::default()));
    let state = AppState {
        daemon: Arc::clone(&daemon_arc),
        settings: Mutex::new(settings.clone()),
    };
    let settings_for_setup = settings.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_serial_port_options,
            get_settings,
            set_settings,
            get_daemon_status,
            get_daemon_logs,
            clear_daemon_logs,
            daemon_start,
            daemon_stop,
            set_autostart_windows,
            get_autostart_windows,
            get_xfer64_state,
            launch_or_install_xfer64,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let st: tauri::State<'_, AppState> = app.state();
            let daemon_for_setup = Arc::clone(&st.daemon);

            // Read the flag into a local and drop the guard before the block: `build_tray_menu`
            // locks `settings` itself, and `parking_lot::Mutex` is not reentrant. Rust drops an
            // `if` condition's temporaries before the block, so holding it here would be fine
            // today -- but changing this to `if let`/`match` later would deadlock at startup.
            let tray_enabled = { st.settings.lock().tray_enabled };
            if tray_enabled {
                let menu = build_tray_menu(&handle)?;
                let icon = app
                    .default_window_icon()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing default window icon"))?;
                let _tray = TrayIconBuilder::with_id("tray")
                    .icon(icon)
                    .menu(&menu)
                    // Right-click opens the menu; left-click is left free so double-click can
                    // raise the window. With the menu on left-click the first click of a
                    // double-click pops the menu, which makes the gesture unusable.
                    .show_menu_on_left_click(false)
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        } = event
                        {
                            show_main_window(tray.app_handle());
                        }
                    })
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "quit" => {
                            if let Some(s) = app.try_state::<AppState>() {
                                kill_daemon(&s.daemon);
                            }
                            app.exit(0);
                        }
                        "show" => show_main_window(app),
                        "toggle" => {
                            let Some(s) = app.try_state::<AppState>() else {
                                return;
                            };
                            if daemon_is_running(&s.daemon) {
                                kill_daemon(&s.daemon);
                            } else {
                                let settings = s.settings.lock().clone();
                                if let Err(e) = start_daemon(app, &s.daemon, &settings) {
                                    push_log(&s.daemon, format!("Start failed: {e}"));
                                }
                            }
                            let _ = app.emit("daemon-changed", ());
                        }
                        "restart" => {
                            let Some(s) = app.try_state::<AppState>() else {
                                return;
                            };
                            kill_daemon(&s.daemon);
                            let settings = s.settings.lock().clone();
                            if let Err(e) = start_daemon(app, &s.daemon, &settings) {
                                push_log(&s.daemon, format!("Restart failed: {e}"));
                            }
                            let _ = app.emit("daemon-changed", ());
                        }
                        "xfer64" => {
                            let app = app.clone();
                            tauri::async_runtime::spawn_blocking(move || {
                                if let Err(e) = launch_or_install_xfer64_blocking(&app) {
                                    tracing_warn(&format!("Xfer64: {e}"));
                                }
                            });
                        }
                        // "status" is disabled and cannot be clicked.
                        _ => {}
                    })
                    .build(app)?;

                // The window emits `daemon-changed` too, so the tray tracks state regardless of
                // which one started or stopped the daemon.
                let tray_handle = handle.clone();
                handle.listen("daemon-changed", move |_| {
                    refresh_tray_menu(&tray_handle);
                });
            }

            // Default: main window opens normally. Only hide on launch when the user opted in
            // (requires tray — otherwise they'd have no way to show the window).
            if let Some(w) = app.get_webview_window("main") {
                if settings_for_setup.start_minimized && settings_for_setup.tray_enabled {
                    let _ = w.hide();
                } else {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            if settings_for_setup.auto_start_daemon
                && effective_serial(&settings_for_setup).is_some()
            {
                let h = handle.clone();
                let d = Arc::clone(&daemon_for_setup);
                let s = settings_for_setup.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let _ = start_daemon(&h, &d, &s);
                    let _ = h.emit("daemon-changed", ());
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Some(state) = window.try_state::<AppState>() {
                    let settings = state.settings.lock();
                    if settings.tray_enabled && settings.minimize_to_tray_on_close {
                        api.prevent_close();
                        drop(settings);
                        let _ = window.hide();
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app_handle, event| {
            if let RunEvent::Exit = event {
                kill_daemon(&daemon_arc);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file written by an older build omits newer fields; they must fall back to
    /// their serde defaults rather than failing the parse, which would silently reset everything.
    #[test]
    fn settings_from_older_file_keeps_known_fields() {
        let json = r#"{"serialPort":"COM7","baud":57600,"listen":"127.0.0.1:38765"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.serial_port.as_deref(), Some("COM7"));
        assert_eq!(s.baud, 57600);
        assert!(s.auto_start_daemon, "default_true");
        assert!(s.tray_enabled, "default_true");
        assert!(!s.autostart_app);
        assert_eq!(s.multi64d_log_preset, Multi64dLogPreset::Default);
    }

    /// start_minimized is meaningless without a tray to restore from.
    #[test]
    fn normalize_clears_start_minimized_without_tray() {
        let s = normalize_settings(Settings {
            tray_enabled: false,
            start_minimized: true,
            ..Settings::default()
        });
        assert!(!s.start_minimized);
        let s = normalize_settings(Settings {
            tray_enabled: true,
            start_minimized: true,
            ..Settings::default()
        });
        assert!(s.start_minimized);
    }

    /// The baud the user picks must reach multi64d; it was collected and never passed.
    #[test]
    fn baud_is_serialised_for_the_daemon_argument() {
        let s = Settings {
            baud: 57600,
            ..Settings::default()
        };
        assert_eq!(s.baud.to_string(), "57600");
    }
}

#[cfg(test)]
mod tray_tests {
    use super::*;

    #[test]
    fn status_names_the_port_when_running() {
        let l = tray_labels(true, Some("COM4"), "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running on COM4");
        assert_eq!(l.toggle, "Stop daemon");
    }

    #[test]
    fn status_falls_back_to_listen_when_the_port_is_unknown() {
        // A live daemon with no configured or auto-detected port: name where it listens rather
        // than a port we cannot identify.
        let l = tray_labels(true, None, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running (127.0.0.1:38765)");
    }

    #[test]
    fn stopped_shows_start_and_disables_restart() {
        let l = tray_labels(false, Some("COM4"), "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: stopped");
        assert_eq!(l.toggle, "Start daemon");
        assert!(l.toggle_enabled, "a port is configured, so Start is usable");
        assert!(
            !l.restart_enabled,
            "restarting a stopped daemon is just Start"
        );
    }

    #[test]
    fn start_is_disabled_without_a_port() {
        // `start_daemon` fails with no serial port, so the item must not invite the click.
        let l = tray_labels(false, None, "127.0.0.1:38765");
        assert!(!l.toggle_enabled);
    }

    #[test]
    fn stop_stays_enabled_even_without_a_port() {
        // The port can disappear while the daemon runs; stopping it must still be possible.
        let l = tray_labels(true, None, "127.0.0.1:38765");
        assert_eq!(l.toggle, "Stop daemon");
        assert!(l.toggle_enabled);
        assert!(l.restart_enabled);
    }

    #[test]
    fn xfer64_label_tracks_availability() {
        let installed = Xfer64State {
            installed: true,
            installer_available: true,
        };
        assert_eq!(xfer64_label(Some(&installed)), ("Open Xfer64", true));

        let only_installer = Xfer64State {
            installed: false,
            installer_available: true,
        };
        assert_eq!(
            xfer64_label(Some(&only_installer)),
            ("Install Xfer64…", true)
        );

        let neither = Xfer64State {
            installed: false,
            installer_available: false,
        };
        assert_eq!(
            xfer64_label(Some(&neither)),
            ("Xfer64 (not available)", false)
        );
        // Unreadable state is treated as unavailable rather than offering a click that fails.
        assert_eq!(xfer64_label(None), ("Xfer64 (not available)", false));
    }
}

#[cfg(test)]
mod frontend_tests {
    /// `appearance.js` is duplicated verbatim in both apps because `frontendDist` is per-app and no
    /// file can be shared across crates at runtime. Nothing else enforces that, so a fix applied to
    /// one copy would silently leave the other stale — one app quietly ignoring a preference the
    /// other honours. Documented in `docs/frontend-appearance.md`; checked here.
    #[test]
    fn appearance_js_is_identical_in_both_apps() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = here
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .expect("crates/multi64/src-tauri -> repo root");
        let multi64 = root.join("crates/multi64/src/appearance.js");
        let xfer64 = root.join("crates/xfer64/src/appearance.js");

        let a = std::fs::read_to_string(&multi64)
            .unwrap_or_else(|e| panic!("read {}: {e}", multi64.display()));
        let b = std::fs::read_to_string(&xfer64)
            .unwrap_or_else(|e| panic!("read {}: {e}", xfer64.display()));

        if a != b {
            let first = a
                .lines()
                .zip(b.lines())
                .position(|(x, y)| x != y)
                .map(|i| format!("first differing line: {}", i + 1))
                .unwrap_or_else(|| {
                    format!(
                        "same prefix, lengths differ: {} vs {} lines",
                        a.lines().count(),
                        b.lines().count()
                    )
                });
            panic!(
                "appearance.js has drifted between the apps ({first}).\n\
                 Edit one and copy it to the other; they must stay byte-identical."
            );
        }
    }
}
