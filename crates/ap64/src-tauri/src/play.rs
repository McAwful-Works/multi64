//! The Play card: run a game's connector against the cart, through the Multi64 app's
//! daemon, for Archipelago's own client to connect to.
//!
//! There is no ROM file to choose. The connector reads the ROM from the cart (M64P
//! `PEEKROM`), and before starting, the session reads the cart's header and hook sites (or,
//! for a hook the profile finds per seed, the agent image) to check it is running the chosen
//! game with its agent in place.
//!
//! One session at a time, on its own thread (the connector's Lua state is not `Send`,
//! so everything is built there). The page hears about it through two events:
//! `play://status` with a [`Status`], and `play://log` with one line of text.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ap64_cart::backend::{Backend, Multi64, Stats};
use ap64_cart::Log;
use ap64_connector::server::{self, Event};
use ap64_connector::{retroarch, Connector, Native, Script, NATIVES, SCRIPTS};
use ap64_core::profile::{Addr, Transform, Write};
use ap64_core::transform::{dma_entries, rom_address, DmaEntry};
use ap64_core::{rom as rom_fmt, Bundle};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

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
    /// Whether a session is up, which is the only thing Start and Stop follow.
    ///
    /// Not inferred from `state`: a session that is waiting for a console to come back is
    /// as running as one mid-game, and reading that off the wording meant a reset console
    /// silently handed Start back to someone who had not asked to stop.
    pub running: bool,
    /// idle | connecting | waiting-client | playing | stopped
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
    /// Where the current (or last) session's log is saved, if it could be.
    log_file: Mutex<Option<PathBuf>>,
}

/// What plays a game: a connector script AP64 runs, or a client AP64 answers itself.
#[derive(Clone, Copy)]
enum Kind {
    Script(Script),
    Native(Native),
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Script(s) => s.name,
            Kind::Native(n) => n.name,
        }
    }

    fn client(&self) -> &'static str {
        match self {
            Kind::Script(s) => s.client,
            Kind::Native(n) => n.client,
        }
    }
}

fn kind(id: &str) -> Option<Kind> {
    SCRIPTS
        .iter()
        .find(|s| s.id == id)
        .map(|s| Kind::Script(*s))
        .or_else(|| {
            NATIVES
                .iter()
                .find(|n| n.id == id)
                .map(|n| Kind::Native(*n))
        })
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
            let k = kind(&b.profile.connector)?;
            Some(Game {
                id: b.profile.id.clone(),
                // The name alone: Archipelago patched the seed, so the release was
                // settled long before AP64 saw it, and there is nothing here to pick
                // between. It stays on the profile for the header check to fail on.
                name: b.profile.name.clone(),
                connector: k.name().to_string(),
                client: k.client().to_string(),
            })
        })
        .collect()
}

/// Whether the chosen game's Archipelago client is ready for AP64. The Play card asks when Start
/// is pressed, and offers the fix before starting when one is needed.
///
/// Only a game whose client needs changing before it can reach AP64 has one of these at all
/// ([`ap64_connector::ClientFix`]); for every other game the answer is `None` and Start just
/// starts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetup {
    /// `ready`, `needed`, `missing` (the client is not installed) or `unknown`.
    pub state: String,
    pub message: String,
    /// The file the fix changes, when it was found.
    pub path: String,
    /// Why, for Developer details.
    pub technical: String,
}

fn client_fix(bundles: &[Bundle], game_id: &str) -> Option<(Native, ap64_connector::ClientFix)> {
    let b = bundles.iter().find(|b| b.profile.id == game_id)?;
    match kind(&b.profile.connector)? {
        Kind::Native(n) => n.client_fix.map(|f| (n, f)),
        Kind::Script(_) => None,
    }
}

pub fn client_setup(bundles: &[Bundle], game_id: &str) -> Option<ClientSetup> {
    use ap64_connector::ClientState;
    let (native, fix) = client_fix(bundles, game_id)?;
    let client = native.client;
    let setup = |state: &str, message: String, path: String, technical: String| ClientSetup {
        state: state.into(),
        message,
        path,
        technical,
    };
    let Some(path) = (fix.find)() else {
        return Some(setup(
            "missing",
            format!(
                "Install this game's world in Archipelago ({}) before playing",
                fix.file
            ),
            String::new(),
            "looked in custom_worlds and lib/worlds under %ProgramData%\\Archipelago and \
             %LOCALAPPDATA%\\Archipelago, and AP64_ARCHIPELAGO_DIR if set"
                .into(),
        ));
    };
    let shown = path.display().to_string();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return Some(setup(
                "unknown",
                format!("{} could not be read", fix.file),
                shown,
                e.to_string(),
            ))
        }
    };
    Some(match (fix.state)(&bytes) {
        ClientState::Ready => setup(
            "ready",
            format!("{client} is ready for AP64"),
            shown,
            String::new(),
        ),
        ClientState::NeedsFix => setup(
            "needed",
            format!(
                "{}. AP64 will add it to {} and keep the original",
                fix.why, fix.file
            ),
            shown,
            String::new(),
        ),
        ClientState::Unrecognized(why) => setup(
            "unknown",
            format!(
                "This version of {} is not one AP64 knows, so {client} may not connect",
                fix.file
            ),
            shown,
            why,
        ),
    })
}

