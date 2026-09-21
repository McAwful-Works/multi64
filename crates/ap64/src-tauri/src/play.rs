//! The Play card: run a game's connector against the cart, through the Multi64 app's
//! daemon, for Archipelago's own client to connect to.
//!
//! There is no ROM file to choose. The connector reads the ROM from the cart (M64P
//! `PEEKROM`), and before starting, the session reads the cart's header and hook sites to
//! check it is running the chosen game with its agent in place.
//!
//! One session at a time, on its own thread (the connector's Lua state is not `Send`,
//! so everything is built there). The page hears about it through two events:
//! `play://status` with a [`Status`], and `play://log` with one line of text.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ap64_cart::backend::{Backend, Multi64, Stats};
use ap64_cart::Log;
use ap64_connector::server::{self, Event};
use ap64_connector::{Connector, Script, SCRIPTS};
use ap64_core::profile::{Transform, Write};
use ap64_core::transform::{dma_entries, rom_address, DmaEntry};
use ap64_core::{rom as rom_fmt, Bundle};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// The Multi64 app's daemon, where it listens by default.
pub const DEFAULT_URL: &str = "ws://127.0.0.1:38765/ws";

/// A failure worth showing, in two parts.
///
/// The first sentence is for whoever is holding the controller: what happened, and what to
/// do about it. The second is the addresses and values behind it -- the part that answers
/// "why do you say that", which is a question only someone working on AP64 asks, and which
/// turns a usable message into one a player cannot read past. The page shows the first
/// always and the second under Developer details.
#[derive(Debug, Clone, Default)]
pub struct Issue {
    pub message: String,
    pub technical: String,
}

impl Issue {
    /// A failure with nothing behind it worth separating out.
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            technical: String::new(),
        }
    }

    fn new(message: impl Into<String>, technical: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            technical: technical.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    /// For the log, where both halves belong on one line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if !self.technical.is_empty() {
            write!(f, " ({})", self.technical)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// idle | connecting | waiting-client | playing | stopped | failed
    pub state: String,
    pub detail: String,
    /// The part of `detail` only a developer needs; empty when there is none.
    pub detail_dev: String,
    /// The three things that have to be up, each as the session last saw it: `idle`, `waiting`,
    /// `ok` or `failed`.
    ///
    /// A single status line can only say what is happening now, which leaves the question a
    /// player actually has -- which part is not working -- to be inferred from it. These are
    /// answered separately and kept current: every request answered re-affirms the first two,
    /// and the client's own comings and goings drive the third.
    pub bridge: String,
    pub console: String,
    pub client: String,
    pub port: Option<u16>,
    pub requests: u64,
    pub reconnects: u32,
    pub stalls: u32,
    pub handled: u64,
}

pub struct Session {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// Lines kept for a log window opened later, or reopened. A session's worth of events
/// without holding a whole run's history.
const LOG_LINES: usize = 500;

#[derive(Default)]
pub struct Play {
    session: Mutex<Option<Session>>,
    status: Arc<Mutex<Status>>,
    /// Everything the session has logged, oldest first.
    ///
    /// The window that shows it can be closed and opened again, and need not exist when a line
    /// is written, so the lines live here rather than in whichever page happens to be up.
    log: Arc<Mutex<VecDeque<String>>>,
}

fn script(id: &str) -> Option<&'static Script> {
    SCRIPTS.iter().find(|s| s.id == id)
}

/// A game the Play card offers: which connector plays it, and which Archipelago client
/// goes with that.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub name: String,
    pub connector: String,
    pub client: String,
}

pub fn games(bundles: &[Bundle]) -> Vec<Game> {
    bundles
        .iter()
        .filter_map(|b| {
            let s = script(&b.profile.connector)?;
            Some(Game {
                id: b.profile.id.clone(),
                name: format!("{} ({})", b.profile.name, b.profile.release),
                connector: s.name.to_string(),
                client: s.client.to_string(),
            })
        })
        .collect()
}

/// A [`Multi64`] the session can still read counters from after the connector owns it.
struct SharedCart(Rc<RefCell<Multi64>>);

impl Backend for SharedCart {
    fn rdram_size(&self) -> u32 {
        self.0.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.0.borrow_mut().read_many(r)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.0.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        self.0.borrow().rom_window()
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.0.borrow_mut().read_rom_many(r)
    }
    fn generation(&self) -> u32 {
        self.0.borrow().generation()
    }
}

