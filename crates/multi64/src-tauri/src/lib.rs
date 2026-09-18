//! Multi64 — manages `multi64d`, tray, settings (Windows-first).

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use multi64_cart_probe::{usb_is_sc64, DetectedCart};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
#[cfg(feature = "xfer64")]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Which cart `multi64d` is started for: a fixed one, or Auto-detect at each start. Files
    /// written before this existed mean Auto-detect.
    #[serde(default)]
    pub cart: CartSetting,
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
    /// EverDrive-64 X7: experimental, run on one cart so far (`l3-over-everdrive-x7.md` §4.5).
    Ed64,
    /// EverDrive-64 PRO: experimental, never run against a cart (`l3-over-everdrive-pro.md` §8).
    Ed64Pro,
}

impl DaemonCart {
    /// The `--cart` value.
    fn arg(self) -> &'static str {
        match self {
            DaemonCart::Sc64 => "sc64",
            DaemonCart::Ed64 => "ed64",
            DaemonCart::Ed64Pro => "ed64pro",
        }
    }

    /// How the window, tray and log name the cart. Anything unproven says so every time.
    fn label(self) -> &'static str {
        match self {
            DaemonCart::Sc64 => "SummerCart64",
            DaemonCart::Ed64 => "EverDrive-64 X7 (beta)",
            DaemonCart::Ed64Pro => "EverDrive-64 PRO (beta)",
        }
    }
}

impl From<DetectedCart> for DaemonCart {
    fn from(found: DetectedCart) -> Self {
        match found {
            DetectedCart::Sc64 => DaemonCart::Sc64,
            DetectedCart::Ed64 => DaemonCart::Ed64,
            DetectedCart::Ed64Pro => DaemonCart::Ed64Pro,
        }
    }
}

/// The Settings → **Cart** choice: a fixed cart, or Auto-detect.
///
/// The fixed names are the daemon's own `--cart` values (see [`DaemonCart`]). `auto` never reaches
/// the daemon, which is always started for the cart detection found.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CartSetting {
    /// Find the cart at each start: a SummerCart64 by its USB descriptors, which sends nothing, else
    /// by probing ports with `multi64-cart-probe`. The default.
    #[default]
    Auto,
    Sc64,
    Ed64,
    Ed64Pro,
}

impl CartSetting {
    /// The cart this setting fixes, or `None` for Auto-detect.
    fn fixed(self) -> Option<DaemonCart> {
        match self {
            CartSetting::Auto => None,
            CartSetting::Sc64 => Some(DaemonCart::Sc64),
            CartSetting::Ed64 => Some(DaemonCart::Ed64),
            CartSetting::Ed64Pro => Some(DaemonCart::Ed64Pro),
        }
    }

    fn label(self) -> &'static str {
        match self.fixed() {
            Some(cart) => cart.label(),
            None => "Auto-detect",
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
    /// Every cart's L2 pipe (`multi64_sc64_l2`, `multi64_ed64_l2`, `multi64_ed64pro_l2`) + `multi64d`
    /// at debug, without per-read serial trace.
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
                "multi64_sc64_l2=debug,multi64_ed64_l2=debug,multi64_ed64pro_l2=debug,multi64d=debug,tower_http=warn,info",
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
            cart: CartSetting::Auto,
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
    /// While running, the cart the live process was started for and the port it holds; while
    /// stopped, the Cart setting.
    pub cart: String,
    /// Only when the poll asked for it (Settings is open): the ports and auto pick, from the
    /// same enumeration as `message`, so the Settings hints track the Note without a second one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_options: Option<SerialPortOptions>,
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

/// Copy one of the child's output streams into the log, a line at a time, until it ends.
fn spawn_log_reader<R: std::io::Read + Send + 'static>(
    inner: Arc<Mutex<DaemonInner>>,
    stream: Option<R>,
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
        "The bridge (multi64d.exe) was not found. Build it with `cargo build -p multi64d` using the same profile as Multi64 (e.g. both debug or both release), then rebuild Multi64 so `src-tauri/resources/multi64d.exe` is copied, or place multi64d.exe next to Multi64's exe. You can also set MULTI64D_EXE to the full path. Checked: {}",
        tried.join("; ")
    ))
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
                "No SummerCart64 found on USB. Plug in the cart or pick its serial port in \
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortOptions {
    pub ports: Vec<String>,
    pub auto: Option<String>,
    /// Set whenever `auto` is `None`: why Auto has no port.
    pub auto_warning: Option<String>,
    /// More than one cart, so Start refuses to guess. `auto_warning` is set for no cart too, and
    /// with Cart on Auto-detect the two need different hints: no cart only means Start probes.
    pub ambiguous: bool,
    /// The Serial port hint for a chosen EverDrive on Auto, sent so its wording lives only in
    /// [`everdrive_needs_port!`] beside the status line's.
    pub everdrive_hint: &'static str,
}

/// Enumerate the ports once and derive both the list and the auto pick from it.
///
/// The port dropdown needs both, and asking for them separately meant two `available_ports()`
/// enumerations per refresh — plus a window where a cart plugged in between the two calls could
/// be picked as `auto` while being absent from the list the dropdown was built from.
fn serial_port_options() -> SerialPortOptions {
    options_from(serialport::available_ports().unwrap_or_default())
}

/// [`serial_port_options`] for one given enumeration.
fn options_from(ports: Vec<serialport::SerialPortInfo>) -> SerialPortOptions {
    let auto = pick_auto(&ports);
    SerialPortOptions {
        ambiguous: matches!(auto, AutoPort::Ambiguous(_)),
        auto_warning: auto.problem(),
        ports: ports.into_iter().map(|p| p.port_name).collect(),
        auto: auto.port(),
        everdrive_hint: EVERDRIVE_NEEDS_PORT_HINT,
    }
}

/// Why a fixed EverDrive cart has no port, with `$where` saying where to pick one. Both wordings
/// come from here so they cannot drift apart.
macro_rules! everdrive_needs_port {
    ($where:literal) => {
        concat!(
            "For a fixed Cart type, Auto-detect finds only a SummerCart64. Pick the \
             EverDrive's serial port",
            $where,
            ", or set Cart to Auto-detect."
        )
    };
}

/// For the status line and the daemon log, worded like [`AutoPort::problem`].
const EVERDRIVE_NEEDS_PORT: &str = everdrive_needs_port!(" in Settings");

/// For the Settings → Serial port hint, which is already in Settings.
const EVERDRIVE_NEEDS_PORT_HINT: &str = everdrive_needs_port!("");

/// What a start would do, decided without writing to any port.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StartPlan {
    /// The cart and port are known. `detected` when Auto-detect chose the cart (by USB descriptors).
    Ready {
        cart: DaemonCart,
        port: String,
        detected: bool,
    },
    /// Auto-detect has to ask: on this port, or on every serial port when `None`.
    Probe(Option<String>),
    /// Start cannot work; the reason is worded for the status line and the daemon log.
    Blocked(String),
}

impl StartPlan {
    /// The port the plan names, if any.
    fn port(&self) -> Option<String> {
        match self {
            StartPlan::Ready { port, .. } => Some(port.clone()),
            StartPlan::Probe(port) => port.clone(),
            StartPlan::Blocked(_) => None,
        }
    }
}

/// Plan a start from the saved port and Cart setting. `ports` runs only when the plan depends on
/// what is plugged in, so a fixed cart with a saved port never costs an enumeration.
///
/// - **A fixed cart** uses the saved port, else Auto's pick, which only ever finds a SummerCart64:
///   the X7's FT245R (`0403:6001`) is a stock FTDI part, and a PRO can only be recognised by
///   writing to it. Handing an EverDrive an SC64's port would run its framing against the wrong
///   cart, so a fixed EverDrive with no saved port is blocked.
/// - **Auto-detect** takes a SummerCart64 recognised by its USB descriptors (the saved port, or the
///   unique one on Auto) without sending anything, and otherwise plans a probe. Two SC64s on Auto
///   are still a guess, so they block rather than probe.
fn plan_start(
    saved: Option<&str>,
    cart: CartSetting,
    ports: impl FnOnce() -> Vec<serialport::SerialPortInfo>,
) -> StartPlan {
    let saved = saved.map(str::trim).filter(|s| !s.is_empty());
    match (cart.fixed(), saved) {
        (Some(cart), Some(port)) => StartPlan::Ready {
            cart,
            port: port.to_string(),
            detected: false,
        },
        (Some(DaemonCart::Sc64), None) => match pick_auto(&ports()) {
            AutoPort::Found(port) => StartPlan::Ready {
                cart: DaemonCart::Sc64,
                port,
                detected: false,
            },
            other => StartPlan::Blocked(other.problem().unwrap_or_default()),
        },
        (Some(_), None) => StartPlan::Blocked(EVERDRIVE_NEEDS_PORT.to_string()),
        (None, Some(port)) => {
            let sc64 = ports().iter().any(|p| {
                p.port_name.eq_ignore_ascii_case(port)
                    && matches!(&p.port_type, serialport::SerialPortType::UsbPort(usb) if usb_is_sc64(usb))
            });
            if sc64 {
                StartPlan::Ready {
                    cart: DaemonCart::Sc64,
                    port: port.to_string(),
                    detected: true,
                }
            } else {
                StartPlan::Probe(Some(port.to_string()))
            }
        }
        (None, None) => match pick_auto(&ports()) {
            AutoPort::Found(port) => StartPlan::Ready {
                cart: DaemonCart::Sc64,
                port,
                detected: true,
            },
            AutoPort::NoCart => StartPlan::Probe(None),
            ambiguous => StartPlan::Blocked(ambiguous.problem().unwrap_or_default()),
        },
    }
}

