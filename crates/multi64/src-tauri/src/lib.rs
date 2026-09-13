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
    /// Which cart `multi64d` talks to (`--cart`). Files written before this existed mean SC64.
    #[serde(default)]
    pub cart: DaemonCart,
}

fn default_true() -> bool {
    true
}

/// The cart `multi64d` is started for, passed as `--cart`.
///
/// The serialised names are the daemon's own `--cart` values, so the settings file and the
/// argument share one vocabulary (checked against `multi64d::CartKind` in the tests).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DaemonCart {
    /// SummerCart64: the only backend proven on hardware.
    #[default]
    Sc64,
    /// EverDrive-64 X7: experimental, never run against a cart (`l3-over-everdrive-x7.md` §4.5).
    Ed64,
}

impl DaemonCart {
    /// The `--cart` value.
    fn arg(self) -> &'static str {
        match self {
            DaemonCart::Sc64 => "sc64",
            DaemonCart::Ed64 => "ed64",
        }
    }

    /// How the window, tray and log name the cart. Anything unproven says so every time.
    fn label(self) -> &'static str {
        match self {
            DaemonCart::Sc64 => "SummerCart64",
            DaemonCart::Ed64 => "EverDrive-64 X7 (experimental)",
        }
    }
}

/// How verbose `multi64d` stderr logging should be (see `multi64d` `--serial-trace` and `RUST_LOG`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Multi64dLogPreset {
    /// `RUST_LOG` unset → tracing default `info` (same as upstream).
    #[default]
    Default,
    /// Both L2 pipes (`multi64_sc64_l2`, `multi64_ed64_l2`) + `multi64d` at debug, without
    /// per-read serial trace.
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
                "multi64_sc64_l2=debug,multi64_ed64_l2=debug,multi64d=debug,tower_http=warn,info",
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
            cart: DaemonCart::Sc64,
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
    /// While running, the cart the live process was started for; while stopped, the next start's.
    pub cart: &'static str,
}

#[derive(Default)]
struct DaemonInner {
    child: Option<Child>,
    /// The port `child` was spawned with; set and cleared together with it.
    ///
    /// Settings only say what the *next* start would use. Re-resolving them can name a different
    /// port than the live process holds: Auto re-enumerates, and a cart plugged in or pulled
    /// mid-session changes the answer.
    serial: Option<String>,
    /// The cart `child` was spawned for. Only meaningful while `serial` is set, for the same
    /// reason: a cart changed in Settings is what the next start uses, not what is running.
    cart: DaemonCart,
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

/// FTDI FT232H, the USB bridge on a SummerCart64.
///
/// A stock FTDI part that also turns up in unrelated adapters, so the VID/PID is only half of the
/// match; see [`usb_is_sc64`].
const SC64_USB_VID: u16 = 0x0403;
const SC64_USB_PID: u16 = 0x6014;
/// The cart programs its FTDI serial number as `SC64…` and its product string as `SC64`.
const SC64_USB_TAG: &str = "SC64";

/// Whether a USB serial device is a SummerCart64, judged from its descriptors alone.
///
/// Deliberately writes nothing: until proven otherwise the port belongs to some other device,
/// opening it can reset that device (many adapters toggle DTR on open), and the cart's own port
/// may already be held by `multi64d`.
///
/// Windows reports the FTDI serial with the interface letter appended (`SC64XXXXXXA`) and the
/// driver's description (`USB Serial Port`) as the product, so there the serial prefix is what
/// matches; other platforms read the product string from the descriptor.
fn usb_is_sc64(usb: &serialport::UsbPortInfo) -> bool {
    let tagged = |s: &Option<String>| {
        s.as_deref()
            .and_then(|s| s.get(..SC64_USB_TAG.len()))
            .is_some_and(|head| head.eq_ignore_ascii_case(SC64_USB_TAG))
    };
    usb.vid == SC64_USB_VID
        && usb.pid == SC64_USB_PID
        && (tagged(&usb.serial_number) || tagged(&usb.product))
}

/// What auto-selection found among the enumerated ports.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AutoPort {
    /// Exactly one cart.
    Found(String),
    /// No port identifies as a cart. Other serial devices may be present; they are not candidates.
    NoCart,
    /// More than one cart, so any choice would be a guess.
    Ambiguous(Vec<String>),
}