/// The file table in the cart's ROM at `table`, read a piece at a time up to its end.
fn cart_table(cart: &mut Multi64, table: u32) -> Result<Vec<DmaEntry>, String> {
    const STEP: usize = 0x1000;
    // dma_entries gives up past 0x2000 entries; so does this.
    const MAX: usize = 0x2_0000;
    let mut bytes = Vec::new();
    while bytes.len() < MAX {
        let got = cart
            .read_rom_many(&[(table + bytes.len() as u32, STEP)])
            .map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&got[0]);
        if let Ok(entries) = dma_entries(&bytes, 0) {
            return Ok(entries);
        }
    }
    Err(format!("the cart ROM has no file table at 0x{table:X}"))
}

/// Check the cart is running `bundle`'s game with its agent: the header names the game, and
/// every hook the profile writes is in place. Returns the ROM's internal name.
///
/// A profile's offsets are where the game sees its bytes. For a profile with a transform they
/// are not ROM offsets: AP64 stores a file it patched away from where the seed had it
/// (`transform::repack`), so each hook is looked up in the cart's own file table first.
fn verify_cart(bundle: &Bundle, cart: &mut Multi64) -> Result<String, Issue> {
    let p = &bundle.profile;
    let jals: Vec<(u32, u32)> = p
        .write
        .iter()
        .filter_map(|w| match w {
            Write::Jal { at, target, .. } => {
                Some((*at, 0x0C00_0000 | ((target >> 2) & 0x03FF_FFFF)))
            }
            _ => None,
        })
        .collect();
    // The header and boot code in one region: what the hash covers too, so it is cached.
    let got = cart
        .read_rom_many(&[(0u32, 0x1000usize)])
        .map_err(|e| Issue::plain(e.to_string()))?;
    let h = rom_fmt::header(&got[0])
        .ok_or_else(|| Issue::plain("the cart's ROM header could not be read"))?;
    if h.game_code != p.game_code || h.version != p.version {
        // Which ROM is on the cart and which was expected is a comparison of header fields, so
        // it is the developer's half. The player picked the game from the list and knows which
        // file they loaded; what they need is what to do about it.
        return Err(Issue::new(
            "the cart is running a different ROM; load the one you patched and start again",
            format!(
                "cart header {} {} v{}, profile expects {} {} v{}",
                if h.name.is_empty() {
                    "(no name)"
                } else {
                    &h.name
                },
                h.game_code,
                h.version,
                p.name,
                p.game_code,
                p.version
            ),
        ));
    }
    // One message for the player, whatever the reason: the reason is the developer's half.
    let without = |why: String| {
        Issue::new(
            "the ROM on the cart has no AP64 agent; add the agent to the seed and load that ROM",
            why,
        )
    };
    let table = match &p.transform {
        None => None,
        Some(Transform::Yaz0Dmadata { table }) => {
            Some(cart_table(cart, *table).map_err(Issue::plain)?)
        }
    };
    let mut at_rom = Vec::with_capacity(jals.len());
    for &(at, _) in &jals {
        at_rom.push(match &table {
            None => at,
            Some(t) => rom_address(t, at).ok_or_else(|| {
                without(format!(
                    "the file holding the hook at 0x{at:X} is still compressed"
                ))
            })?,
        });
    }
    let regions: Vec<(u32, usize)> = at_rom.iter().map(|&r| (r, 4)).collect();
    let words = cart
        .read_rom_many(&regions)
        .map_err(|e| Issue::plain(e.to_string()))?;
    for ((&(at, want), &rom), word) in jals.iter().zip(&at_rom).zip(&words) {
        let word = u32::from_be_bytes(word[..4].try_into().unwrap());
        if word != want {
            return Err(without(if rom == at {
                format!("the hook at ROM 0x{at:X} is 0x{word:08X}")
            } else {
                format!("the hook at 0x{at:X}, ROM 0x{rom:X}, is 0x{word:08X}")
            }));
        }
    }
    Ok(h.name)
}