/// [`plan_start`] against the ports plugged in now.
fn start_plan(settings: &Settings) -> StartPlan {
    plan_start(settings.serial_port.as_deref(), settings.cart, || {
        serialport::available_ports().unwrap_or_default()
    })
}

/// Auto-detect by asking: `only` that port, or every port until one answers as a cart.
///
/// Every port given to `probe` receives its test commands. Across all ports, USB serial devices go
/// first and each group in name order, so the result does not depend on enumeration order; the
/// first cart to answer wins. `log` gets each outcome. `cancelled` is checked before each port and
/// after each probe, and `probe` should stop on it too.
fn detect_cart(
    only: Option<&str>,
    ports: &[serialport::SerialPortInfo],
    mut probe: impl FnMut(&str) -> Option<DetectedCart>,
    cancelled: impl Fn() -> bool,
    mut log: impl FnMut(String),
) -> Result<(DaemonCart, String), String> {
    let candidates: Vec<String> = match only {
        Some(port) => vec![port.to_string()],
        None => {
            let mut ranked: Vec<(bool, String)> = ports
                .iter()
                .map(|p| {
                    let usb = matches!(p.port_type, serialport::SerialPortType::UsbPort(_));
                    (!usb, p.port_name.clone())
                })
                .collect();
            ranked.sort();
            ranked.into_iter().map(|(_, name)| name).collect()
        }
    };
    if candidates.is_empty() {
        return Err("Auto-detect: no serial ports found. Plug in the cart.".to_string());
    }
    for port in &candidates {
        // Each probe can take seconds, and Exit waits for this start to finish (#147).
        if cancelled() {
            return Err(EXITING.to_string());
        }
        match probe(port) {
            Some(found) => {
                let cart = DaemonCart::from(found);
                log(format!("Auto-detect: {} answered on {port}", cart.label()));
                return Ok((cart, port.clone()));
            }
            // A probe cut short by Exit did not finish trying the port: don't log it as empty.
            None if cancelled() => return Err(EXITING.to_string()),
            None => log(format!("Auto-detect: no cart answered on {port}")),
        }
    }
    Err(match only {
        Some(port) => format!(
            "Auto-detect: no cart answered on {port}. Check the cart is plugged in and no other \
             program is using the serial port, or choose its Cart type in Settings."
        ),
        None => format!(
            "Auto-detect: no cart answered on any serial port ({}). Plug in the cart, or close \
             any program using its serial port.",
            candidates.join(", ")
        ),
    })
}

/// The cart and port for a start, probing when Auto-detect needs to, and whether Auto-detect
/// chose the cart. Probing writes to ports and can take seconds per port: call it off the UI
/// thread, and after stopping any daemon that holds the port.
fn resolve_start(
    settings: &Settings,
    mut log: impl FnMut(String),
) -> Result<(DaemonCart, String, bool), String> {
    match start_plan(settings) {
        StartPlan::Ready {
            cart,
            port,
            detected,
        } => {
            if detected {
                log(format!(
                    "Auto-detect: SummerCart64 on {port}, recognized by its USB IDs (nothing sent)"
                ));
            }
            Ok((cart, port, detected))
        }
        StartPlan::Blocked(why) => Err(why),
        StartPlan::Probe(only) => {
            log(match &only {
                Some(port) => format!(
                    "Auto-detect: {port} is not a SummerCart64 by its USB IDs; sending cart test commands to it"
                ),
                None => "Auto-detect: no SummerCart64 on USB; sending cart test commands to each serial port"
                    .to_string(),
            });
            let ports = serialport::available_ports().unwrap_or_default();
            let (cart, port) = detect_cart(
                only.as_deref(),
                &ports,
                |port| multi64_cart_probe::probe_port_cancellable(port, shutting_down),
                shutting_down,
                &mut log,
            )?;
            Ok((cart, port, true))
        }
    }
}

/// How the window's Cart row names a running daemon's cart: the cart and the port the live process
/// holds. Whether Auto-detect chose it is not repeated there; the start log says so.
fn running_cart_label(cart: DaemonCart, serial: &str) -> String {
    format!("{} on {serial}", cart.label())
}

/// Status → Note for a stopped daemon: what Start would do, or why it cannot. Empty when there is
/// nothing to add to the Bridge row's "Stopped".
fn stopped_message(plan: &StartPlan) -> String {
    match plan {
        StartPlan::Ready { .. } => String::new(),
        StartPlan::Probe(Some(port)) => format!("Start bridge will detect the cart on {port}"),
        StartPlan::Probe(None) => {
            "No SummerCart64 on USB; Start bridge will look for a cart on each serial port".into()
        }
        StartPlan::Blocked(why) => why.clone(),
    }
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

/// Set once Multi64 starts exiting. A start in progress checks it before spawning, and Auto-detect
/// checks it between ports and at every read while probing one, so Exit waits a fraction of a
/// second rather than for the rest of the probes (#147).
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

const EXITING: &str = "Multi64 is exiting, so the bridge was not started.";

fn shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::SeqCst)
}

fn begin_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
}

/// Whether something already accepts connections on the listen address.
///
/// A connect rather than a trial bind, which could briefly take the address from a daemon being
/// started. The timeout is short because a refused loopback connect takes about 2 s on Windows,
/// while one that is accepted completes in well under a millisecond. An address given as a
/// wildcard is tried on loopback.
fn listen_address_in_use(listen: &str) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpStream, ToSocketAddrs};
    let host = listen
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let Ok(addrs) = host.to_socket_addrs() else {
        return false;
    };
    addrs.into_iter().any(|mut addr| {
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr.ip() {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            });
        }
        TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(150)).is_ok()
    })
}

/// How long a check that found the listen address free is reused; see [`ListenChecks`].
const LISTEN_FREE_REUSE: std::time::Duration = std::time::Duration::from_secs(10);

/// The last listen-address check, shared by the status poll, the tray menu and Start.
static LISTEN_CHECKS: ListenChecks = ListenChecks::new();

/// Remembers the last [`listen_address_in_use`] answer.
///
/// Finding the address free is the expensive answer: on Windows a refused loopback connect takes
/// about 2 s, so it costs the whole 150 ms timeout, and the status poll asks every 2 s while the
/// bridge is stopped. So a free answer is reused for [`LISTEN_FREE_REUSE`]. An address in use
/// answers in well under a millisecond and is checked every time, so it shows as free as soon as
/// the other process lets go. Something taking a free address shows within the reuse window, except
/// to Start, which always checks.
struct ListenChecks {
    last: Mutex<Option<ListenCheck>>,
}

struct ListenCheck {
    listen: String,
    at: std::time::Instant,
    in_use: bool,
}

impl ListenChecks {
    const fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Run `check` and remember its answer.
    fn fresh(
        &self,
        listen: &str,
        now: std::time::Instant,
        check: impl FnOnce(&str) -> bool,
    ) -> bool {
        let in_use = check(listen);
        *self.last.lock() = Some(ListenCheck {
            listen: listen.to_string(),
            at: now,
            in_use,
        });
        in_use
    }

    /// False if `listen` was found free less than [`LISTEN_FREE_REUSE`] ago, else [`Self::fresh`].
    fn recent(
        &self,
        listen: &str,
        now: std::time::Instant,
        check: impl FnOnce(&str) -> bool,
    ) -> bool {
        let reuse = self.last.lock().as_ref().is_some_and(|c| {
            !c.in_use
                && c.listen == listen
                && now.saturating_duration_since(c.at) < LISTEN_FREE_REUSE
        });
        !reuse && self.fresh(listen, now, check)
    }
}

/// Whether the listen address is in use now, for Start.
fn listen_in_use_now(listen: &str) -> bool {
    LISTEN_CHECKS.fresh(listen, std::time::Instant::now(), listen_address_in_use)
}

/// Whether the listen address is in use, reusing a recent free answer, for the status poll and tray.
fn listen_in_use_recently(listen: &str) -> bool {
    LISTEN_CHECKS.recent(listen, std::time::Instant::now(), listen_address_in_use)
}

/// Why Start refuses while something else holds the listen address.
fn listen_in_use_message(listen: &str) -> String {
    format!(
        "Something is already listening on {listen}, most likely a bridge left running after \
         Multi64 closed unexpectedly. Multi64 can only stop a bridge it started: end multi64d.exe \
         in Task Manager, or choose another Listen address in Settings, then start the bridge again."
    )
}