impl AutoPort {
    fn port(self) -> Option<String> {
        match self {
            AutoPort::Found(port) => Some(port),
            AutoPort::NoCart | AutoPort::Ambiguous(_) => None,
        }
    }

    /// Why there is no port, worded for the settings hint, the status line and the daemon log.
    fn problem(&self) -> Option<String> {
        match self {
            AutoPort::Found(_) => None,
            AutoPort::NoCart => Some(
                "No SummerCart64 found on USB. Plug in the cart or pick its COM port in \
                 Settings; other serial devices are never chosen automatically."
                    .to_string(),
            ),
            AutoPort::Ambiguous(ports) => Some(format!(
                "More than one SummerCart64 found ({}). Pick one in Settings.",
                ports.join(", ")
            )),
        }
    }
}

/// The cart's port, or nothing.
///
/// This used to return the first USB serial device of any kind, else the first port at all.
/// Enumeration order is not stable, so on a machine with a second USB serial adapter the daemon
/// opened whichever came first — and then reported itself healthy while holding an unrelated
/// device, because opening a port proves nothing about what is on the other end. A port is now
/// chosen only when it identifies as a cart, and only when that choice is unique.
fn pick_auto(ports: &[serialport::SerialPortInfo]) -> AutoPort {
    let mut carts: Vec<String> = ports
        .iter()
        .filter(|p| {
            matches!(&p.port_type, serialport::SerialPortType::UsbPort(usb) if usb_is_sc64(usb))
        })
        .map(|p| p.port_name.clone())
        .collect();
    match carts.len() {
        0 => AutoPort::NoCart,
        1 => AutoPort::Found(carts.remove(0)),
        _ => {
            carts.sort();
            AutoPort::Ambiguous(carts)
        }
    }
}

fn auto_port() -> AutoPort {
    // A failed enumeration found no cart; reporting that beats failing the caller.
    serialport::available_ports()
        .map(|ports| pick_auto(&ports))
        .unwrap_or(AutoPort::NoCart)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortOptions {
    pub ports: Vec<String>,
    pub auto: Option<String>,
    /// Set whenever `auto` is `None`: why Auto has no port.
    pub auto_warning: Option<String>,
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
        auto_warning: auto.problem(),
        auto: auto.port(),
    }
}

/// Why Auto has no port for an EverDrive, worded like [`AutoPort::problem`].
const EVERDRIVE_NEEDS_PORT: &str =
    "Auto only recognises a SummerCart64. Pick the EverDrive's COM port in Settings.";

/// The saved port if there is one, else the auto pick. `Err` says why there is no port.
///
/// `auto` runs only without a saved port, so a pinned port never costs an enumeration.
///
/// Auto never picks a port for an EverDrive. It judges from USB descriptors alone, and the X7's
/// FT245R (`0403:6001`) is a stock FTDI part with nothing cart-specific to match; finding it by
/// writing to ports would disturb whatever else is plugged in. Handing it an SC64's port instead
/// would run the EverDrive framing against the wrong cart.
fn resolve_serial(
    saved: Option<&str>,
    cart: DaemonCart,
    auto: impl FnOnce() -> AutoPort,
) -> Result<String, String> {
    if let Some(port) = saved.filter(|s| !s.is_empty()) {
        return Ok(port.to_string());
    }
    if cart != DaemonCart::Sc64 {
        return Err(EVERDRIVE_NEEDS_PORT.to_string());
    }
    let auto = auto();
    match auto.problem() {
        Some(problem) => Err(problem),
        None => auto.port().ok_or_else(|| "no serial port".to_string()),
    }
}

fn serial_for(settings: &Settings) -> Result<String, String> {
    resolve_serial(settings.serial_port.as_deref(), settings.cart, auto_port)
}