impl Play {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Everything the session has logged so far, for a window opened after it started.
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    pub fn running(&self) -> bool {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !s.thread.is_finished())
    }

    pub fn start(
        &self,
        app: AppHandle,
        bundles: &[Bundle],
        game_id: String,
        url: String,
    ) -> Result<(), String> {
        if self.running() {
            return Err("already running; stop it first".into());
        }
        let game = bundles
            .iter()
            .find(|b| b.profile.id == game_id)
            .ok_or("choose a game")?
            .clone();
        let script = *script(&game.profile.connector).ok_or("unknown connector")?;
        // A new session starts a new log: what the last one did is not this one's history.
        let kept = self.log.clone();
        kept.lock().unwrap().clear();
        let url = if url.trim().is_empty() {
            DEFAULT_URL.to_string()
        } else {
            url.trim().to_string()
        };

        let stop = Arc::new(AtomicBool::new(false));
        let status = self.status.clone();
        *status.lock().unwrap() = Status {
            state: "connecting".into(),
            detail: format!("{} {} · {}", game.profile.name, game.profile.release, url),
            ..Status::default()
        };
        let _ = app.emit("play://status", status.lock().unwrap().clone());

        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("connector".into())
                .spawn(move || run(app, game, script, url, stop, status, kept))
                .map_err(|e| e.to_string())?
        };
        *self.session.lock().unwrap() = Some(Session { stop, thread });
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(s) = self.session.lock().unwrap().take() {
            s.stop.store(true, Ordering::Relaxed);
            // The loop checks the flag every few milliseconds; a cart mid-reconnect
            // can take a couple of seconds. Don't block the UI on it.
            std::thread::spawn(move || {
                let _ = s.thread.join();
            });
        }
    }
}

/// Marks for [`Status::bridge`], [`Status::console`] and [`Status::client`].
const IDLE: &str = "idle";
const WAITING: &str = "waiting";
const OK: &str = "ok";
const FAILED: &str = "failed";
/// Multi64 answering, but holding no serial link: its own state, because "looking for Multi64"
/// is wrong when Multi64 is right there.
const NO_CART: &str = "nocart";
/// The Multi64 app is running, but nothing answers on its bridge.
const NO_BRIDGE: &str = "nobridge";

/// The Multi64 app's process, which is what a player installed and started.
const MULTI64_PROCESS: &str = if cfg!(windows) {
    "multi64.exe"
} else {
    "multi64"
};

/// Where the Multi64 app is installed, if it can be found.
///
/// Each app has its own installer and both land beside each other under the same parent --
/// `…/AP64/ap64-app.exe` and `…/Multi64/multi64.exe` -- so the surest place to look is next to
/// this very executable, wherever that turned out to be. The rest are the installer's defaults,
/// and `MULTI64_APP` is for anyone running it from somewhere else entirely.
fn multi64_exe() -> Option<PathBuf> {
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Some(set) = std::env::var_os("MULTI64_APP") {
        tried.push(PathBuf::from(set));
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(parent) = me.parent().and_then(|d| d.parent()) {
            tried.push(parent.join("Multi64").join(MULTI64_PROCESS));
        }
    }
    for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            tried.push(PathBuf::from(dir).join("Multi64").join(MULTI64_PROCESS));
        }
    }
    tried.into_iter().find(|p| p.is_file())
}