/// Make the chosen game's client fix, keeping the original in `backups`. Returns what to
/// tell the player.
pub fn fix_client(
    bundles: &[Bundle],
    game_id: &str,
    backups: &std::path::Path,
) -> Result<String, String> {
    let (native, fix) = client_fix(bundles, game_id).ok_or("this game's client needs no fix")?;
    let path =
        (fix.find)().ok_or_else(|| format!("{} is not installed in Archipelago", fix.file))?;
    let original = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let fixed = (fix.apply)(&original)?;
    std::fs::create_dir_all(backups).map_err(|e| format!("making {}: {e}", backups.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = backups.join(format!("{}.{stamp}.original", fix.file));
    std::fs::write(&backup, &original).map_err(|e| format!("saving the original: {e}"))?;
    // Written beside it and moved over it, so a failure part-way leaves the original in place.
    let staged = path.with_extension("apworld.ap64-new");
    std::fs::write(&staged, &fixed).map_err(|e| format!("writing {}: {e}", staged.display()))?;
    if let Err(e) = std::fs::rename(&staged, &path) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!(
            "{} is in use; close {} and the Archipelago Launcher, then try again ({e})",
            fix.file, native.client
        ));
    }
    Ok(format!(
        "Fixed. Restart the Archipelago Launcher before opening {}. The original is saved as {}",
        native.client,
        backup.display()
    ))
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
///
/// A hook placed relative to a find has no fixed offset to read, and locating it would mean
/// reading the whole ROM back. The agent image is checked at its offset in its place.
fn verify_cart(bundle: &Bundle, cart: &mut Multi64) -> Result<OnCart, Issue> {
    let p = &bundle.profile;
    let mut found_hooks = false;
    let jals: Vec<(u32, u32)> = p
        .write
        .iter()
        .filter_map(|w| match w {
            Write::Jal {
                at: Addr::Rom(at),
                target,
                ..
            } => Some((*at, 0x0C00_0000 | ((target >> 2) & 0x03FF_FFFF))),
            Write::Jal { .. } => {
                found_hooks = true;
                None
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
    // An agent that can move is wherever the stub on the cart was told it is.
    let agent_rom = match p.agent_rom_imm() {
        Some((Addr::Rom(hi), Some(Addr::Rom(lo)))) if table.is_none() => {
            let got = cart
                .read_rom_many(&[(*hi, 4), (*lo, 4)])
                .map_err(|e| Issue::plain(e.to_string()))?;
            let word = |b: &[u8]| u32::from_be_bytes(b[..4].try_into().unwrap());
            Some(ap64_core::profile::imm_value(word(&got[0]), word(&got[1])))
        }
        _ => None,
    };
    if found_hooks {
        let image = &bundle.blobs[&p.agent.image];
        let at = match &table {
            None => agent_rom.unwrap_or(p.agent.rom),
            Some(t) => rom_address(t, p.agent.rom).ok_or_else(|| {
                without(format!(
                    "the file holding the agent at 0x{:X} is still compressed",
                    p.agent.rom
                ))
            })?,
        };
        let got = cart
            .read_rom_many(&[(at, image.len())])
            .map_err(|e| Issue::plain(e.to_string()))?;
        if got[0] != *image {
            return Err(without(format!(
                "the agent at ROM 0x{at:X} is not this build's"
            )));
        }
    }
    Ok(OnCart {
        name: h.name,
        agent_rom,
    })
}

/// What [`verify_cart`] learned about the cart, for the session log.
struct OnCart {
    /// The ROM's internal name.
    name: String,
    /// Where the agent is in the cart's ROM, read from the stub, for a profile whose agent can
    /// move. `None` for one whose agent the game's own loader puts in place.
    agent_rom: Option<u32>,
}

/// Session logs kept on disk, newest first; older ones are deleted when a session starts.
const LOG_FILES: usize = 20;

/// Open this session's log file under `dir`, deleting all but the newest [`LOG_FILES`] - 1
/// already there, so this one makes [`LOG_FILES`]. Named by the local time it started, so the
/// names sort by age.
fn open_log_file(dir: &Path) -> io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".txt"))
        })
        .collect();
    old.sort();
    let excess = old.len().saturating_sub(LOG_FILES - 1);
    for p in &old[..excess] {
        let _ = std::fs::remove_file(p);
    }
    let path = dir.join(format!(
        "session-{}.txt",
        chrono::Local::now().format("%Y-%m-%d-%H%M%S")
    ));
    let file = File::create(&path)?;
    Ok((path, file))
}