fn effective_serial(settings: &Settings) -> Option<String> {
    serial_for(settings).ok()
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
            inner.serial = None;
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
    inner.serial = None;
    if let Some(mut c) = inner.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Spawn-time arguments for `multi64d`. Pure, so what reaches the daemon is testable without
/// spawning it.
fn daemon_args(serial: &str, settings: &Settings) -> Vec<String> {
    vec![
        "--serial".into(),
        serial.into(),
        // baud is a spawn-time argument like the rest: multi64d takes --baud and feeds it to the
        // L2 pipe. It used to be collected and saved by the UI but never passed, so the setting
        // did nothing and the daemon always ran at its own 115200 default.
        "--baud".into(),
        settings.baud.to_string(),
        "--cart".into(),
        settings.cart.arg().into(),
        "--listen".into(),
        settings.listen.clone(),
        "--no-print-ports".into(),
    ]
}

fn start_daemon(
    app: &tauri::AppHandle,
    daemon: &Arc<Mutex<DaemonInner>>,
    settings: &Settings,
) -> Result<(), String> {
    let _ops = DAEMON_OPS.lock();
    kill_daemon_locked(daemon);
    let serial = serial_for(settings)?;
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
            "Starting multi64d on {serial} for {} ({}) [logging: {log_label}]",
            settings.cart.label(),
            settings.listen
        ),
    );
    if settings.cart == DaemonCart::Ed64 {
        push_log(
            daemon,
            "EverDrive-64 X7 support is experimental and has never been run against a cart: \
             a running daemon does not show that the cart link works."
                .to_string(),
        );
    }

    let daemon_path = resolve_multi64d_path(app)?;
    let mut cmd = Command::new(&daemon_path);
    cmd.env("NO_COLOR", "1")
        .args(daemon_args(&serial, settings))
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
    inner.serial = Some(serial);
    inner.cart = settings.cart;
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
    // Serial port, baud, cart, listen address and log preset are command-line arguments fixed at
    // spawn, so a running daemon keeps using the old ones. Saving used to appear to apply them
    // while the bridge quietly stayed on the previous port.
    let spawn_args_changed = settings.serial_port != prev.serial_port
        || settings.baud != prev.baud
        || settings.cart != prev.cart
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
    // Read after `daemon_is_running` returns: it takes the daemon lock itself.
    let cart = if running {
        state.daemon.lock().cart
    } else {
        settings.cart
    };
    let healthy = running && check_health(&listen);
    let message = if !running {
        // Name why Start cannot work. Without a saved port this enumerates on every poll, which
        // is cheap and only happens while stopped.
        match serial_for(&settings) {
            Ok(_) => "Stopped".into(),
            Err(why) => format!("Stopped — {why}"),
        }
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
        cart: cart.label(),
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

fn tray_labels(running: bool, serial: Option<&str>, cart: DaemonCart, listen: &str) -> TrayLabels {
    // SC64 is the default and needs no mention; any other cart is named, so an experimental
    // daemon is never mistaken for the proven one.
    let cart_note = match cart {
        DaemonCart::Sc64 => String::new(),
        other => format!(" · {}", other.label()),
    };
    let status = if running {
        match serial {
            Some(port) => format!("Daemon: running on {port}"),
            // No configured or auto-detected port, but a live process: report where it
            // listens rather than claiming a port we cannot name.
            None => format!("Daemon: running ({listen})"),
        }
    } else if serial.is_some() {
        "Daemon: stopped".to_string()
    } else {
        // Start is greyed out below; say why, since the tray has no room for the full reason.
        "Daemon: stopped (no cart port)".to_string()
    };
    TrayLabels {
        status: format!("{status}{cart_note}"),
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

/// The port the tray names: while running, the one the live process was spawned with; while
/// stopped, the one Start would use.
///
/// Running never re-resolves, so the label cannot drift from the process and a running daemon
/// costs no port enumeration per menu rebuild.
fn tray_serial(
    running: bool,
    spawned: Option<String>,
    resolve: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if running {
        spawned
    } else {
        resolve()
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
    let (running, listen, serial, cart) = match app.try_state::<AppState>() {
        Some(state) => {
            let settings = state.settings.lock().clone();
            // `daemon_is_running` takes the daemon lock itself, so read the port after it returns.
            let running = daemon_is_running(&state.daemon);
            let (spawned, spawned_cart) = {
                let inner = state.daemon.lock();
                (inner.serial.clone(), inner.cart)
            };
            // Same rule as the port: a running daemon is named for what it was started with.
            let cart = if running { spawned_cart } else { settings.cart };
            (
                running,
                settings.listen.clone(),
                tray_serial(running, spawned, || effective_serial(&settings)),
                cart,
            )
        }
        None => (false, DEFAULT_LISTEN.to_string(), None, DaemonCart::Sc64),
    };

    let labels = tray_labels(running, serial.as_deref(), cart, &listen);
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

            if settings_for_setup.auto_start_daemon {
                match serial_for(&settings_for_setup) {
                    Ok(_) => {
                        let h = handle.clone();
                        let d = Arc::clone(&daemon_for_setup);
                        let s = settings_for_setup.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            if let Err(e) = start_daemon(&h, &d, &s) {
                                push_log(&d, format!("Start failed: {e}"));
                            }
                            let _ = h.emit("daemon-changed", ());
                        });
                    }
                    // Skipping the auto-start used to leave no trace; the log now says why.
                    Err(why) => push_log(&daemon_for_setup, format!("multi64d not started: {why}")),
                }
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
        assert_eq!(
            s.cart,
            DaemonCart::Sc64,
            "files predating the setting meant SC64"
        );
    }

    #[test]
    fn a_saved_everdrive_cart_is_read_back() {
        let json =
            r#"{"serialPort":"COM6","baud":115200,"listen":"127.0.0.1:38765","cart":"ed64"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.cart, DaemonCart::Ed64);
    }

    /// The settings file and `--cart` must use multi64d's own names, or the daemon refuses to
    /// start (clap rejects an unknown value) or reads a config it does not understand.
    #[test]
    fn cart_names_match_multi64d() {
        for cart in [DaemonCart::Sc64, DaemonCart::Ed64] {
            let json = serde_json::to_string(&cart).unwrap();
            let daemon: multi64d::CartKind = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("multi64d does not know {json}: {e}"));
            assert_eq!(daemon.as_str(), cart.arg());
        }
    }

    #[test]
    fn daemon_args_carry_the_cart() {
        let flag = |args: &[String], name: &str| {
            let at = args.iter().position(|a| a == name).expect(name);
            args[at + 1].clone()
        };
        let sc64 = daemon_args("COM4", &Settings::default());
        assert_eq!(flag(&sc64, "--cart"), "sc64");
        assert_eq!(flag(&sc64, "--serial"), "COM4");
        let ed64 = daemon_args(
            "COM6",
            &Settings {
                cart: DaemonCart::Ed64,
                baud: 57600,
                ..Settings::default()
            },
        );
        assert_eq!(flag(&ed64, "--cart"), "ed64");
        assert_eq!(flag(&ed64, "--baud"), "57600");
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
mod port_selection_tests {
    use super::*;
    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

    fn usb(
        name: &str,
        vid: u16,
        pid: u16,
        serial: Option<&str>,
        product: Option<&str>,
    ) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: serial.map(str::to_string),
                manufacturer: None,
                product: product.map(str::to_string),
            }),
        }
    }

    /// A cart as Windows reports it: `FTDIBUS\VID_0403+PID_6014+SC64XXXXXXA\0000`, with the
    /// FTDI interface letter on the serial and the driver's description as the product.
    fn sc64_windows(name: &str) -> SerialPortInfo {
        usb(
            name,
            0x0403,
            0x6014,
            Some("SC64XXXXXXA"),
            Some("USB Serial Port"),
        )
    }

    /// A CH340 adapter as Windows reports it: `USB\VID_1A86&PID_7523\...`.
    fn ch340(name: &str) -> SerialPortInfo {
        usb(name, 0x1a86, 0x7523, None, Some("USB-SERIAL CH340"))
    }

    /// The reported failure: a CH340 enumerated ahead of the cart was picked, and the daemon
    /// held it while claiming to be healthy.
    #[test]
    fn picks_the_cart_over_an_adapter_enumerated_first() {
        let ports = [ch340("COM5"), sc64_windows("COM4")];
        assert_eq!(pick_auto(&ports), AutoPort::Found("COM4".into()));
    }

    #[test]
    fn the_pick_does_not_depend_on_enumeration_order() {
        let ports = [sc64_windows("COM4"), ch340("COM5")];
        assert_eq!(pick_auto(&ports), AutoPort::Found("COM4".into()));
    }

    /// The old fallback took the only USB serial device, cart or not.
    #[test]
    fn a_lone_non_cart_adapter_is_not_chosen() {
        let got = pick_auto(&[ch340("COM5")]);
        assert_eq!(got, AutoPort::NoCart);
        assert!(got.problem().is_some(), "the UI needs a reason to show");
    }

    /// The old last resort took the first port of any kind.
    #[test]
    fn non_usb_ports_are_not_chosen() {
        let ports = [
            SerialPortInfo {
                port_name: "COM1".into(),
                port_type: SerialPortType::PciPort,
            },
            SerialPortInfo {
                port_name: "COM3".into(),
                port_type: SerialPortType::BluetoothPort,
            },
            SerialPortInfo {
                port_name: "COM9".into(),
                port_type: SerialPortType::Unknown,
            },
        ];
        assert_eq!(pick_auto(&ports), AutoPort::NoCart);
        assert_eq!(pick_auto(&[]), AutoPort::NoCart);
    }

    /// The cart's VID/PID is a stock FTDI part; without the SC64 tag it is just some adapter.
    #[test]
    fn a_stock_ftdi_part_without_the_tag_is_not_a_cart() {
        let ports = [usb(
            "COM6",
            0x0403,
            0x6014,
            Some("FT9ABCDEA"),
            Some("USB Serial Port"),
        )];
        assert_eq!(pick_auto(&ports), AutoPort::NoCart);
    }

    #[test]
    fn the_tag_without_the_ftdi_ids_is_not_a_cart() {
        let wrong_pid = usb("COM6", 0x0403, 0x6001, Some("SC64XXXXXXA"), None);
        let wrong_vid = usb("COM7", 0x1a86, 0x6014, None, Some("SC64"));
        assert_eq!(pick_auto(&[wrong_pid, wrong_vid]), AutoPort::NoCart);
    }

    /// Linux and macOS read the descriptor's product string; the serial may not come through.
    #[test]
    fn matches_on_the_product_string_too() {
        let ports = [usb("/dev/ttyUSB0", 0x0403, 0x6014, None, Some("SC64"))];
        assert_eq!(pick_auto(&ports), AutoPort::Found("/dev/ttyUSB0".into()));
    }

    #[test]
    fn the_tag_match_ignores_case_and_survives_short_or_non_ascii_strings() {
        assert!(usb_is_sc64(&UsbPortInfo {
            vid: 0x0403,
            pid: 0x6014,
            serial_number: Some("sc64xxxxxxa".into()),
            manufacturer: None,
            product: None,
        }));
        for odd in ["", "SC", "SCβ4", "βSC64"] {
            assert!(
                !usb_is_sc64(&UsbPortInfo {
                    vid: 0x0403,
                    pid: 0x6014,
                    serial_number: Some(odd.into()),
                    manufacturer: None,
                    product: Some(odd.into()),
                }),
                "{odd:?} must not match (or panic on a char boundary)"
            );
        }
    }

    #[test]
    fn two_carts_is_ambiguous_rather_than_a_guess() {
        let ports = [sc64_windows("COM8"), ch340("COM5"), sc64_windows("COM4")];
        let got = pick_auto(&ports);
        assert_eq!(got, AutoPort::Ambiguous(vec!["COM4".into(), "COM8".into()]));
        let problem = got.problem().unwrap();
        assert!(problem.contains("COM4, COM8"), "{problem}");
    }

    #[test]
    fn a_saved_port_wins_without_enumerating() {
        let got = resolve_serial(Some("COM5"), DaemonCart::Sc64, || {
            panic!("auto must not run with a saved port")
        });
        assert_eq!(got, Ok("COM5".into()));
    }

    #[test]
    fn an_empty_saved_port_means_auto() {
        let got = resolve_serial(Some(""), DaemonCart::Sc64, || {
            AutoPort::Found("COM4".into())
        });
        assert_eq!(got, Ok("COM4".into()));
        let got = resolve_serial(None, DaemonCart::Sc64, || AutoPort::Found("COM4".into()));
        assert_eq!(got, Ok("COM4".into()));
    }

    /// Auto cannot identify an EverDrive, and must not hand it an SC64's port either.
    #[test]
    fn an_everdrive_on_auto_is_an_error_without_enumerating() {
        let err = resolve_serial(None, DaemonCart::Ed64, || {
            panic!("an EverDrive must not fall back to the SC64 auto pick")
        })
        .unwrap_err();
        assert!(err.contains("EverDrive"), "{err}");
        let got = resolve_serial(Some("COM6"), DaemonCart::Ed64, || {
            panic!("auto must not run with a saved port")
        });
        assert_eq!(got, Ok("COM6".into()));
    }

    /// No cart on Auto is an error carrying the reason, not some other port.
    #[test]
    fn no_cart_on_auto_is_an_error_with_the_reason() {
        let err = resolve_serial(None, DaemonCart::Sc64, || AutoPort::NoCart).unwrap_err();
        assert_eq!(Some(err), AutoPort::NoCart.problem());
        let err = resolve_serial(None, DaemonCart::Sc64, || {
            AutoPort::Ambiguous(vec!["COM4".into(), "COM8".into()])
        })
        .unwrap_err();
        assert!(err.contains("More than one"), "{err}");
    }

    /// The frontend reads `autoWarning`; a rename on either side would silently drop the warning.
    #[test]
    fn options_serialise_the_warning_for_the_frontend() {
        let json = serde_json::to_value(SerialPortOptions {
            ports: vec!["COM5".into()],
            auto: None,
            auto_warning: AutoPort::NoCart.problem(),
        })
        .unwrap();
        assert!(json["auto"].is_null());
        assert!(json["autoWarning"].is_string());
    }
}

