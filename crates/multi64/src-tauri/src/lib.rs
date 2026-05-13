//! Multi64 — manages `multi64d`, tray, settings (Windows-first).

use auto_launch::AutoLaunch;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, RunEvent};

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
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .map(normalize_settings)
        .unwrap_or_else(|| normalize_settings(Settings::default()))
}

fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = settings_path()?;
    let s = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())
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

fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

fn auto_pick_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(_) = &p.port_type {
            return Some(p.port_name.clone());
        }
    }
    ports.first().map(|p| p.port_name.clone())
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

fn check_health(listen: &str) -> bool {
    let url = daemon_health_url(listen);
    ureq::get(&url)
        .timeout(std::time::Duration::from_secs(1))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

fn kill_daemon(daemon: &Arc<Mutex<DaemonInner>>) {
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
    kill_daemon(daemon);
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
    cmd.env("NO_COLOR", "1")
        .args([
            "--serial",
            &serial,
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

#[tauri::command]
fn get_serial_ports() -> Vec<String> {
    list_ports()
}

#[tauri::command]
fn get_auto_serial() -> Option<String> {
    auto_pick_port()
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
fn set_settings(state: tauri::State<'_, AppState>, settings: Settings) -> Result<(), String> {
    let settings = normalize_settings(settings);
    let prev_autostart = { state.settings.lock().autostart_app };
    save_settings(&settings)?;
    *state.settings.lock() = settings.clone();
    if settings.autostart_app != prev_autostart {
        set_autostart_windows_impl(settings.autostart_app)?;
    }
    Ok(())
}

fn set_autostart_windows_impl(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let auto = AutoLaunch::new("Multi64", &exe.to_string_lossy(), &[] as &[&str]);
    if enabled {
        auto.enable().map_err(|e| e.to_string())?;
    } else {
        auto.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_daemon_status(state: tauri::State<'_, AppState>) -> DaemonStatus {
    let settings = state.settings.lock().clone();
    let listen = settings.listen.clone();
    let running = state.daemon.lock().child.is_some();
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

#[tauri::command]
fn daemon_start(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let settings = state.settings.lock().clone();
    match start_daemon(&app, &state.daemon, &settings) {
        Ok(()) => Ok(()),
        Err(e) => {
            push_log(&state.daemon, format!("Start failed: {e}"));
            let _ = app.emit("daemon-changed", ());
            Err(e)
        }
    }
}

#[tauri::command]
fn daemon_stop(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    kill_daemon(&state.daemon);
    let _ = app.emit("daemon-changed", ());
    Ok(())
}

#[tauri::command]
fn set_autostart_windows(enabled: bool) -> Result<(), String> {
    set_autostart_windows_impl(enabled)
}

#[tauri::command]
fn get_autostart_windows() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        let auto = AutoLaunch::new("Multi64", &exe.to_string_lossy(), &[] as &[&str]);
        return auto.is_enabled().unwrap_or(false);
    }
    false
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
        "cart-explorer-setup.exe",
        "resources/cart-explorer-setup.exe",
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

#[tauri::command]
fn get_xfer64_state(app: tauri::AppHandle) -> Result<Xfer64State, String> {
    let installed = is_xfer64_installed();
    let installer_available = resolve_xfer64_installer_path(&app).is_some();
    Ok(Xfer64State {
        installed,
        installer_available,
    })
}

/// If Xfer64 is installed, launch it. Otherwise run the bundled installer (`xfer64-setup.exe` or `xfer64-setup.msi`).
#[tauri::command]
fn launch_or_install_xfer64(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(exe) = first_xfer64_exe() {
        return Command::new(&exe)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ());
    }
    let Some(installer_path) = resolve_xfer64_installer_path(&app) else {
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
            get_serial_ports,
            get_auto_serial,
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

            if st.settings.lock().tray_enabled {
                let quit = MenuItem::with_id(app, "quit", "Exit Multi64", true, None::<&str>)?;
                let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &quit])?;
                let icon = app
                    .default_window_icon()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing default window icon"))?;
                let _tray = TrayIconBuilder::with_id("tray")
                    .icon(icon)
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "quit" => {
                            if let Some(s) = app.try_state::<AppState>() {
                                kill_daemon(&s.daemon);
                            }
                            app.exit(0);
                        }
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        _ => {}
                    })
                    .build(app)?;
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