/// Start the Multi64 app, so a session does not stall on something AP64 could do itself.
///
/// Best effort by design: it is started and left alone, and whether it comes up is answered by
/// the retry loop below like any other reason the bridge is not there yet. A failure here is a
/// log line, never the end of the session -- someone may be running Multi64 from a place this
/// cannot guess.
fn start_multi64(log: &Log) {
    let Some(exe) = multi64_exe() else {
        log("Multi64 is not running, and AP64 cannot find it to start it".into());
        return;
    };
    let mut cmd = Command::new(&exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window for a GUI app started from here.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.spawn() {
        Ok(_) => log(format!("started Multi64 ({})", exe.display())),
        Err(e) => log(format!("could not start Multi64 ({}): {e}", exe.display())),
    }
}

/// Whether the Multi64 app itself is running, whatever its bridge is doing.
///
/// Asked only when the bridge is silent, and only to tell two very different situations apart:
/// the app was never started, which is the player's to fix, and the app is up but its daemon is
/// not answering where AP64 is looking, which is not.
fn multi64_app_running() -> bool {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    // Whole name, not a prefix: the daemon beside it is `multi64d`, and counting that as the
    // app would report the one thing this exists to tell apart.
    sys.processes()
        .values()
        .any(|p| p.name().eq_ignore_ascii_case(MULTI64_PROCESS))
}

/// How often counters are pushed to the page while playing.
const STATS_EVERY: Duration = Duration::from_secs(1);
/// Between attempts to reach a cart that is not answering yet.
const CART_RETRY: Duration = Duration::from_secs(2);
/// How long a session keeps trying to reach a cart that stopped answering before it ends.
///
/// The backend's own default is minutes, which suits a tool waiting on a power cycle. A session
/// has an Archipelago client waiting on every request, and that client cannot tell a long wait
/// from a hang -- it can, however, notice a closed socket and reconnect by itself. So a console
/// that goes away ends the session promptly: the client recovers, and Start picks it up again
/// once the ROM is back. Long enough to ride out a stall or a daemon restarting.
const CART_GRACE: Duration = Duration::from_secs(8);

/// How long the cart must have gone unasked before the session asks it something itself. A
/// client that is polling proves the same thing for free, and a probe beside its requests would
/// only take a round trip from them.
const CART_QUIET: Duration = Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
fn run(
    app: AppHandle,
    game: Bundle,
    script: Script,
    url: String,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    kept_log: Arc<Mutex<VecDeque<String>>>,
) {
    let log: Log = {
        let app = app.clone();
        Arc::new(move |line: String| {
            {
                let mut kept = kept_log.lock().unwrap();
                if kept.len() == LOG_LINES {
                    kept.pop_front();
                }
                kept.push_back(line.clone());
            }
            // To every window: the log window listens for these, and may not be open.
            let _ = app.emit("play://log", line);
        })
    };
    let set = |f: &dyn Fn(&mut Status)| {
        let mut s = status.lock().unwrap();
        f(&mut s);
        let _ = app.emit("play://status", s.clone());
    };

    set(&|s| {
        s.bridge = WAITING.into();
        s.console = WAITING.into();
        s.client = WAITING.into();
    });

    // A session needs Multi64, so start it rather than wait for someone to notice.
    if !multi64_app_running() {
        start_multi64(&log);
    }

    // Wait for the cart: the console may not be on yet, or the daemon not started.
    let cart = loop {
        if stop.load(Ordering::Relaxed) {
            set(&|s| {
                s.state = "stopped".into();
                s.detail = String::new();
                s.detail_dev = String::new();
                s.bridge = IDLE.into();
                s.console = IDLE.into();
                s.client = IDLE.into();
            });
            return;
        }
        match Multi64::connect(&url, log.clone()) {
            Ok(c) => break c,
            Err(e) => {
                let msg = e.to_string();
                // A failed connect on its own says nothing about which end is at fault: the
                // HELLO it waits for needs the daemon, the serial link and an agent, and any
                // of the three being absent looks the same from here. The daemon answers a
                // plain HTTP GET whether or not a cart is attached, so ask it.
                let daemon = ap64_cart::transport::serial_active(&url);
                // Only when the bridge is silent is the app's own state worth the look.
                let app = daemon.is_none() && multi64_app_running();
                set(&|s| {
                    s.state = "connecting".into();
                    s.detail_dev = msg.clone();
                    match daemon {
                        None if app => {
                            s.detail = format!("Multi64 is running, but nothing answers at {url}");
                            s.bridge = NO_BRIDGE.into();
                            s.console = WAITING.into();
                        }
                        None => {
                            s.detail = "start the Multi64 app, with the cart plugged in".into();
                            s.bridge = WAITING.into();
                            s.console = WAITING.into();
                        }
                        Some(false) => {
                            s.detail = "Multi64 is running but has no cart connected".into();
                            s.bridge = NO_CART.into();
                            s.console = WAITING.into();
                        }
                        Some(true) => {
                            // Multi64 has the cart; it is the ROM that has not answered.
                            s.detail = "waiting for the ROM on the console".into();
                            s.bridge = OK.into();
                            s.console = WAITING.into();
                        }
                    }
                });
                let until = Instant::now() + CART_RETRY;
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    };
    // The daemon answered and an agent is on the other end of it.
    set(&|s| s.bridge = OK.into());
    log(format!(
        "cart agent answered: {} MiB RDRAM, {}",
        cart.rdram_size() >> 20,
        if cart.writable() {
            "writable"
        } else {
            "read-only"
        }
    ));

    let mut cart = cart;
    match verify_cart(&game, &mut cart) {
        Ok(name) => {
            log(format!(
                "the cart is running {} ({}) with the agent",
                game.profile.name,
                if name.is_empty() {
                    "no internal name"
                } else {
                    &name
                }
            ));
            set(&|s| s.console = OK.into());
        }
        Err(e) => {
            log(e.to_string());
            set(&|s| {
                s.state = "failed".into();
                s.detail = e.message.clone();
                s.detail_dev = e.technical.clone();
                s.console = FAILED.into();
            });
            return;
        }
    }

    // What the session believes about the console and the client, so a cart that goes quiet and
    // comes back is reported both ways round rather than only on the way down. `client_here` is
    // shared with the health callback below, which runs on this same thread from inside a cart
    // call and has to word the status the way the event loop would.
    let console_up = Rc::new(Cell::new(true));
    let client_here = Rc::new(Cell::new(false));
    let mut last_answer = Instant::now();

    // What the backend says about the cart, while it says it.
    //
    // A request that fails is retried and then reconnected for as long as the backend's own
    // deadline, and nothing comes back to this loop in the meantime -- so without this, a
    // console that was reset went on being reported as running until that deadline expired.
    // The client notices immediately, because its own reads are what fail; this is how AP64
    // learns it at the same time.
    {
        let stop = stop.clone();
        cart.set_cancelled(Rc::new(move || stop.load(Ordering::Relaxed)));
    }
    cart.set_reconnect_deadline(CART_GRACE);
    {
        let status = status.clone();
        let app = app.clone();
        let client_here = client_here.clone();
        let console_up = console_up.clone();
        let client = script.client;
        let log = log.clone();
        cart.set_health(Rc::new(move |answering: bool| {
            if console_up.replace(answering) == answering {
                return;
            }
            log(if answering {
                "the cart is answering again".into()
            } else {
                "the cart stopped answering".into()
            });
            let mut s = status.lock().unwrap();
            s.console = if answering { OK } else { FAILED }.into();
            if answering {
                s.state = if client_here.get() {
                    "playing"
                } else {
                    "waiting-client"
                }
                .into();
                s.detail = if client_here.get() {
                    format!("{client} connected")
                } else {
                    format!("open {client} from the Archipelago Launcher")
                };
            } else {
                s.state = "waiting-console".into();
                s.detail = "the ROM stopped answering; load it again on the console".into();
            }
            s.detail_dev = String::new();
            let _ = app.emit("play://status", s.clone());
        }));
    }

    let cart = Rc::new(RefCell::new(cart));
    let connector = match Connector::new(&script, Box::new(SharedCart(cart.clone())), log.clone()) {
        Ok(c) => c,
        Err(e) => {
            set(&|s| {
                s.state = "failed".into();
                s.detail = e.clone();
                s.detail_dev = String::new();
            });
            return;
        }
    };

    let ports = script.ports;
    let mut last_stats = Instant::now();
    let mut handled = 0u64;
    // A check the game showed only between two polls, handed to the client by the watch
    // rather than waiting for the scene to change. Worth a line: it is the only place a
    // session says the queue is doing anything.
    let mut last_watch = ap64_cart::watch::WatchStats::default();
    let push_stats = |stats: Stats, handled: u64| {
        set(&|s| {
            s.requests = stats.requests;
            s.reconnects = stats.reconnects;
            s.stalls = stats.stalls;
            s.handled = handled;
        })
    };
    let result = server::serve(&connector, ports, &stop, &mut |e| match e {
        Event::Listening(port) => {
            log(format!(
                "listening on localhost:{port}; open {} from the Archipelago Launcher",
                script.client
            ));
            set(&|s| {
                s.state = "waiting-client".into();
                s.port = Some(port);
                s.detail = format!("open {} from the Archipelago Launcher", script.client);
                s.detail_dev = format!("listening on 127.0.0.1:{port}");
                s.client = WAITING.into();
            });
        }
        Event::ClientConnected(addr) => {
            client_here.set(true);
            log(format!("{} connected from {addr}", script.client));
            set(&|s| {
                s.state = "playing".into();
                s.detail = format!("{} connected", script.client);
                s.detail_dev = format!("from {addr}");
                s.client = OK.into();
            });
        }
        Event::ClientDisconnected(why) => {
            client_here.set(false);
            log(format!("{} disconnected: {why}", script.client));
            set(&|s| {
                s.state = "waiting-client".into();
                s.detail = format!("{} disconnected; waiting for it", script.client);
                s.detail_dev = why.clone();
                s.client = WAITING.into();
            });
        }
        // Nothing is being asked of the cart, so ask it something: a console that was reset
        // or a ROM that was swapped answers nothing, and a session with no client attached would
        // otherwise go on reporting whatever was true when the last request was answered.
        Event::Idle => {
            // A client that is polling proves the cart for free; this is for the quiet spells,
            // including a session with no client attached at all. `alive` reports through the
            // same health callback, so the wording lives in one place.
            if last_answer.elapsed() >= CART_QUIET {
                let _ = cart.borrow_mut().alive();
            }
        }
        Event::Handled => {
            handled += 1;
            last_answer = Instant::now();
            // Deliberately not touched here: an answered line says the client asked something
            // and the connector replied, which a script that catches its own errors will do
            // with a cart that is not there at all. What the cart is doing is the backend's to
            // report (set_health above) and the probe below's to check.
            if let Some(w) = connector.watch_stats() {
                if w.replayed > last_watch.replayed || w.dropped > last_watch.dropped {
                    log(format!(
                        "in-scene events: {} seen, {} given to the client{}",
                        w.events,
                        w.replayed,
                        if w.dropped > 0 {
                            format!(", {} dropped by a full queue", w.dropped)
                        } else {
                            String::new()
                        }
                    ));
                }
                last_watch = w;
            }
            if last_stats.elapsed() >= STATS_EVERY {
                last_stats = Instant::now();
                push_stats(cart.borrow().stats(), handled);
            }
        }
    });
    push_stats(cart.borrow().stats(), handled);
    match result {
        Ok(()) => {
            log("stopped".into());
            set(&|s| {
                s.state = "stopped".into();
                s.detail = String::new();
                s.bridge = IDLE.into();
                s.console = IDLE.into();
                s.client = IDLE.into();
            });
        }
        Err(e) => {
            log(format!("session ended: {e}"));
            set(&|s| {
                s.state = "failed".into();
                // Which link gave way is on its own row; this says what to do about it. The
                // error itself names addresses and URLs, which is the developer's half.
                s.detail = "the cart stopped answering; load the ROM again and press Start".into();
                s.detail_dev = e.clone();
                s.console = FAILED.into();
                s.client = IDLE.into();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MULTI64_PROCESS` is compared against what sysinfo reports, which is the executable's
    /// file name -- with its extension on Windows. Checked against this very process, so the
    /// form is pinned without needing Multi64 installed.
    #[test]
    fn the_name_looked_for_is_the_form_the_os_reports() {
        use sysinfo::{ProcessRefreshKind, RefreshKind, System};
        let sys = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        let me = sysinfo::get_current_pid().and_then(|pid| {
            sys.process(pid)
                .map(|p| p.name().to_string_lossy().to_string())
                .ok_or("no such process")
        });
        let me = me.expect("this process is running");
        let exe = std::env::current_exe().expect("this process has a path");
        let file_name = exe.file_name().unwrap().to_string_lossy().to_string();
        // A prefix, not the whole string: Linux reports `comm`, which the kernel caps at 15
        // characters, and a test binary's name (`ap64_app_lib-<hash>`) is longer than that.
        // Windows reports the whole file name, extension and all, which is what pins the form
        // the constant is written in.
        assert!(
            file_name
                .to_ascii_lowercase()
                .starts_with(&me.to_ascii_lowercase()),
            "sysinfo reports {me:?}, the executable is {file_name:?}"
        );
        assert_eq!(
            MULTI64_PROCESS.contains('.'),
            file_name.contains('.'),
            "the constant must be in the same form: {MULTI64_PROCESS:?} against {file_name:?}"
        );
    }

    /// `MULTI64_APP` is process-wide and these tests run in one process, in parallel: without
    /// this, one clears the variable while the other is reading it. Seen as a Windows job
    /// failing while Linux passed, which is how a race presents itself.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Someone running Multi64 from somewhere this cannot guess sets `MULTI64_APP`.
    #[test]
    fn the_override_is_used_when_it_names_a_file() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let exe = std::env::current_exe().expect("this process has a path");
        std::env::set_var("MULTI64_APP", &exe);
        let found = multi64_exe();
        std::env::remove_var("MULTI64_APP");
        assert_eq!(found.as_deref(), Some(exe.as_path()));
    }

    /// An override pointing at nothing is ignored rather than taken as the answer.
    #[test]
    fn an_override_that_is_not_there_is_not_used() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let missing = std::env::temp_dir().join("ap64-no-such-multi64.exe");
        std::env::set_var("MULTI64_APP", &missing);
        let found = multi64_exe();
        std::env::remove_var("MULTI64_APP");
        assert_ne!(found.as_deref(), Some(missing.as_path()));
    }
}