#[cfg(test)]
mod tray_tests {
    use super::*;

    #[test]
    fn status_names_the_port_when_running() {
        let l = tray_labels(true, Some("COM4"), DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running on COM4");
        assert_eq!(l.toggle, "Stop daemon");
    }

    #[test]
    fn status_falls_back_to_listen_when_the_port_is_unknown() {
        // A live daemon with no configured or auto-detected port: name where it listens rather
        // than a port we cannot identify.
        let l = tray_labels(true, None, DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running (127.0.0.1:38765)");
    }

    /// An experimental daemon is named in the tray, running or not; SC64 stays unadorned.
    #[test]
    fn status_names_an_everdrive_cart() {
        let l = tray_labels(true, Some("COM6"), DaemonCart::Ed64, "127.0.0.1:38765");
        assert_eq!(
            l.status,
            "Daemon: running on COM6 · EverDrive-64 X7 (experimental)"
        );
        let l = tray_labels(false, None, DaemonCart::Ed64, "127.0.0.1:38765");
        assert_eq!(
            l.status,
            "Daemon: stopped (no cart port) · EverDrive-64 X7 (experimental)"
        );
        let l = tray_labels(true, Some("COM4"), DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running on COM4");
    }

    #[test]
    fn stopped_shows_start_and_disables_restart() {
        let l = tray_labels(false, Some("COM4"), DaemonCart::Sc64, "127.0.0.1:38765");
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
        let l = tray_labels(false, None, DaemonCart::Sc64, "127.0.0.1:38765");
        assert!(!l.toggle_enabled);
        // ...and the status line says why it is greyed out.
        assert_eq!(l.status, "Daemon: stopped (no cart port)");
    }

    #[test]
    fn stop_stays_enabled_even_without_a_port() {
        // The port can disappear while the daemon runs; stopping it must still be possible.
        let l = tray_labels(true, None, DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.toggle, "Stop daemon");
        assert!(l.toggle_enabled);
        assert!(l.restart_enabled);
    }

    /// The reported drift: the daemon was spawned on one port, Auto would now resolve another,
    /// and the label must keep naming the port the process actually holds.
    #[test]
    fn running_names_the_spawned_port_not_the_current_resolution() {
        let serial = tray_serial(true, Some("COM4".into()), || {
            panic!("a running daemon must not re-resolve its port")
        });
        assert_eq!(serial.as_deref(), Some("COM4"));
        let l = tray_labels(true, serial.as_deref(), DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running on COM4");
    }

    #[test]
    fn stopped_names_what_start_would_use() {
        assert_eq!(
            tray_serial(false, None, || Some("COM4".into())).as_deref(),
            Some("COM4")
        );
        // A stale spawned port from a dead process is not what Start would use.
        assert_eq!(tray_serial(false, Some("COM5".into()), || None), None);
    }

    #[test]
    fn running_without_a_recorded_port_falls_back_to_listen() {
        let serial = tray_serial(true, None, || Some("COM4".into()));
        let l = tray_labels(true, serial.as_deref(), DaemonCart::Sc64, "127.0.0.1:38765");
        assert_eq!(l.status, "Daemon: running (127.0.0.1:38765)");
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