impl Play {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Everything the session has logged so far, for a window opened after it started.
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    /// The file the current (or last) session's log is saved to, if it could be.
    pub fn log_file(&self) -> Option<PathBuf> {
        self.log_file.lock().unwrap().clone()
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
        let kind = kind(&game.profile.connector).ok_or("unknown connector")?;
        // A new session starts a new log: what the last one did is not this one's history.
        let kept = self.log.clone();
        kept.lock().unwrap().clear();
        let file = app
            .path()
            .app_data_dir()
            .map_err(|e| io::Error::other(e.to_string()))
            .and_then(|dir| open_log_file(&dir.join("logs")))
            .map_err(|e| e.to_string());
        *self.log_file.lock().unwrap() = file.as_ref().ok().map(|(p, _)| p.clone());
        let url = if url.trim().is_empty() {
            DEFAULT_URL.to_string()
        } else {
            url.trim().to_string()
        };

        let stop = Arc::new(AtomicBool::new(false));
        let status = self.status.clone();
        *status.lock().unwrap() = Status {
            running: true,
            state: "connecting".into(),
            detail: game.profile.name.clone(),
            detail_dev: url.clone(),
            ..Status::default()
        };
        let _ = app.emit("play://status", status.lock().unwrap().clone());

        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("connector".into())
                .spawn(move || run(app, game, kind, url, stop, status, kept, file))
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

/// How long a silent agent is waited out before its link is rebuilt, in a session whose
/// client AP64 answers itself ([`Multi64::set_silence_budget`]).
const NATIVE_SILENCE: Duration = Duration::from_secs(10);

/// Between the session log's summary lines.
const SUMMARY_EVERY: Duration = Duration::from_secs(300);

#[allow(clippy::too_many_arguments)]
fn run(
    app: AppHandle,
    game: Bundle,
    kind: Kind,
    url: String,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    kept_log: Arc<Mutex<VecDeque<String>>>,
    file: Result<(PathBuf, File), String>,
) {
    let (file_path, file) = match file {
        Ok((path, file)) => (Ok(path), Some(file)),
        Err(e) => (Err(e), None),
    };
    let log: Log = {
        let app = app.clone();
        let file = Mutex::new(file);
        Arc::new(move |line: String| {
            // Local time, to the second, to line up against the client's log file in
            // Archipelago's `logs` folder, which stamps every line in local time.
            let line = format!("{} {line}", chrono::Local::now().format("%H:%M:%S"));
            // Written as it happens, so a crash or a closed window loses nothing. A write that
            // fails stops the file there; the window keeps the whole log regardless.
            {
                let mut f = file.lock().unwrap();
                if f.as_mut().is_some_and(|f| writeln!(f, "{line}").is_err()) {
                    *f = None;
                }
            }
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

    // What anyone reading this log later needs first: which build, which game, which bridge,
    // and where the log is kept.
    log(format!(
        "AP64 {} ({}), {} ({} profile), through Multi64 at {url}",
        env!("CARGO_PKG_VERSION"),
        env!("AP64_COMMIT"),
        game.profile.name,
        game.profile.id
    ));
    log(match &file_path {
        Ok(path) => format!("this log is also saved to {}", path.display()),
        Err(e) => format!("this log is not being saved to a file: {e}"),
    });

    // A session needs Multi64, so start it rather than wait for someone to notice.
    if !multi64_app_running() {
        start_multi64(&log);
    }

    // Requests answered across the whole session, not just this link: a console that was
    // reset should not make the count start again.
    let mut handled = 0u64;

    // Start means "keep at it until I say stop". A console reset, a ROM swapped, a daemon
    // restarted, the wrong game loaded -- none of those end a session, they put it back to
    // waiting for the cart, because every one of them is something the person playing is
    // about to undo. Deciding for them that a session was over meant a reset console handed
    // Start back to someone who had not asked to stop, and took Stop away from them.
    // Between attempts, and interruptible so Stop is still felt inside one.
    //
    // Every failure below comes back to the top of this loop, and two of them -- the wrong
    // game on the console, a script that will not load against it -- leave a cart that
    // answers straight away. Without a pause those would spin on the cart as fast as USB
    // allows and bury the log, so the wait belongs here, once, rather than at each failure.
    let pause = || {
        let until = Instant::now() + CART_RETRY;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let mut first_try = true;
    'session: while !stop.load(Ordering::Relaxed) {
        if !std::mem::take(&mut first_try) {
            pause();
        }
        // Wait for the cart: the console may not be on yet, or the daemon not started.
        let cart = loop {
            if stop.load(Ordering::Relaxed) {
                break 'session;
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
                                s.detail =
                                    format!("Multi64 is running, but nothing answers at {url}");
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
                                // Say so rather than "starting": this is also where a session waits
                                // after a console was reset, and it is not starting then.
                                s.state = "waiting-console".into();
                                s.detail = "waiting for the ROM on the console".into();
                                s.bridge = OK.into();
                                s.console = WAITING.into();
                            }
                        }
                    });
                    pause();
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
            Ok(on) => {
                log(format!(
                    "the cart is running {} ({}) with the agent",
                    game.profile.name,
                    if on.name.is_empty() {
                        "no internal name"
                    } else {
                        &on.name
                    }
                ));
                let agent = &game.profile.agent;
                log(match on.agent_rom {
                    Some(at) if at != agent.rom => format!(
                        "the agent is at ROM 0x{at:X}, past the seed's data (its usual place is \
                         0x{:X}), and runs at 0x{:08X}",
                        agent.rom, agent.vram
                    ),
                    Some(at) => format!(
                        "the agent is at ROM 0x{at:X} and runs at 0x{:08X}",
                        agent.vram
                    ),
                    None => format!(
                        "the agent runs at 0x{:08X}, put there by the game's own loader",
                        agent.vram
                    ),
                });
                set(&|s| s.console = OK.into());
            }
            Err(e) => {
                log(e.to_string());
                set(&|s| {
                    s.state = "waiting-console".into();
                    s.detail = e.message.clone();
                    s.detail_dev = e.technical.clone();
                    s.console = FAILED.into();
                });
                continue 'session;
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
        // A client AP64 answers itself never waits on the cart, so a long scene load can be
        // waited out rather than costing a reconnect. DK64's loads silence the agent for up to
        // 3 s, against the 3.6 s the default allows.
        if let Kind::Native(_) = kind {
            cart.set_silence_budget(NATIVE_SILENCE);
        }
        {
            let status = status.clone();
            let app = app.clone();
            let client_here = client_here.clone();
            let console_up = console_up.clone();
            let client = kind.client();
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
        // A native connector has no script to load: AP64 answers its client itself.
        let connector = match kind {
            Kind::Native(_) => None,
            Kind::Script(script) => {
                match Connector::new(&script, Box::new(SharedCart(cart.clone())), log.clone()) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        log(format!("the connector script would not load: {e}"));
                        set(&|s| {
                            s.state = "waiting-console".into();
                            s.detail =
                                "the connector could not read the game; check the ROM on the console"
                                    .into();
                            s.detail_dev = e.clone();
                            s.console = FAILED.into();
                        });
                        continue 'session;
                    }
                }
            }
        };

        let client = kind.client();
        // Where the client finds AP64, for the log: scripts are TCP, native connectors UDP.
        let transport = match kind {
            Kind::Script(_) => "localhost",
            Kind::Native(_) => "UDP localhost",
        };
        let mut last_stats = Instant::now();
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
        // A line every few minutes saying what this link has been doing, so a quiet stretch of
        // log reads as quiet rather than as nothing being written.
        let mut misses = 0u64;
        let mut last_summary = Instant::now();
        let mut summarized = (handled, 0u64, Stats::default());
        let mut on_event = |e| {
            match e {
                Event::Listening(port) => {
                    log(format!(
                    "listening on {transport}:{port}; open {client} from the Archipelago Launcher"
                ));
                    set(&|s| {
                        s.state = "waiting-client".into();
                        s.port = Some(port);
                        s.detail = format!("open {client} from the Archipelago Launcher");
                        s.detail_dev = format!("listening on 127.0.0.1:{port}");
                        s.client = WAITING.into();
                    });
                }
                Event::ClientConnected(addr) => {
                    client_here.set(true);
                    log(format!("{client} connected from {addr}"));
                    set(&|s| {
                        s.state = "playing".into();
                        s.detail = format!("{client} connected");
                        s.detail_dev = format!("from {addr}");
                        s.client = OK.into();
                    });
                }
                Event::ClientDisconnected(why) => {
                    client_here.set(false);
                    log(format!("{client} disconnected: {why}"));
                    set(&|s| {
                        s.state = "waiting-client".into();
                        s.detail = format!("{client} disconnected; waiting for it");
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
                    if let Some(w) = connector.as_ref().and_then(|c| c.watch_stats()) {
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
                Event::Missed(line) => {
                    misses += 1;
                    log(line);
                }
                Event::Note(line) => log(line),
            }
            if last_summary.elapsed() >= SUMMARY_EVERY {
                let stats = cart.borrow().stats();
                let (h, m, st) = summarized;
                log(format!(
                    "in the last {} min: {} requests answered, {} with an error for want of the \
                     cart; {} stalls, {} reconnects; {client} {}",
                    SUMMARY_EVERY.as_secs() / 60,
                    handled - h,
                    misses - m,
                    stats.stalls - st.stalls,
                    stats.reconnects - st.reconnects,
                    if client_here.get() {
                        "connected"
                    } else {
                        "not connected"
                    }
                ));
                last_summary = Instant::now();
                summarized = (handled, misses, stats);
            }
        };
        let result = match (kind, &connector) {
            (Kind::Script(script), Some(connector)) => {
                server::serve(connector, script.ports, &stop, &mut on_event)
            }
            // The cart stays on this thread, and each call borrows it only for its length, so
            // the handler's own borrows above never overlap one of these.
            (Kind::Native(native), _) => retroarch::serve(
                &mut SharedCart(cart.clone()),
                &native.options,
                &stop,
                &mut on_event,
            ),
            (Kind::Script(_), None) => unreachable!("a script session loads its script first"),
        };
        // Whatever stalls were being counted belong before what happens next.
        cart.borrow_mut().flush_stalls(true);
        push_stats(cart.borrow().stats(), handled);
        match result {
            // `serve` only returns cleanly when the stop flag it was given is set.
            Ok(()) => break 'session,
            Err(e) => {
                log(format!("the link gave way: {e}; still trying"));
                set(&|s| {
                    s.state = "waiting-console".into();
                    // Which link gave way is on its own row; this says what to do about it, and
                    // does not ask for Start, because the session is still here waiting. The
                    // error itself names addresses and URLs, which is the developer's half.
                    s.detail = "the ROM stopped answering; load it again on the console".into();
                    s.detail_dev = e.clone();
                    s.console = FAILED.into();
                    s.client = IDLE.into();
                });
            }
        }
    }

    log("stopped".into());
    set(&|s| {
        s.running = false;
        s.state = "stopped".into();
        s.detail = String::new();
        s.detail_dev = String::new();
        s.bridge = IDLE.into();
        s.console = IDLE.into();
        s.client = IDLE.into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile whose connector names nothing is silently left off the Play card, so every
    /// built-in game is checked for one, and a script and a native connector never share an id.
    #[test]
    fn every_game_has_a_connector_and_ids_are_unambiguous() {
        let bundles = ap64_core::builtin().unwrap();
        assert_eq!(games(&bundles).len(), bundles.len());
        for s in SCRIPTS {
            assert!(!NATIVES.iter().any(|n| n.id == s.id), "{}", s.id);
        }
        let bt = bundles.iter().find(|b| b.profile.id == "bt").unwrap();
        assert!(matches!(kind(&bt.profile.connector), Some(Kind::Native(_))));
    }

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

    /// Starting a session keeps the newest logs and makes the new one the last of them; anything
    /// else in the folder is not the pruning's to touch.
    #[test]
    fn a_session_log_keeps_the_newest_and_nothing_else_is_touched() {
        let dir = std::env::temp_dir().join(format!("ap64-logs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..25 {
            std::fs::write(dir.join(format!("session-2000-01-01-0000{i:02}.txt")), "").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();

        let (path, _file) = open_log_file(&dir).unwrap();
        let mut sessions: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("session-"))
            .collect();
        sessions.sort();
        assert_eq!(sessions.len(), LOG_FILES);
        assert_eq!(
            sessions[0], "session-2000-01-01-000006.txt",
            "the oldest went"
        );
        assert!(path.exists() && sessions.last().unwrap().starts_with("session-20"));
        assert!(dir.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