/// A job object whose processes are killed when its last handle closes (#146).
///
/// The daemon is assigned to one that lives as long as Multi64. Windows closes the handle when
/// Multi64's process ends however it ends — a panic, Task Manager, sign-out — so the bridge can no
/// longer outlive it holding the serial port and the listen address.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct KillOnCloseJob(HANDLE);

    // SAFETY: the handle names a kernel object, usable from any thread; nothing here is thread-local.
    unsafe impl Send for KillOnCloseJob {}
    unsafe impl Sync for KillOnCloseJob {}

    fn failed(call: &str) -> String {
        format!("{call} failed: {}", std::io::Error::last_os_error())
    }

    impl KillOnCloseJob {
        pub fn new() -> Result<Self, String> {
            // SAFETY: no attributes and no name create an unnamed job with default security.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(failed("CreateJobObjectW"));
            }
            // Owned from here, so an early return below closes it.
            let job = Self(handle);
            let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: `info` is the structure this information class takes, passed with its size.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&info) as u32,
                )
            };
            if ok == 0 {
                return Err(failed("SetInformationJobObject"));
            }
            Ok(job)
        }

        pub fn assign(&self, child: &Child) -> Result<(), String> {
            // SAFETY: the job handle is open for `self`'s life and the process handle for `child`'s.
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
            if ok == 0 {
                return Err(failed("AssignProcessToJobObject"));
            }
            Ok(())
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY: the handle is open and owned by this value alone.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Elsewhere there is no job object; the daemon is stopped only by Exit.
#[cfg(not(windows))]
mod job {
    use std::process::Child;

    pub struct KillOnCloseJob;

    impl KillOnCloseJob {
        pub fn new() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn assign(&self, _child: &Child) -> Result<(), String> {
            Ok(())
        }
    }
}

/// Put `child` in the job Multi64 holds for its whole life, created on first use.
fn tie_to_multi64_lifetime(child: &Child) -> Result<(), String> {
    static JOB: std::sync::OnceLock<Result<job::KillOnCloseJob, String>> =
        std::sync::OnceLock::new();
    match JOB.get_or_init(job::KillOnCloseJob::new) {
        Ok(job) => job.assign(child),
        Err(e) => Err(e.clone()),
    }
}

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
fn daemon_args(serial: &str, cart: DaemonCart, settings: &Settings) -> Vec<String> {
    vec![
        "--serial".into(),
        serial.into(),
        // baud is a spawn-time argument like the rest: multi64d takes --baud and feeds it to the
        // L2 pipe. It used to be collected and saved by the UI but never passed, so the setting
        // did nothing and the daemon always ran at its own 115200 default.
        "--baud".into(),
        settings.baud.to_string(),
        "--cart".into(),
        cart.arg().into(),
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
    let started = {
        let _ops = DAEMON_OPS.lock();
        start_daemon_locked(app, daemon, settings)
    };
    // Only once DAEMON_OPS is released (#221). A Rust listener runs on the emitting thread, and the
    // tray's listener rebuilds the menu through the main thread and waits for it; the main thread,
    // running Exit, may itself be waiting in `kill_daemon` for DAEMON_OPS. Emitting under the lock
    // could leave each waiting on the other, and Multi64 hung on exit.
    if started.is_ok() {
        let _ = app.emit("daemon-changed", ());
    }
    started
}

/// [`start_daemon`] for a caller holding [`DAEMON_OPS`]. Emits nothing: see there.
fn start_daemon_locked(
    app: &tauri::AppHandle,
    daemon: &Arc<Mutex<DaemonInner>>,
    settings: &Settings,
) -> Result<(), String> {
    kill_daemon_locked(daemon);
    if shutting_down() {
        return Err(EXITING.to_string());
    }
    {
        let mut inner = daemon.lock();
        inner.logs.clear();
    }
    // After the kill, so what is left on the address is not ours. A daemon that cannot bind exits
    // at once, and before this the window only showed Stopped with no way to stop the other one
    // (#146). Checked before Auto-detect, which would otherwise probe ports that process holds.
    // Always a fresh check, which also updates what the tray reads when this start's failure
    // rebuilds it.
    if listen_in_use_now(&settings.listen) {
        return Err(listen_in_use_message(&settings.listen));
    }
    // After the kill, so the port the old daemon held is free to probe.
    let (cart, serial, detected) = resolve_start(settings, |line| push_log(daemon, line))?;
    let log_label = match settings.multi64d_log_preset {
        Multi64dLogPreset::Default => "default",
        Multi64dLogPreset::Debug => "debug",
        Multi64dLogPreset::SerialTrace => "serial-trace",
        Multi64dLogPreset::Verbose => "verbose",
    };
    push_log(
        daemon,
        format!(
            "Starting multi64d on {serial} for {}{} ({}) [logging: {log_label}]",
            cart.label(),
            if detected {
                ", found by Auto-detect"
            } else {
                ""
            },
            settings.listen
        ),
    );
    let unproven = match cart {
        DaemonCart::Sc64 => None,
        DaemonCart::Ed64 => Some("EverDrive-64 X7 support is experimental and has run on one cart"),
        DaemonCart::Ed64Pro => {
            Some("EverDrive-64 PRO support is experimental and has never been run against a cart")
        }
    };
    if let Some(status) = unproven {
        push_log(
            daemon,
            format!("{status}: a running bridge does not show that the cart link works."),
        );
    }

    let daemon_path = resolve_multi64d_path(app)?;
    let mut cmd = Command::new(&daemon_path);
    cmd.env("NO_COLOR", "1")
        .args(daemon_args(&serial, cart, settings))
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .stdin(Stdio::null());
    apply_multi64d_log_preset(&mut cmd, settings.multi64d_log_preset);
    command_no_window(&mut cmd);

    // Probing can take seconds; an Exit clicked meanwhile must not get a daemon it then kills.
    if shutting_down() {
        return Err(EXITING.to_string());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start the bridge: {e}"))?;
    if let Err(e) = tie_to_multi64_lifetime(&child) {
        push_log(
            daemon,
            format!("The bridge may keep running if Multi64 closes unexpectedly: {e}"),
        );
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let d = Arc::clone(daemon);
    spawn_log_reader(Arc::clone(&d), stdout, "");
    spawn_log_reader(d, stderr, "[stderr] ");

    let mut inner = daemon.lock();
    inner.child = Some(child);
    inner.serial = Some(serial);
    inner.cart = cart;
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
    commit_settings(
        &prev,
        &settings,
        || multi64_auto_launch().ok()?.is_enabled().ok(),
        set_autostart_windows_impl,
        save_settings,
    )?;
    *state.settings.lock() = settings.clone();
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

/// Write the sign-in entry and the settings file so that they cannot disagree (#148).
///
/// The file used to be saved first and the registry written after, so a failed write left the file
/// saying autostart was on. Later saves compared against that file, saw no change and never
/// retried, and Multi64 silently did not start at sign-in. Now the registry is written first, the
/// file only if that worked, and the registry is put back if the file cannot be written. The
/// comparison is with what Windows has (`autostart_enabled`, `None` when it cannot be read, which
/// falls back to the file), so a file and registry that already disagree are reconciled on the
/// next save.
fn commit_settings(
    prev: &Settings,
    next: &Settings,
    autostart_enabled: impl FnOnce() -> Option<bool>,
    mut set_autostart: impl FnMut(bool) -> Result<(), String>,
    save: impl FnOnce(&Settings) -> Result<(), String>,
) -> Result<(), String> {
    let current = autostart_enabled().unwrap_or(prev.autostart_app);
    let changed = current != next.autostart_app;
    if changed {
        set_autostart(next.autostart_app)?;
    }
    if let Err(e) = save(next) {
        if changed {
            if let Err(undo) = set_autostart(current) {
                return Err(format!(
                    "{e} (the Windows sign-in setting could not be put back either: {undo})"
                ));
            }
        }
        return Err(e);
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
/// this every 2 s. `with_ports` adds [`DaemonStatus::port_options`], for the Settings hints.
#[tauri::command]
async fn get_daemon_status(
    app: tauri::AppHandle,
    with_ports: bool,
) -> Result<DaemonStatus, String> {
    on_blocking_pool(&app, "get_daemon_status", move |_, state| {
        daemon_status(state, with_ports)
    })
    .await
}

/// The stopped Note and, when `with_ports`, the port options, from at most one call to
/// `enumerate` however many of them need the ports.
fn note_and_port_options(
    running: bool,
    settings: &Settings,
    with_ports: bool,
    enumerate: impl FnOnce() -> Vec<serialport::SerialPortInfo>,
) -> (String, Option<SerialPortOptions>) {
    let mut enumerate = Some(enumerate);
    let mut enumerated: Option<Vec<serialport::SerialPortInfo>> = None;
    let mut ports = || {
        enumerated
            .get_or_insert_with(|| enumerate.take().map(|f| f()).unwrap_or_default())
            .clone()
    };
    let message = if !running {
        // Name what Start would do, or why it cannot. Without a saved port this enumerates,
        // which only happens while stopped; it never probes a port.
        stopped_message(&plan_start(
            settings.serial_port.as_deref(),
            settings.cart,
            &mut ports,
        ))
    } else {
        // The Bridge and Health rows already say running and whether it responds.
        String::new()
    };
    let port_options = with_ports.then(|| options_from(ports()));
    (message, port_options)
}

fn daemon_status(state: &AppState, with_ports: bool) -> DaemonStatus {
    let settings = state.settings.lock().clone();
    let listen = settings.listen.clone();
    let running = daemon_is_running(&state.daemon);
    // Read after `daemon_is_running` returns: it takes the daemon lock itself.
    let cart = if running {
        let inner = state.daemon.lock();
        match inner.serial.as_deref() {
            Some(serial) => running_cart_label(inner.cart, serial),
            None => inner.cart.label().to_string(),
        }
    } else {
        settings.cart.label().to_string()
    };
    let healthy = running && check_health(&listen);
    let (mut message, port_options) = note_and_port_options(running, &settings, with_ports, || {
        serialport::available_ports().unwrap_or_default()
    });
    // A failed auto-start is otherwise only in the log, behind Developer mode (#146).
    if !running && listen_in_use_recently(&listen) {
        message = listen_in_use_message(&listen);
    }
    DaemonStatus {
        running,
        healthy,
        listen,
        message,
        cart,
        port_options,
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

/// Basenames we search for (NSIS / MSI / legacy installs).
#[cfg(feature = "xfer64")]
const XFER64_EXE_NAMES: &[&str] = &["Xfer64.exe", "xfer64.exe", "multi64-cart-explorer.exe"];

/// Windows: resolve installed Xfer64 from registry (NSIS and WiX/MSI register App Paths and/or Uninstall).
#[cfg(windows)]
#[cfg(feature = "xfer64")]
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

#[cfg(feature = "xfer64")]
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

#[cfg(feature = "xfer64")]
fn first_xfer64_exe() -> Option<PathBuf> {
    xfer64_exe_candidates().into_iter().find(|p| p.is_file())
}

#[cfg(feature = "xfer64")]
fn is_xfer64_installed() -> bool {
    first_xfer64_exe().is_some()
}

/// True when `p` is a non-placeholder bundled installer (build.rs writes 0 bytes for the unused kind).
#[cfg(feature = "xfer64")]
fn xfer64_installer_is_valid(p: &Path) -> bool {
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.len() > 1024)
            .unwrap_or(false)
}

/// Same layout rules as [`resolve_multi64d_path`]: bundled `resources/...` often lands under
/// `$RESOURCE_DIR/resources/` (see `tauri.conf.json` `bundle.resources`).
#[cfg(feature = "xfer64")]
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

/// What the window needs to decide whether to offer Xfer64. In a standalone build both are
/// always false and the card is not rendered at all — see [`build_info`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Xfer64State {
    pub installed: bool,
    pub installer_available: bool,
}

#[cfg(feature = "xfer64")]
fn xfer64_state(app: &AppHandle) -> Xfer64State {
    Xfer64State {
        installed: is_xfer64_installed(),
        installer_available: resolve_xfer64_installer_path(app).is_some(),
    }
}

/// What this build of Multi64 contains, so the window can leave out what is not there rather than
/// showing a control that cannot work.
///
/// A build-time fact, not a runtime one: the standalone package carries no Xfer64 installer and
/// none of the code that looks for an installed copy, so the card is never rendered at all.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub xfer64: bool,
}

#[tauri::command]
fn build_info() -> BuildInfo {
    BuildInfo {
        xfer64: cfg!(feature = "xfer64"),
    }
}

/// Blocking: reads the uninstall registry keys and stats every candidate install path.
///
/// The command stays registered in a standalone build so the invoke surface does not change shape
/// between builds; it reports both flags false, and the window hides the card before ever calling
/// it (see [`build_info`]).
#[tauri::command]
async fn get_xfer64_state(app: tauri::AppHandle) -> Result<Xfer64State, String> {
    #[cfg(not(feature = "xfer64"))]
    {
        let _ = &app;
        Ok(Xfer64State {
            installed: false,
            installer_available: false,
        })
    }
    #[cfg(feature = "xfer64")]
    {
        on_blocking_pool(&app, "get_xfer64_state", |app, _| xfer64_state(app)).await
    }
}

/// If Xfer64 is installed, launch it. Otherwise run the bundled installer (`xfer64-setup.exe` or `xfer64-setup.msi`).
/// Blocking: the same registry and path search as [`get_xfer64_state`], then a process spawn.
#[tauri::command]
async fn launch_or_install_xfer64(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(not(feature = "xfer64"))]
    {
        let _ = &app;
        Err("this build of Multi64 does not include Xfer64".into())
    }
    #[cfg(feature = "xfer64")]
    {
        on_blocking_pool(&app, "launch_or_install_xfer64", |app, _| {
            launch_or_install_xfer64_blocking(app)
        })
        .await?
    }
}

#[cfg(feature = "xfer64")]
fn launch_or_install_xfer64_blocking(app: &AppHandle) -> Result<(), String> {
    if let Some(exe) = first_xfer64_exe() {
        return Command::new(&exe)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ());
    }
    let Some(installer_path) = resolve_xfer64_installer_path(app) else {
        return Err(
            "The Xfer64 installer is not bundled. Build Xfer64, then build Multi64 (see crates/multi64/README.md: NSIS vs MSI pairing)."
                .into(),
        );
    };
    launch_xfer64_bundled_installer(&installer_path)
}

#[cfg(feature = "xfer64")]
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

/// The note after the tray's status line. SC64 is the default and needs no mention; any other cart
/// is named, so an experimental daemon is never mistaken for the proven one, and a stopped daemon on
/// Auto-detect says so. A running daemon is named for what it was started with, by the cart's name
/// alone: the status line before it already names the port.
fn tray_cart_note(
    running: bool,
    spawned: DaemonCart,
    setting: CartSetting,
) -> Option<&'static str> {
    if running {
        (spawned != DaemonCart::Sc64).then(|| spawned.label())
    } else {
        (setting != CartSetting::Sc64).then(|| setting.label())
    }
}

/// `can_start` is whether Start can work at all: Auto-detect can start with no port known yet.
/// `listen_in_use` is whether something else already listens on `listen` while stopped, so Start
/// would fail with [`listen_in_use_message`].
fn tray_labels(
    running: bool,
    serial: Option<&str>,
    can_start: bool,
    cart_note: Option<&str>,
    listen: &str,
    listen_in_use: bool,
) -> TrayLabels {
    let cart_note = cart_note.map(|c| format!(" · {c}")).unwrap_or_default();
    let status = if running {
        match serial {
            Some(port) => format!("Bridge: running on {port}"),
            // No configured or auto-detected port, but a live process: report where it
            // listens rather than claiming a port we cannot name.
            None => format!("Bridge: running ({listen})"),
        }
    } else if listen_in_use {
        // Labelled, not greyed, as the window's Start button is: the menu is rebuilt only when the
        // bridge changes, so a greyed Start would stay greyed after the other process has gone.
        // Named before a missing port, because Start checks the address first.
        "Bridge: stopped (listen address in use)".to_string()
    } else if can_start {
        "Bridge: stopped".to_string()
    } else {
        // Start is greyed out below; say why, since the tray has no room for the full reason.
        "Bridge: stopped (no serial port)".to_string()
    };
    TrayLabels {
        status: format!("{status}{cart_note}"),
        toggle: if running {
            "Stop bridge"
        } else {
            "Start bridge"
        },
        // Stopping always works; starting needs a port or a probe, and fails otherwise.
        toggle_enabled: running || can_start,
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
#[cfg(feature = "xfer64")]
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
/// window shows the finer-grained health. While stopped it does check the listen address, through
/// [`listen_in_use_recently`]: under a millisecond while the address is taken, and usually a reused
/// answer while it is free.
fn build_tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let (running, listen, serial, can_start, cart_note) = match app.try_state::<AppState>() {
        Some(state) => {
            let settings = state.settings.lock().clone();
            // `daemon_is_running` takes the daemon lock itself, so read the port after it returns.
            let running = daemon_is_running(&state.daemon);
            let (spawned, spawned_cart) = {
                let inner = state.daemon.lock();
                (inner.serial.clone(), inner.cart)
            };
            // A stopped daemon is described by what Start would do, which never probes a port.
            let plan = (!running).then(|| start_plan(&settings));
            let can_start = !matches!(plan, Some(StartPlan::Blocked(_)));
            (
                running,
                settings.listen.clone(),
                tray_serial(running, spawned, || plan.as_ref().and_then(StartPlan::port)),
                can_start,
                tray_cart_note(running, spawned_cart, settings.cart),
            )
        }
        None => (false, DEFAULT_LISTEN.to_string(), None, false, None),
    };
    // Without app state there are no settings, so nothing to check.
    let listen_in_use =
        !running && app.try_state::<AppState>().is_some() && listen_in_use_recently(&listen);

    let labels = tray_labels(
        running,
        serial.as_deref(),
        can_start,
        cart_note,
        &listen,
        listen_in_use,
    );
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
        "Restart bridge",
        labels.restart_enabled,
        None::<&str>,
    )?;

    #[cfg(feature = "xfer64")]
    let xfer = {
        let xfer_state = xfer64_state(app);
        let (xfer_text, xfer_enabled) = xfer64_label(Some(&xfer_state));
        MenuItem::with_id(app, "xfer64", xfer_text, xfer_enabled, None::<&str>)?
    };

    let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Exit Multi64", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    #[cfg(feature = "xfer64")]
    let sep3 = PredefinedMenuItem::separator(app)?;

    // Built as a list rather than a fixed array so the standalone build simply leaves the Xfer64
    // entry and its separator out, instead of showing a permanently greyed item for something the
    // package does not contain.
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        vec![&status, &sep1, &toggle, &restart, &sep2];
    #[cfg(feature = "xfer64")]
    {
        items.push(&xfer);
        items.push(&sep3);
    }
    items.push(&show);
    items.push(&quit);

    Menu::with_items(app, &items)
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
            build_info,
            get_serial_port_options,
            get_settings,
            set_settings,
            get_daemon_status,
            get_daemon_logs,
            clear_daemon_logs,
            daemon_start,
            daemon_stop,
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
                        // Off the event loop (#147): `kill_daemon` waits on DAEMON_OPS, which a
                        // start holds for its whole Auto-detect probe. The flag makes that start
                        // stop probing at its next read, without spawning.
                        "quit" => {
                            begin_shutdown();
                            let app = app.clone();
                            tauri::async_runtime::spawn_blocking(move || {
                                if let Some(s) = app.try_state::<AppState>() {
                                    kill_daemon(&s.daemon);
                                }
                                app.exit(0);
                            });
                        }
                        "show" => show_main_window(app),
                        // Off the event loop: starting can probe serial ports for seconds.
                        "toggle" => {
                            let app = app.clone();
                            tauri::async_runtime::spawn_blocking(move || {
                                let Some(s) = app.try_state::<AppState>() else {
                                    return;
                                };
                                if daemon_is_running(&s.daemon) {
                                    kill_daemon(&s.daemon);
                                } else {
                                    let settings = s.settings.lock().clone();
                                    if let Err(e) = start_daemon(&app, &s.daemon, &settings) {
                                        push_log(&s.daemon, format!("Start failed: {e}"));
                                    }
                                }
                                let _ = app.emit("daemon-changed", ());
                            });
                        }
                        "restart" => {
                            let app = app.clone();
                            tauri::async_runtime::spawn_blocking(move || {
                                let Some(s) = app.try_state::<AppState>() else {
                                    return;
                                };
                                kill_daemon(&s.daemon);
                                let settings = s.settings.lock().clone();
                                if let Err(e) = start_daemon(&app, &s.daemon, &settings) {
                                    push_log(&s.daemon, format!("Restart failed: {e}"));
                                }
                                let _ = app.emit("daemon-changed", ());
                            });
                        }
                        #[cfg(feature = "xfer64")]
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
                match start_plan(&settings_for_setup) {
                    // Skipping the auto-start used to leave no trace; the log now says why.
                    StartPlan::Blocked(why) => {
                        push_log(&daemon_for_setup, format!("multi64d not started: {why}"))
                    }
                    // A thread of its own: Auto-detect may probe ports for seconds.
                    _ => {
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
                // Set first, so a start still probing stops at its next read.
                begin_shutdown();
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
            CartSetting::Auto,
            "files predating the setting auto-detect"
        );
    }

    #[test]
    fn a_saved_everdrive_cart_is_read_back() {
        let json =
            r#"{"serialPort":"COM6","baud":115200,"listen":"127.0.0.1:38765","cart":"ed64"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.cart, CartSetting::Ed64);
        let json =
            r#"{"serialPort":"COM8","baud":115200,"listen":"127.0.0.1:38765","cart":"ed64pro"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.cart, CartSetting::Ed64Pro);
    }

    /// The settings file and `--cart` must use multi64d's own names, or the daemon refuses to
    /// start (clap rejects an unknown value) or reads a config it does not understand.
    #[test]
    fn cart_names_match_multi64d() {
        for cart in [DaemonCart::Sc64, DaemonCart::Ed64, DaemonCart::Ed64Pro] {
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
        let sc64 = daemon_args("COM4", DaemonCart::Sc64, &Settings::default());
        assert_eq!(flag(&sc64, "--cart"), "sc64");
        assert_eq!(flag(&sc64, "--serial"), "COM4");
        let slow = Settings {
            baud: 57600,
            ..Settings::default()
        };
        let ed64 = daemon_args("COM6", DaemonCart::Ed64, &slow);
        assert_eq!(flag(&ed64, "--cart"), "ed64");
        assert_eq!(flag(&ed64, "--baud"), "57600");
        let pro = daemon_args("COM8", DaemonCart::Ed64Pro, &Settings::default());
        assert_eq!(flag(&pro, "--cart"), "ed64pro");
    }

    /// `auto` is a Multi64 setting, never a daemon argument; the fixed values are the daemon's.
    #[test]
    fn cart_settings_map_onto_daemon_carts() {
        assert_eq!(CartSetting::default(), CartSetting::Auto);
        assert_eq!(CartSetting::Auto.fixed(), None);
        assert_eq!(CartSetting::Auto.label(), "Auto-detect");
        assert_eq!(
            serde_json::to_string(&CartSetting::Auto).unwrap(),
            "\"auto\""
        );
        for (setting, cart) in [
            (CartSetting::Sc64, DaemonCart::Sc64),
            (CartSetting::Ed64, DaemonCart::Ed64),
            (CartSetting::Ed64Pro, DaemonCart::Ed64Pro),
        ] {
            assert_eq!(setting.fixed(), Some(cart));
            assert_eq!(
                serde_json::to_string(&setting).unwrap(),
                serde_json::to_string(&cart).unwrap()
            );
        }
        assert_eq!(DaemonCart::from(DetectedCart::Sc64), DaemonCart::Sc64);
        assert_eq!(DaemonCart::from(DetectedCart::Ed64Pro).arg(), "ed64pro");
        assert_eq!(DaemonCart::from(DetectedCart::Ed64).arg(), "ed64");
    }

    #[test]
    fn a_running_cart_is_named_with_its_port() {
        assert_eq!(
            running_cart_label(DaemonCart::Sc64, "COM4"),
            "SummerCart64 on COM4"
        );
        assert_eq!(
            running_cart_label(DaemonCart::Ed64Pro, "COM6"),
            "EverDrive-64 PRO (beta) on COM6"
        );
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

    fn with_autostart(autostart_app: bool) -> Settings {
        Settings {
            autostart_app,
            ..Settings::default()
        }
    }

    /// #148: a failed sign-in change used to be saved anyway, so the box stayed ticked.
    #[test]
    fn a_failed_autostart_change_is_not_saved() {
        let mut saved = false;
        let got = commit_settings(
            &with_autostart(false),
            &with_autostart(true),
            || Some(false),
            |_| Err("access denied".to_string()),
            |_| {
                saved = true;
                Ok(())
            },
        );
        assert_eq!(got, Err("access denied".to_string()));
        assert!(!saved, "the file must not say autostart is on");
    }

    /// A file that could not be written leaves Windows as it was, not ahead of the file.
    #[test]
    fn a_failed_save_undoes_the_autostart_change() {
        let mut writes = Vec::new();
        let got = commit_settings(
            &with_autostart(false),
            &with_autostart(true),
            || Some(false),
            |on| {
                writes.push(on);
                Ok(())
            },
            |_| Err("disk full".to_string()),
        );
        assert_eq!(got, Err("disk full".to_string()));
        assert_eq!(writes, [true, false]);
    }

    /// #148: once the file said on while Windows said off, later saves compared with the file and
    /// never retried. They compare with Windows now.
    #[test]
    fn autostart_is_retried_when_windows_disagrees_with_the_file() {
        let mut writes = Vec::new();
        commit_settings(
            &with_autostart(true),
            &with_autostart(true),
            || Some(false),
            |on| {
                writes.push(on);
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(writes, [true]);
    }

    #[test]
    fn autostart_is_left_alone_when_nothing_changes() {
        for on in [false, true] {
            // Unreadable counts as what the file last said.
            for read in [Some(on), None] {
                commit_settings(
                    &with_autostart(on),
                    &with_autostart(on),
                    || read,
                    |_| panic!("nothing to write"),
                    |_| Ok(()),
                )
                .unwrap();
            }
        }
    }

    /// #146: a bridge left behind by a crashed Multi64 still holds the listen address; Start
    /// must say so instead of spawning a daemon that exits at once.
    #[test]
    fn a_listener_on_the_listen_address_is_detected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        assert!(listen_address_in_use(&addr), "{addr}");
        assert!(listen_address_in_use(&format!("http://{addr}/")), "{addr}");
        drop(listener);
        assert!(!listen_address_in_use(&addr), "{addr}");
        assert!(!listen_address_in_use("not an address"));
        let msg = listen_in_use_message("127.0.0.1:38765");
        assert!(
            msg.contains("127.0.0.1:38765") && msg.contains("multi64d.exe"),
            "{msg}"
        );
    }

    /// The status poll paid a 150 ms connect every 2 s while stopped. A free answer is reused for a
    /// while; an in-use one, which is cheap and must clear as soon as the address frees, never is.
    #[test]
    fn a_free_listen_address_is_reused_briefly_and_an_address_in_use_never() {
        use std::time::{Duration, Instant};
        const A: &str = "127.0.0.1:38765";
        let checks = ListenChecks::new();
        let t0 = Instant::now();
        let never = |_: &str| -> bool { panic!("the free answer should be reused") };

        assert!(!checks.recent(A, t0, |_| false));
        assert!(!checks.recent(A, t0 + Duration::from_secs(9), never));
        // Another address, or the reuse window over, checks again.
        assert!(checks.recent("127.0.0.1:1", t0, |_| true));
        assert!(!checks.recent(A, t0, |_| false));
        assert!(checks.recent(A, t0 + LISTEN_FREE_REUSE, |_| true));

        // In use is checked every time, so the address shows free the moment it is let go.
        let mut asked = 0;
        assert!(checks.recent(A, t0, |_| {
            asked += 1;
            true
        }));
        assert!(!checks.recent(A, t0, |_| {
            asked += 1;
            false
        }));
        assert_eq!(asked, 2);

        // Start always checks, and what it finds is what the tray reads next.
        assert!(checks.fresh(A, t0, |_| true));
        assert!(checks.recent(A, t0, |_| true));
        assert!(!checks.fresh(A, t0, |_| false));
        assert!(!checks.recent(A, t0, never));
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
    fn two_carts_is_ambiguous_rather_than_a_guess() {
        let ports = [sc64_windows("COM8"), ch340("COM5"), sc64_windows("COM4")];
        let got = pick_auto(&ports);
        assert_eq!(got, AutoPort::Ambiguous(vec!["COM4".into(), "COM8".into()]));
        let problem = got.problem().unwrap();
        assert!(problem.contains("COM4, COM8"), "{problem}");
    }

    fn pci(name: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_string(),
            port_type: SerialPortType::PciPort,
        }
    }

    fn ready(cart: DaemonCart, port: &str, detected: bool) -> StartPlan {
        StartPlan::Ready {
            cart,
            port: port.to_string(),
            detected,
        }
    }

    #[test]
    fn a_saved_port_wins_without_enumerating() {
        let got = plan_start(Some("COM5"), CartSetting::Sc64, || {
            panic!("a fixed cart with a saved port must not enumerate")
        });
        assert_eq!(got, ready(DaemonCart::Sc64, "COM5", false));
    }

    #[test]
    fn an_empty_saved_port_means_auto() {
        let got = plan_start(Some(""), CartSetting::Sc64, || vec![sc64_windows("COM4")]);
        assert_eq!(got, ready(DaemonCart::Sc64, "COM4", false));
        let got = plan_start(None, CartSetting::Sc64, || vec![sc64_windows("COM4")]);
        assert_eq!(got, ready(DaemonCart::Sc64, "COM4", false));
    }

    /// Auto cannot identify an EverDrive, and must not hand a fixed EverDrive an SC64's port.
    #[test]
    fn a_fixed_everdrive_without_a_port_is_blocked_without_enumerating() {
        for cart in [CartSetting::Ed64, CartSetting::Ed64Pro] {
            let got = plan_start(None, cart, || {
                panic!("an EverDrive must not fall back to the SC64 auto pick")
            });
            assert!(
                matches!(&got, StartPlan::Blocked(why) if why.contains("Auto-detect")),
                "{got:?}"
            );
        }
        let got = plan_start(Some("COM6"), CartSetting::Ed64, || {
            panic!("a saved port needs no enumeration")
        });
        assert_eq!(got, ready(DaemonCart::Ed64, "COM6", false));
    }

    /// No cart on Auto is an error carrying the reason, not some other port.
    #[test]
    fn a_fixed_sc64_with_no_cart_on_auto_is_blocked_with_the_reason() {
        let got = plan_start(None, CartSetting::Sc64, || vec![ch340("COM5")]);
        assert_eq!(got, StartPlan::Blocked(AutoPort::NoCart.problem().unwrap()));
        let got = plan_start(None, CartSetting::Sc64, || {
            vec![sc64_windows("COM4"), sc64_windows("COM8")]
        });
        assert!(matches!(&got, StartPlan::Blocked(why) if why.contains("More than one")));
    }

    /// The common case sends nothing: an SC64 recognised by its descriptors.
    #[test]
    fn auto_detect_takes_an_sc64_by_its_usb_ids_without_probing() {
        let got = plan_start(None, CartSetting::Auto, || {
            vec![ch340("COM5"), sc64_windows("COM4")]
        });
        assert_eq!(got, ready(DaemonCart::Sc64, "COM4", true));
        let got = plan_start(Some("com4"), CartSetting::Auto, || {
            vec![sc64_windows("COM4")]
        });
        assert_eq!(got, ready(DaemonCart::Sc64, "com4", true));
    }

    #[test]
    fn auto_detect_probes_a_saved_port_that_is_not_an_sc64_and_only_that_port() {
        let got = plan_start(Some("COM6"), CartSetting::Auto, || vec![ch340("COM5")]);
        assert_eq!(got, StartPlan::Probe(Some("COM6".into())));
        assert_eq!(got.port().as_deref(), Some("COM6"));
    }

    #[test]
    fn auto_detect_without_an_sc64_probes_every_port_but_two_sc64s_still_block() {
        let got = plan_start(None, CartSetting::Auto, || vec![ch340("COM5")]);
        assert_eq!(got, StartPlan::Probe(None));
        assert_eq!(got.port(), None);
        let got = plan_start(None, CartSetting::Auto, || {
            vec![sc64_windows("COM4"), sc64_windows("COM8")]
        });
        assert!(matches!(got, StartPlan::Blocked(_)));
    }

    #[test]
    fn detect_tries_usb_ports_first_in_name_order_and_stops_at_the_first_cart() {
        let ports = [pci("COM1"), ch340("COM7"), ch340("COM3")];
        let mut tried = Vec::new();
        let mut logged = Vec::new();
        let got = detect_cart(
            None,
            &ports,
            |port| {
                tried.push(port.to_string());
                (port == "COM7").then_some(DetectedCart::Ed64Pro)
            },
            || false,
            |line| logged.push(line),
        );
        assert_eq!(got, Ok((DaemonCart::Ed64Pro, "COM7".to_string())));
        assert_eq!(tried, ["COM3", "COM7"], "COM1 is not USB and comes last");
        assert_eq!(logged.len(), 2);
        assert!(logged[1].contains("EverDrive-64 PRO"), "{logged:?}");
    }

    #[test]
    fn detect_names_every_port_it_tried_when_nothing_answers() {
        let ports = [pci("COM1"), ch340("COM3")];
        let err = detect_cart(None, &ports, |_| None, || false, |_| {}).unwrap_err();
        assert!(err.contains("COM3, COM1"), "{err}");
        let err =
            detect_cart(None, &[], |_| panic!("no ports to probe"), || false, |_| {}).unwrap_err();
        assert!(err.contains("no serial ports"), "{err}");
    }

    #[test]
    fn detect_on_a_saved_port_probes_only_that_port() {
        let mut tried = Vec::new();
        let err = detect_cart(
            Some("COM9"),
            &[ch340("COM3")],
            |port| {
                tried.push(port.to_string());
                None
            },
            || false,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(tried, ["COM9"]);
        assert!(err.contains("COM9"), "{err}");
    }

    /// #147: Exit waited for every port's probe (about 3 s each) before the window could close.
    /// Detection now stops at the next port once Multi64 is exiting.
    #[test]
    fn detect_stops_probing_once_multi64_is_exiting() {
        let ports = [ch340("COM3"), ch340("COM5"), ch340("COM7")];
        let exiting = std::cell::Cell::new(false);
        let mut tried = Vec::new();
        let mut logged = Vec::new();
        let err = detect_cart(
            None,
            &ports,
            |port| {
                tried.push(port.to_string());
                exiting.set(true);
                None
            },
            || exiting.get(),
            |line| logged.push(line),
        )
        .unwrap_err();
        assert_eq!(tried, ["COM3"], "no port after the Exit is probed");
        assert_eq!(err, EXITING);
        // The probe on COM3 was cut short, so COM3 was never fully tried.
        assert!(logged.is_empty(), "{logged:?}");
        // Already exiting: nothing is sent at all.
        let err = detect_cart(Some("COM9"), &[], |_| panic!("probed"), || true, |_| {});
        assert_eq!(err.unwrap_err(), EXITING);
    }

    #[test]
    fn a_stopped_daemon_says_what_start_will_do() {
        // Nothing to add to the Bridge row's "Stopped", so the Note stays empty.
        assert_eq!(stopped_message(&ready(DaemonCart::Sc64, "COM4", true)), "");
        assert!(stopped_message(&StartPlan::Probe(Some("COM6".into()))).contains("on COM6"));
        assert!(stopped_message(&StartPlan::Probe(None)).contains("each serial port"));
        assert_eq!(stopped_message(&StartPlan::Blocked("why".into())), "why");
    }

    /// The frontend reads `autoWarning`; a rename on either side would silently drop the warning.
    #[test]
    fn options_serialise_the_warning_for_the_frontend() {
        let json = serde_json::to_value(SerialPortOptions {
            ports: vec!["COM5".into()],
            auto: None,
            auto_warning: AutoPort::NoCart.problem(),
            ambiguous: false,
            everdrive_hint: EVERDRIVE_NEEDS_PORT_HINT,
        })
        .unwrap();
        assert!(json["auto"].is_null());
        assert!(json["autoWarning"].is_string());
        assert_eq!(json["ambiguous"], false);
        assert_eq!(json["everdriveHint"], EVERDRIVE_NEEDS_PORT_HINT);
    }

    /// Both EverDrive wordings come from one macro; this pins the text each place shows.
    #[test]
    fn everdrive_needs_port_wordings_are_unchanged() {
        assert_eq!(
            EVERDRIVE_NEEDS_PORT,
            "For a fixed Cart type, Auto-detect finds only a SummerCart64. Pick the EverDrive's \
             serial port in Settings, or set Cart to Auto-detect."
        );
        assert_eq!(
            EVERDRIVE_NEEDS_PORT_HINT,
            "For a fixed Cart type, Auto-detect finds only a SummerCart64. Pick the EverDrive's \
             serial port, or set Cart to Auto-detect."
        );
    }

    fn settings_for(serial_port: Option<&str>, cart: CartSetting) -> Settings {
        Settings {
            serial_port: serial_port.map(str::to_string),
            cart,
            ..Settings::default()
        }
    }

    /// The Note and the Settings hints share one enumeration per poll, and a poll that needs
    /// neither enumerates nothing.
    #[test]
    fn a_status_poll_enumerates_at_most_once() {
        let cases = [
            // (running, saved port, cart, with_ports, enumerations)
            (false, None, CartSetting::Auto, true, 1),
            (false, None, CartSetting::Auto, false, 1),
            (false, None, CartSetting::Sc64, true, 1),
            (false, Some("COM6"), CartSetting::Auto, true, 1),
            (false, Some("COM6"), CartSetting::Ed64, false, 0),
            (false, Some("COM6"), CartSetting::Ed64, true, 1),
            (true, None, CartSetting::Auto, false, 0),
            (true, None, CartSetting::Auto, true, 1),
        ];
        for (running, saved, cart, with_ports, want) in cases {
            let calls = std::cell::Cell::new(0);
            let settings = settings_for(saved, cart);
            let (message, options) = note_and_port_options(running, &settings, with_ports, || {
                calls.set(calls.get() + 1);
                vec![ch340("COM5"), sc64_windows("COM4")]
            });
            let case = (running, saved, cart, with_ports);
            assert_eq!(calls.get(), want, "{case:?}");
            assert_eq!(options.is_some(), with_ports, "{case:?}");
            if running {
                assert_eq!(message, "", "{case:?}");
            } else {
                let plan = plan_start(saved, cart, || vec![ch340("COM5"), sc64_windows("COM4")]);
                assert_eq!(message, stopped_message(&plan), "{case:?}");
            }
        }
    }

    /// The Note and the hints come from the same enumeration, so they agree about what is plugged in.
    #[test]
    fn a_status_poll_note_and_options_agree() {
        let settings = settings_for(None, CartSetting::Sc64);
        let (message, options) =
            note_and_port_options(false, &settings, true, || vec![ch340("COM5")]);
        let options = options.unwrap();
        assert_eq!(Some(message), options.auto_warning);
        assert_eq!(options.ports, ["COM5"]);
    }

    /// No cart and two carts both leave `auto` empty with a warning; only two carts is ambiguous,
    /// because only then does Start refuse rather than probe.
    #[test]
    fn options_flag_only_more_than_one_cart_as_ambiguous() {
        let two = options_from(vec![sc64_windows("COM8"), sc64_windows("COM4")]);
        assert!(two.ambiguous);
        assert!(two.auto.is_none() && two.auto_warning.is_some());

        let none = options_from(vec![ch340("COM5")]);
        assert!(!none.ambiguous);
        assert!(none.auto.is_none() && none.auto_warning.is_some());

        let one = options_from(vec![ch340("COM5"), sc64_windows("COM4")]);
        assert!(!one.ambiguous);
        assert_eq!(one.auto.as_deref(), Some("COM4"));
        assert!(one.auto_warning.is_none());
        assert_eq!(one.ports, ["COM5", "COM4"]);
    }
}

#[cfg(test)]
mod tray_tests {
    use super::*;

    const LISTEN: &str = "127.0.0.1:38765";

    #[test]
    fn status_names_the_port_when_running() {
        let l = tray_labels(true, Some("COM4"), true, None, LISTEN, false);
        assert_eq!(l.status, "Bridge: running on COM4");
        assert_eq!(l.toggle, "Stop bridge");
    }

    #[test]
    fn status_falls_back_to_listen_when_the_port_is_unknown() {
        // A live daemon with no configured or auto-detected port: name where it listens rather
        // than a port we cannot identify.
        let l = tray_labels(true, None, true, None, LISTEN, false);
        assert_eq!(l.status, "Bridge: running (127.0.0.1:38765)");
    }

    /// An experimental daemon is named in the tray, running or not; SC64 stays unadorned.
    #[test]
    fn the_cart_note_names_everything_but_a_running_sc64() {
        assert_eq!(
            tray_cart_note(true, DaemonCart::Sc64, CartSetting::Auto),
            None
        );
        assert_eq!(
            tray_cart_note(true, DaemonCart::Ed64Pro, CartSetting::Ed64Pro),
            Some("EverDrive-64 PRO (beta)")
        );
        // Chosen by Auto-detect or not, the note is the cart's name; the status line names the port.
        assert_eq!(
            tray_cart_note(true, DaemonCart::Ed64Pro, CartSetting::Auto),
            Some("EverDrive-64 PRO (beta)")
        );
        assert_eq!(
            tray_cart_note(false, DaemonCart::Sc64, CartSetting::Auto),
            Some("Auto-detect")
        );
        assert_eq!(
            tray_cart_note(false, DaemonCart::Ed64, CartSetting::Sc64),
            None
        );
        let l = tray_labels(
            true,
            Some("COM6"),
            true,
            Some("EverDrive-64 X7 (beta)"),
            LISTEN,
            false,
        );
        assert_eq!(l.status, "Bridge: running on COM6 · EverDrive-64 X7 (beta)");
        let l = tray_labels(
            false,
            None,
            false,
            Some("EverDrive-64 X7 (beta)"),
            LISTEN,
            false,
        );
        assert_eq!(
            l.status,
            "Bridge: stopped (no serial port) · EverDrive-64 X7 (beta)"
        );
    }

    #[test]
    fn stopped_shows_start_and_disables_restart() {
        let l = tray_labels(false, Some("COM4"), true, None, LISTEN, false);
        assert_eq!(l.status, "Bridge: stopped");
        assert_eq!(l.toggle, "Start bridge");
        assert!(l.toggle_enabled, "a port is configured, so Start is usable");
        assert!(
            !l.restart_enabled,
            "restarting a stopped daemon is just Start"
        );
    }

    #[test]
    fn start_is_disabled_when_it_cannot_work() {
        // `start_daemon` fails without a port or a probe, so the item must not invite the click.
        let l = tray_labels(false, None, false, None, LISTEN, false);
        assert!(!l.toggle_enabled);
        // ...and the status line says why it is greyed out.
        assert_eq!(l.status, "Bridge: stopped (no serial port)");
    }

    /// Start stayed enabled and unexplained while another process held the listen address, then
    /// failed. It stays enabled, as the window's does, and the status line says why it would fail.
    #[test]
    fn a_taken_listen_address_is_named_while_stopped() {
        let l = tray_labels(false, Some("COM4"), true, None, LISTEN, true);
        assert_eq!(l.status, "Bridge: stopped (listen address in use)");
        assert_eq!(l.toggle, "Start bridge");
        assert!(
            l.toggle_enabled,
            "the menu may be stale by the time it is clicked"
        );
        // It is what Start reports first, so it wins over a missing port.
        let l = tray_labels(false, None, false, Some("Auto-detect"), LISTEN, true);
        assert_eq!(
            l.status,
            "Bridge: stopped (listen address in use) · Auto-detect"
        );
        // A running bridge holds the address itself.
        let l = tray_labels(true, Some("COM4"), true, None, LISTEN, true);
        assert_eq!(l.status, "Bridge: running on COM4");
    }

    /// Auto-detect with no SC64 on USB has no port yet, but Start will probe for one.
    #[test]
    fn auto_detect_can_start_before_a_port_is_known() {
        let l = tray_labels(false, None, true, Some("Auto-detect"), LISTEN, false);
        assert!(l.toggle_enabled);
        assert_eq!(l.status, "Bridge: stopped · Auto-detect");
    }

    #[test]
    fn stop_stays_enabled_even_without_a_port() {
        // The port can disappear while the daemon runs; stopping it must still be possible.
        let l = tray_labels(true, None, false, None, LISTEN, false);
        assert_eq!(l.toggle, "Stop bridge");
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
        let l = tray_labels(true, serial.as_deref(), true, None, LISTEN, false);
        assert_eq!(l.status, "Bridge: running on COM4");
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
        let l = tray_labels(true, serial.as_deref(), true, None, LISTEN, false);
        assert_eq!(l.status, "Bridge: running (127.0.0.1:38765)");
    }

    #[cfg(feature = "xfer64")]
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

#[cfg(all(test, windows))]
mod job_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// #146: closing the job — which Windows does when Multi64's process ends, however it ends —
    /// must end the daemon too, so it cannot keep the serial port and the listen address.
    #[test]
    fn closing_the_job_ends_the_process_in_it() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ping");
        let job = job::KillOnCloseJob::new().expect("create job");
        job.assign(&child).expect("assign to job");
        assert!(
            child.try_wait().unwrap().is_none(),
            "runs while the job is open"
        );
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        let exited = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if exited.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(exited.is_some(), "the process outlived its job");
    }
}

#[cfg(test)]
mod frontend_tests {
    /// Every app that carries the shared base. `frontendDist` is per-app and no file can be
    /// shared across crates at runtime, so each holds its own copy and every copy must match.
    const SHARED_BASE_APPS: [&str; 3] = ["multi64", "xfer64", "multi64-test-app"];

    /// `styles.css` is the shared base of these apps' stylesheets: the palette, the size tokens and
    /// the components they all use (`docs/frontend-appearance.md` §5). Like `appearance.js` it is
    /// copied rather than shared, so this is what keeps them from drifting apart again.
    #[test]
    fn shared_styles_css_is_identical_in_both_apps() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crates/multi64/src-tauri -> repo root");
        let read = |app: &str| {
            let path = root.join(format!("crates/{app}/src/styles.css"));
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        };
        let base = read(SHARED_BASE_APPS[0]);
        for app in &SHARED_BASE_APPS[1..] {
            let other = read(app);
            if base != other {
                let first = base
                    .lines()
                    .zip(other.lines())
                    .position(|(a, b)| a != b)
                    .map_or_else(
                        || "same prefix, different lengths".to_string(),
                        |i| format!("first differing line: {}", i + 1),
                    );
                panic!(
                    "styles.css has drifted between {} and {app} ({first}).
                     Edit one and copy it to the others; every copy must stay byte-identical.",
                    SHARED_BASE_APPS[0]
                );
            }
        }
    }

    /// `appearance.js` is duplicated verbatim in every app carrying the shared base, because
    /// `frontendDist` is per-app and no file can be shared across crates at runtime. Nothing else
    /// enforces that, so a fix applied to one copy would silently leave the others stale — one app
    /// quietly ignoring a preference the rest honour. Documented in
    /// `docs/frontend-appearance.md`; checked here.
    #[test]
    fn appearance_js_is_identical_in_both_apps() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = here
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .expect("crates/multi64/src-tauri -> repo root");
        let read = |app: &str| {
            let path = root.join(format!("crates/{app}/src/appearance.js"));
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        };
        let a = read(SHARED_BASE_APPS[0]);
        for app in &SHARED_BASE_APPS[1..] {
            let b = read(app);
            if a != b {
                let first = a
                    .lines()
                    .zip(b.lines())
                    .position(|(x, y)| x != y)
                    .map_or_else(
                        || "same prefix, different lengths".to_string(),
                        |i| format!("first differing line: {}", i + 1),
                    );
                panic!(
                    "appearance.js has drifted between {} and {app} ({first}).
                     Edit one and copy it to the others; every copy must stay byte-identical.",
                    SHARED_BASE_APPS[0]
                );
            }
        }
    }

    /// Colour functions whose arguments may be literals rather than tokens.
    const COLOUR_FUNCTIONS: &[&str] = &[
        "rgb",
        "rgba",
        "hsl",
        "hsla",
        "hwb",
        "lab",
        "lch",
        "oklab",
        "oklch",
        "color",
        "color-mix",
    ];

    /// The CSS named colours. `transparent` and `currentColor` are absent on purpose: neither
    /// pins a hue, so neither breaks a theme.
    const NAMED_COLOURS: &str = "\
        aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue \
        blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk \
        crimson cyan darkblue darkcyan darkgoldenrod darkgray darkgreen darkgrey darkkhaki \
        darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon darkseagreen \
        darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue \
        dimgray dimgrey dodgerblue firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite \
        gold goldenrod gray green greenyellow grey honeydew hotpink indianred indigo ivory khaki \
        lavender lavenderblush lawngreen lemonchiffon lightblue lightcoral lightcyan \
        lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon lightseagreen \
        lightskyblue lightslategray lightslategrey lightsteelblue lightyellow lime limegreen linen \
        magenta maroon mediumaquamarine mediumblue mediumorchid mediumpurple mediumseagreen \
        mediumslateblue mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream \
        mistyrose moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid \
        palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink plum \
        powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon sandybrown \
        seagreen seashell sienna silver skyblue slateblue slategray slategrey snow springgreen \
        steelblue tan teal thistle tomato turquoise violet wheat white whitesmoke yellow \
        yellowgreen";

    /// Comments replaced by spaces, newlines kept, so line numbers still line up.
    fn strip_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut chars = css.chars().peekable();
        let mut in_comment = false;
        while let Some(c) = chars.next() {
            if in_comment {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("  ");
                    in_comment = false;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                }
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                out.push_str("  ");
                in_comment = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Quoted spans replaced by spaces: a font name or a `content:` string is not a declaration's
    /// colour, however it reads.
    fn strip_strings(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let mut quote: Option<char> = None;
        for c in value.chars() {
            match quote {
                Some(q) => {
                    out.push(' ');
                    if c == q {
                        quote = None;
                    }
                }
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    out.push(' ');
                }
                None => out.push(c),
            }
        }
        out
    }

    /// Every declaration in a stylesheet, as (line, enclosing selector, text). Enough of a CSS
    /// parser for these sheets: rules, at-rules with rules nested inside them, and strings that
    /// may hold a brace or a semicolon.
    fn declarations(css: &str) -> Vec<(usize, String, String)> {
        let source = strip_comments(css);
        let mut out = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        let mut buf = String::new();
        let mut line = 1usize;
        let mut buf_line = 1usize;
        let mut quote: Option<char> = None;
        for c in source.chars() {
            match quote {
                Some(q) => {
                    buf.push(c);
                    if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    '"' | '\'' => {
                        quote = Some(c);
                        buf.push(c);
                    }
                    '{' => {
                        stack.push(buf.split_whitespace().collect::<Vec<_>>().join(" "));
                        buf.clear();
                        buf_line = line;
                    }
                    '}' | ';' => {
                        let declaration = buf.trim().to_string();
                        if declaration.contains(':') {
                            if let Some(selector) = stack.last() {
                                out.push((buf_line, selector.clone(), declaration));
                            }
                        }
                        buf.clear();
                        if c == '}' {
                            stack.pop();
                        }
                        buf_line = line;
                    }
                    _ => {
                        if !c.is_whitespace() && buf.trim().is_empty() {
                            buf_line = line;
                        }
                        buf.push(c);
                    }
                },
            }
            if c == '\n' {
                line += 1;
            }
        }
        out
    }

    /// The palette blocks: `:root` and `:root[data-theme="…"]`, the only rules that may hold a
    /// colour. `:root[data-motion="reduced"] *` and the like are ordinary rules and are checked.
    fn is_palette_block(selector: &str) -> bool {
        selector == ":root" || (selector.starts_with(":root[") && selector.ends_with(']'))
    }

    /// The first colour literal in a declaration's value, if it has one.
    fn colour_literal(declaration: &str) -> Option<String> {
        let (_property, value) = declaration.split_once(':')?;
        let value: Vec<char> = strip_strings(value).chars().collect();
        let mut i = 0;
        while i < value.len() {
            let c = value[i];
            if c == '#' {
                let mut end = i + 1;
                while end < value.len() && value[end].is_ascii_hexdigit() {
                    end += 1;
                }
                let digits = end - i - 1;
                let ends_cleanly = match value.get(end) {
                    None => true,
                    Some(next) => !next.is_alphanumeric() && *next != '-' && *next != '_',
                };
                if matches!(digits, 3 | 4 | 6 | 8) && ends_cleanly {
                    return Some(value[i..end].iter().collect());
                }
                i = end;
                continue;
            }
            if c.is_ascii_alphabetic() {
                let mut end = i;
                while end < value.len() && (value[end].is_ascii_alphanumeric() || value[end] == '-')
                {
                    end += 1;
                }
                // Mid-identifier: part of `--accent-rgb` or `sans-serif`, not a word of its own.
                let continues_an_identifier = i > 0
                    && (value[i - 1] == '-'
                        || value[i - 1] == '_'
                        || value[i - 1].is_alphanumeric());
                let word: String = value[i..end].iter().collect();
                let lower = word.to_ascii_lowercase();
                if continues_an_identifier {
                    i = end;
                    continue;
                }
                if value.get(end) == Some(&'(') {
                    if COLOUR_FUNCTIONS.contains(&lower.as_str()) {
                        let mut depth = 0usize;
                        let mut close = end;
                        while close < value.len() {
                            match value[close] {
                                '(' => depth += 1,
                                ')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            close += 1;
                        }
                        let args: String = value[end..close.min(value.len())].iter().collect();
                        // A token anywhere in the arguments means the theme still reaches it.
                        if !args.contains("var(") {
                            return Some(format!("{word}{args})"));
                        }
                    }
                } else if NAMED_COLOURS.split_whitespace().any(|named| named == lower) {
                    return Some(word);
                }
                i = end;
                continue;
            }
            i += 1;
        }
        None
    }

    /// The palette rule of `docs/frontend-appearance.md` §1: outside the `:root` palette blocks,
    /// no rule in either app's stylesheets may name a colour. A literal is invisible to the theme
    /// switch, so it survives into Light and High contrast unchanged and usually becomes
    /// unreadable there — a failure a diff of the CSS does not show.
    ///
    /// Covers every app in [`SHARED_BASE_APPS`]. `crates/multi64-test-connector-gui` is out of
    /// scope: that window has one hardcoded palette and no theme switch, so it has nothing to
    /// break.
    #[test]
    fn no_colour_literals_outside_the_palette() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crates/multi64/src-tauri -> repo root");
        let mut sheets = Vec::new();
        for app in SHARED_BASE_APPS {
            let dir = root.join(format!("crates/{app}/src"));
            let entries =
                std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
            for entry in entries {
                let path = entry.expect("directory entry").path();
                if path.extension().and_then(|e| e.to_str()) == Some("css") {
                    sheets.push(path);
                }
            }
        }
        sheets.sort();
        assert!(
            sheets.len() >= 4,
            "expected both apps' stylesheets, found {sheets:?}"
        );

        let mut offences = Vec::new();
        for path in &sheets {
            let css = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            for (line, selector, declaration) in declarations(&css) {
                if is_palette_block(&selector) {
                    continue;
                }
                if let Some(literal) = colour_literal(&declaration) {
                    offences.push(format!(
                        "{}:{line}: `{literal}` in `{declaration}` (rule `{selector}`)",
                        path.display()
                    ));
                }
            }
        }
        assert!(
            offences.is_empty(),
            "colour literals outside the :root palette blocks. Use a token, or \
             rgba(var(--…-rgb), a) — docs/frontend-appearance.md §1:\n{}",
            offences.join("\n")
        );
    }
}
