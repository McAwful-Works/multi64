//! The Play card: run a game's connector against the cart, through the Multi64 app's
//! daemon, for Archipelago's own client to connect to.
//!
//! There is no ROM file to choose. The connector reads the ROM from the cart (M64P
//! `PEEKROM`), and before starting, the session reads the cart's header and hook sites (or,
//! for a hook the profile finds per seed, the agent image) to check it is running the chosen
//! game with its agent in place. With [`AUTO`] chosen there is no game to check against:
//! the session takes every game whose header matches the cart's and keeps the one whose hooks
//! are in place ([`identify`]), and serves that game's client. It asks again whenever the link
//! to the cart restarts or the cart is found holding another ROM after a silence, since that is
//! when another ROM can have been loaded, failing the request that found it and checking
//! before the connector reads or writes anything more ([`Checked`]), and a session whose game changed builds the new game's connector in place of
//! the old one's.
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
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ap64_cart::backend::{link_restarted_error, Backend, Multi64, Stats};
use ap64_cart::Log;
use ap64_connector::server::{self, Event};
use ap64_connector::{relay, retroarch, Connector, Native, Protocol, Script, NATIVES, SCRIPTS};
use ap64_core::profile::{Addr, Transform, Write};
use ap64_core::transform::{dma_entries, rom_address, DmaEntry};
use ap64_core::{rom as rom_fmt, Bundle, Cart, Profile};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// The Multi64 app's daemon, where it listens by default.
pub const DEFAULT_URL: &str = "ws://127.0.0.1:38765/ws";

/// The game the page sends to have the session find out for itself which game is on the cart.
pub const AUTO: &str = "auto";

/// What a ROM that a version of AP64 other than this one patched is told. Named, as is
/// [`NO_AGENT`], because [`identify`] ranks them: of two games sharing a header, the failure
/// worth telling is the one about the ROM that is there.
const OTHER_VERSION: &str = "the ROM on the cart was patched by a different version of AP64; add \
                             the agent to the seed again with this one and load that ROM";

/// What a ROM without AP64's hook where its game has one is told.
const NO_AGENT: &str =
    "the ROM on the cart has no AP64 agent; add the agent to the seed and load that ROM";

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
    /// idle | connecting | waiting-console | client-setup | waiting-client | playing | stopped
    pub state: String,
    pub detail: String,
    /// The part of `detail` only a developer needs; empty when there is none.
    pub detail_dev: String,
    /// The three things that have to be up, each as the session last saw it: `idle`, `waiting`,
    /// `ok` or `failed`. The bridge can also be `nobridge`, `nocart` or `notinstalled`, which say
    /// which part of Multi64 is missing, and the AP client `needsfix`, `notinstalled` or
    /// `notsetup`, from an [`AUTO`] session holding a game's client (`LINK_TEXT` in `main.js` has
    /// the words).
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
    /// Whether the session finds the game itself ([`AUTO`]) rather than checking for one.
    pub auto: bool,
    /// The game the cart was found running, by profile id and by name; empty until it has been.
    pub game: String,
    pub game_name: String,
    /// What is in the way of that game's Archipelago client, when something is: the fix it needs,
    /// or a world that is not installed. Only an [`AUTO`] session says, since Start settles it
    /// for a game that was chosen; the page offers the fix from this, as Start would have.
    pub client_setup: Option<ClientSetup>,
}

pub struct Session {
    stop: Arc<AtomicBool>,
    /// What the connector is serving until: set with `stop`, and on its own when the cart turns
    /// out to be running another game, so that game's connector can take over.
    serve_stop: Arc<AtomicBool>,
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
    /// Set when a client fix was made, so a session holding for one looks again at once.
    recheck: Arc<AtomicBool>,
}

/// What plays a game: a connector script AP64 runs, or a client AP64 serves itself, by
/// answering it ([`Protocol::RetroArch`]) or by calling it and relaying ([`Protocol::Relay`]).
///
/// The window calls every game's Archipelago client "the AP client": someone playing opens it
/// from the Archipelago Launcher and has no reason to know which one it is underneath. The
/// session log, which is for finding out, names it ([`Kind::client`]).
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

/// A game the Play card offers, which connector plays it, and what to know before playing it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub name: String,
    pub connector: String,
    /// What to know before playing it on a console, shown at Start ([`Profile::notes`]).
    ///
    /// [`Profile::notes`]: ap64_core::profile::Profile::notes
    pub notes: Vec<String>,
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
                notes: b.profile.notes.clone(),
            })
        })
        .collect()
}

/// Whether the chosen game's Archipelago client is ready for AP64. The Play card asks when Start
/// is pressed, and offers the fix before starting when one is needed; for an [`AUTO`] session it
/// reads it from [`Status::client_setup`] instead.
///
/// Only a game whose client needs changing before it can reach AP64 has one of these at all
/// ([`ap64_connector::ClientFix`]); for every other game the answer is `None` and Start just
/// starts.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetup {
    /// `ready`, `needed`, `missing` (the client is not installed) or `unknown`.
    pub state: String,
    pub message: String,
    /// The installed copies the state is about, separated by `; `: for `needed`, each copy the
    /// fix changes.
    pub path: String,
    /// For `needed`, the installed copies the fix will not change and why, in plain words for
    /// the player: a version AP64 does not recognize, or one that could not be read. Empty when
    /// there are none, and for every other state.
    pub left: String,
    /// Why, for Developer details.
    pub technical: String,
}

fn client_fix(bundles: &[Bundle], game_id: &str) -> Option<ap64_connector::ClientFix> {
    let b = bundles.iter().find(|b| b.profile.id == game_id)?;
    match kind(&b.profile.connector)? {
        Kind::Native(n) => n.client_fix,
        Kind::Script(_) => None,
    }
}

pub fn client_setup(bundles: &[Bundle], game_id: &str) -> Option<ClientSetup> {
    let fix = client_fix(bundles, game_id)?;
    Some(setup_of(&fix, &(fix.find)()))
}

/// What one installed copy of the client's file is.
enum Installed {
    Unreadable(String),
    Is(ap64_connector::ClientState),
}

fn copies(fix: &ap64_connector::ClientFix, paths: &[PathBuf]) -> Vec<(String, Installed)> {
    paths
        .iter()
        .map(|p| {
            let state = match std::fs::read(p) {
                Ok(bytes) => Installed::Is((fix.state)(&bytes)),
                Err(e) => Installed::Unreadable(e.to_string()),
            };
            (p.display().to_string(), state)
        })
        .collect()
}

/// [`client_setup`] for the copies at `paths`. More than one Archipelago install can have the
/// world, and AP64 cannot tell which one the player will open
/// ([`ap64_core::installed::installed_apworlds`]), so it is `ready` only when every copy is,
/// and `needed` when any copy needs the fix, which is then made to each copy that needs it.
fn setup_of(fix: &ap64_connector::ClientFix, paths: &[PathBuf]) -> ClientSetup {
    use ap64_connector::ClientState;
    let setup = |state: &str, message: String, path: String, technical: String| ClientSetup {
        state: state.into(),
        message,
        path,
        left: String::new(),
        technical,
    };
    if paths.is_empty() {
        return setup(
            "missing",
            format!(
                "Install this game's world in Archipelago ({}) before playing",
                fix.file
            ),
            String::new(),
            "looked in custom_worlds and lib/worlds under %ProgramData%\\Archipelago and \
             %LOCALAPPDATA%\\Archipelago, and AP64_ARCHIPELAGO_DIR if set"
                .into(),
        );
    }
    let copies = copies(fix, paths);
    // What is wrong with each copy the fix cannot be made to, for Developer details.
    let odd: Vec<String> = copies
        .iter()
        .filter_map(|(path, c)| match c {
            Installed::Unreadable(e) => Some(format!("{path} could not be read: {e}")),
            Installed::Is(ClientState::Unrecognized(why)) => Some(format!("{path}: {why}")),
            Installed::Is(_) => None,
        })
        .collect();
    let needing: Vec<&str> = copies
        .iter()
        .filter(|(_, c)| matches!(c, Installed::Is(ClientState::NeedsFix)))
        .map(|(path, _)| path.as_str())
        .collect();
    if !needing.is_empty() {
        let what = if needing.len() == 1 {
            format!("add it to {} and keep the original", fix.file)
        } else {
            format!(
                "add it to each of the {} copies of {} installed and keep the originals",
                needing.len(),
                fix.file
            )
        };
        return ClientSetup {
            left: left_alone(fix, &copies),
            ..setup(
                "needed",
                format!("{}. AP64 will {what}", fix.why),
                needing.join("; "),
                odd.join("; "),
            )
        };
    }
    let all: Vec<&str> = copies.iter().map(|(path, _)| path.as_str()).collect();
    if let Some((path, c)) = copies
        .iter()
        .find(|(_, c)| !matches!(c, Installed::Is(ClientState::Ready)))
    {
        let message = match c {
            Installed::Unreadable(_) => format!("{} could not be read", fix.file),
            _ => format!(
                "This version of {} is not one AP64 knows, so the AP client may not connect",
                fix.file
            ),
        };
        return setup("unknown", message, path.clone(), odd.join("; "));
    }
    setup(
        "ready",
        "The AP client is ready for AP64".into(),
        all.join("; "),
        String::new(),
    )
}

/// What the player is told, beside a fix, about the copies it will not change: one AP64 does not
/// recognize is left as it is ([`fix_copies`]), and so is one that could not be read, which also
/// cannot be checked. Which copies, and the detail of why, are in Developer details (`technical`).
fn left_alone(fix: &ap64_connector::ClientFix, copies: &[(String, Installed)]) -> String {
    use ap64_connector::ClientState;
    let count = |f: fn(&Installed) -> bool| copies.iter().filter(|(_, c)| f(c)).count();
    let unknown = count(|c| matches!(c, Installed::Is(ClientState::Unrecognized(_))));
    let unreadable = count(|c| matches!(c, Installed::Unreadable(_)));
    let file = fix.file;
    let mut told = Vec::new();
    match unknown {
        0 => {}
        1 => told.push(format!(
            "One other copy of {file} is a version AP64 does not recognize, so AP64 will leave \
             it as it is. If the AP client runs that copy, it may not connect"
        )),
        n => told.push(format!(
            "{n} other copies of {file} are versions AP64 does not recognize, so AP64 will \
             leave them as they are. If the AP client runs one of them, it may not connect"
        )),
    }
    match unreadable {
        0 => {}
        1 => told.push(format!(
            "One other copy of {file} could not be read, so AP64 cannot tell whether it needs \
             the fix and will leave it as it is"
        )),
        n => told.push(format!(
            "{n} other copies of {file} could not be read, so AP64 cannot tell whether they \
             need the fix and will leave them as they are"
        )),
    }
    told.join(". ")
}

/// Make the chosen game's client fix, keeping the original in `backups`. Returns what to
/// tell the player.
pub fn fix_client(
    bundles: &[Bundle],
    game_id: &str,
    backups: &std::path::Path,
) -> Result<String, String> {
    let fix = client_fix(bundles, game_id).ok_or("this game's client needs no fix")?;
    fix_copies(&fix, &(fix.find)(), backups)
}

/// [`fix_client`] for the copies at `paths`: each one that needs the fix gets it, with its
/// original saved in `backups` first. Copies that are ready, that the fix does not know, or that
/// cannot be read are left as they are, and the last are named in what the player is told: one
/// copy AP64 cannot read must not keep it from fixing the ones it can.
fn fix_copies(
    fix: &ap64_connector::ClientFix,
    paths: &[PathBuf],
    backups: &std::path::Path,
) -> Result<String, String> {
    use ap64_connector::ClientState;
    if paths.is_empty() {
        return Err(format!("{} is not installed in Archipelago", fix.file));
    }
    let mut changes = Vec::new();
    let mut unread = Vec::new();
    for path in paths {
        let original = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                unread.push(format!("{} ({e})", path.display()));
                continue;
            }
        };
        if (fix.state)(&original) == ClientState::NeedsFix {
            let fixed = (fix.apply)(&original)?;
            changes.push((path, original, fixed));
        }
    }
    if changes.is_empty() {
        return Err(match unread.as_slice() {
            [] => format!("no installed copy of {} needs the fix", fix.file),
            _ => format!(
                "no copy of {} that AP64 could read needs the fix; it could not read {}",
                fix.file,
                unread.join(", ")
            ),
        });
    }
    std::fs::create_dir_all(backups).map_err(|e| format!("making {}: {e}", backups.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut saved = Vec::new();
    for (i, (path, original, fixed)) in changes.iter().enumerate() {
        // One name per copy: two installs' copies are fixed in the same second.
        let n = if i == 0 {
            String::new()
        } else {
            format!(".{}", i + 1)
        };
        let backup = backups.join(format!("{}.{stamp}{n}.original", fix.file));
        std::fs::write(&backup, original).map_err(|e| format!("saving the original: {e}"))?;
        // Written beside it and moved over it, so a failure part-way leaves the original in
        // place.
        let staged = path.with_extension("apworld.ap64-new");
        std::fs::write(&staged, fixed).map_err(|e| format!("writing {}: {e}", staged.display()))?;
        if let Err(e) = std::fs::rename(&staged, path) {
            let _ = std::fs::remove_file(&staged);
            let done = if saved.is_empty() {
                String::new()
            } else {
                format!(" ({} other copies were fixed)", saved.len())
            };
            return Err(format!(
                "{} is in use; close the AP client and the Archipelago Launcher, then try again{done} ({e})",
                path.display()
            ));
        }
        saved.push(backup.display().to_string());
    }
    let skipped = match unread.as_slice() {
        [] => String::new(),
        [one] => format!(". AP64 could not read {one}, so it left that copy as it is"),
        many => format!(
            ". AP64 could not read {}, so it left those copies as they are",
            many.join(", ")
        ),
    };
    let told = if saved.len() == 1 {
        format!(
            "Fixed. Restart the Archipelago Launcher before opening the AP client. The original \
             is saved as {}",
            saved[0]
        )
    } else {
        format!(
            "Fixed all {} copies. Restart the Archipelago Launcher before opening the AP client. \
             The originals are saved as {}",
            saved.len(),
            saved.join(", ")
        )
    };
    Ok(format!("{told}{skipped}"))
}

/// The file table in the cart's ROM at `table`, read a piece at a time up to its end.
fn cart_table(cart: &mut dyn Backend, table: u32) -> Result<Vec<DmaEntry>, String> {
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
fn verify_cart(bundle: &Bundle, cart: &mut dyn Backend) -> Result<OnCart, Issue> {
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
    let without = |why: String| Issue::new(NO_AGENT, why);
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
        // Any cart's build: the seed was patched for the cart it is on, which is Multi64's
        // business, not this check's.
        let images: Vec<&[u8]> = bundle
            .builds()
            .map(|b| b.blobs[&b.profile.agent.image].as_slice())
            .collect();
        let len = images.iter().map(|i| i.len()).max().unwrap_or(0);
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
            .read_rom_many(&[(at, len)])
            .map_err(|e| Issue::plain(e.to_string()))?;
        if !images.iter().any(|i| got[0].starts_with(i)) {
            // The hook is AP64's, so the seed was patched: by another version of AP64, whose
            // agent or stub differs from this one's. Its stub may even say the agent is
            // somewhere else, which is why the address can look strange.
            return Err(Issue::new(
                OTHER_VERSION,
                format!("the agent at ROM 0x{at:X} is not this build's"),
            ));
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

/// Which of `bundles` the cart is running with its agent: every game whose header matches the
/// cart's is checked as [`verify_cart`] checks a chosen one, and the one that passes is it.
///
/// The header alone does not say. Worlds rewrite it, and two of them, Banjo-Tooie's and Star Fox
/// 64's, both write `NEDE` version `0x30`, the advanced header flash cart menus read the save
/// type from. Their hooks are in different places, so the check that a chosen game's hooks are
/// in place is also what tells them apart, for a few more words read from the cart.
fn identify<'a>(
    bundles: &'a [Bundle],
    cart: &mut dyn Backend,
) -> Result<(&'a Bundle, OnCart), Issue> {
    // The same region verify_cart reads, which the cart backend caches, so this costs nothing
    // more over the wire.
    let got = cart
        .read_rom_many(&[(0u32, 0x1000usize)])
        .map_err(|e| Issue::plain(e.to_string()))?;
    let h = rom_fmt::header(&got[0])
        .ok_or_else(|| Issue::plain("the cart's ROM header could not be read"))?;
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for b in bundles
        .iter()
        .filter(|b| b.profile.game_code == h.game_code && b.profile.version == h.version)
    {
        match verify_cart(b, cart) {
            Ok(on) => passed.push((b, on)),
            Err(e) => failed.push(e),
        }
    }
    let header = format!(
        "cart header {} {} v{}",
        if h.name.is_empty() {
            "(no name)"
        } else {
            &h.name
        },
        h.game_code,
        h.version
    );
    match (passed.len(), failed.len()) {
        (1, _) => Ok(passed.pop().expect("one passed")),
        (0, 0) => Err(Issue::new(
            "the cart is running a game AP64 does not support; load a ROM AP64 added the agent to",
            header,
        )),
        (0, _) => {
            // Of two games sharing a header, the failure worth telling is the one about the ROM
            // that is there: AP64's hook with another version's agent behind it says more than
            // a hook missing, and that more than a read past the end of a ROM too short to be
            // the other game.
            let rank = |e: &Issue| match e.message.as_str() {
                OTHER_VERSION => 2,
                NO_AGENT => 1,
                _ => 0,
            };
            let mut best = 0;
            for (i, e) in failed.iter().enumerate() {
                if rank(e) > rank(&failed[best]) {
                    best = i;
                }
            }
            Err(failed.swap_remove(best))
        }
        _ => Err(Issue::new(
            "the ROM on the cart passes the checks for more than one game; choose the game \
             instead of Auto-detect",
            format!(
                "{header} passes for {}",
                passed
                    .iter()
                    .map(|(b, _)| b.profile.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// What a session plays: the game chosen, or whichever of them the cart is found running.
enum Choice {
    Game(Box<Bundle>),
    Auto(Vec<Bundle>),
}

impl Choice {
    /// The game on the cart, with its agent in place, or why not.
    fn find(&self, cart: &mut dyn Backend) -> Result<(Bundle, OnCart), Issue> {
        match self {
            Choice::Game(g) => verify_cart(g, cart).map(|on| ((**g).clone(), on)),
            Choice::Auto(all) => identify(all, cart).map(|(g, on)| (g.clone(), on)),
        }
    }

    /// The game before the cart has said which it is: known only when it was chosen.
    fn profile(&self) -> Option<&Profile> {
        match self {
            Choice::Game(g) => Some(&g.profile),
            Choice::Auto(_) => None,
        }
    }
}

/// What a session asks of a cart whose link changed ([`Backend::generation`]): `Ok` while it is
/// still running the game being served, or what it is running instead.
type SameGame = Box<dyn Fn(&mut dyn Backend) -> Result<(), String>>;

/// How many times in a row a link may restart while it is being checked before the check gives
/// up on it.
const CHECK_TRIES: u32 = 3;

/// The cart as a session's connector is given it: asked again what it is running whenever its
/// link changes, before anything more is read from it or written to it.
///
/// The question used to be asked from the session's event handler, which runs after a request
/// has been answered, so the first request on a new link was answered from the new ROM as though
/// it were the old game's. Here it is asked inside the request that finds the change, and that
/// request fails, as one the link restarted under does. A change of game is final: every request
/// after it fails the same way, until the session stops serving and finds the game afresh.
struct Checked<B> {
    inner: Rc<CheckedInner<B>>,
}

struct CheckedInner<B> {
    cart: Rc<RefCell<B>>,
    same_game: SameGame,
    /// What the connector serves until: set once the game is found changed, so it stops and the
    /// session finds the game afresh.
    serve: Arc<AtomicBool>,
    /// The link generation last found running the game.
    checked: Cell<u32>,
    /// What the cart turned out to be running instead, once it has.
    changed: RefCell<Option<String>>,
}

impl<B> Clone for Checked<B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<B: Backend> Checked<B> {
    /// `found_on` is the link generation the game was found on, so a change since then, while
    /// the connector was being built, is checked as any other.
    fn new(
        cart: Rc<RefCell<B>>,
        found_on: u32,
        same_game: SameGame,
        serve: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Rc::new(CheckedInner {
                cart,
                same_game,
                serve,
                checked: Cell::new(found_on),
                changed: RefCell::new(None),
            }),
        }
    }

    /// What the cart is running instead of the game, once a check has found it.
    fn changed(&self) -> Option<String> {
        self.inner.changed.borrow().clone()
    }

    /// Whether the cart is still running the game, asking it again if its link changed since it
    /// last said so. A link that restarts during the check is checked again.
    fn check(&self) -> io::Result<()> {
        let i = &self.inner;
        let mut tries = 0;
        loop {
            if let Some(why) = i.changed.borrow().as_ref() {
                return Err(link_restarted_error(why.clone()));
            }
            let at = i.cart.borrow().generation();
            if at == i.checked.get() {
                return Ok(());
            }
            let why = if tries == CHECK_TRIES {
                "the link to the cart kept restarting while it was checked".to_string()
            } else {
                tries += 1;
                let same = (i.same_game)(&mut *i.cart.borrow_mut());
                match same {
                    // The same game, unless the link restarted again under the check.
                    Ok(()) if i.cart.borrow().generation() == at => {
                        i.checked.set(at);
                        continue;
                    }
                    Ok(()) => continue,
                    Err(why) => why,
                }
            };
            *i.changed.borrow_mut() = Some(why);
            i.serve.store(true, Ordering::Relaxed);
        }
    }

    /// `op` on the cart, between two checks: one so nothing more reaches a cart found running
    /// another game, and one so nothing read from one is handed back as the game's. A write that
    /// was on its way when the console was reset may have landed before the second finds it.
    ///
    /// An `op` that failed is not checked after: what failed it is the error worth telling, and
    /// asking a cart that has just failed what it is running would only wait on it again. The
    /// next request's check asks.
    fn checked<T>(&self, op: impl FnOnce(&mut B) -> io::Result<T>) -> io::Result<T> {
        self.check()?;
        let done = op(&mut self.inner.cart.borrow_mut())?;
        self.check()?;
        Ok(done)
    }
}

impl<B: Backend> Backend for Checked<B> {
    fn rdram_size(&self) -> u32 {
        self.inner.cart.borrow().rdram_size()
    }
    fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.checked(|c| c.read_many(regions))
    }
    fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
        self.checked(|c| c.write_many(writes))
    }
    fn rom_window(&self) -> Option<u32> {
        self.inner.cart.borrow().rom_window()
    }
    fn read_rom_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.checked(|c| c.read_rom_many(regions))
    }
    fn generation(&self) -> u32 {
        self.inner.cart.borrow().generation()
    }
    fn set_watch(&mut self, watch: ap64_cart::watch::Watch) {
        self.inner.cart.borrow_mut().set_watch(watch)
    }
    fn take_watched(&mut self) -> Option<Vec<u8>> {
        self.inner.cart.borrow_mut().take_watched()
    }
    fn sample_watch(&mut self) -> io::Result<()> {
        self.checked(|c| c.sample_watch())
    }
    fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
        self.inner.cart.borrow().watch_stats()
    }
}

/// The question a [`Checked`] cart asks, for a session serving `game`.
fn same_game(choice: Rc<Choice>, game: String) -> SameGame {
    Box::new(move |cart| match choice.find(cart) {
        Ok((now, _)) if now.profile.id == game => Ok(()),
        Ok((now, _)) => Err(format!("the console is now running {}", now.profile.name)),
        Err(e) => Err(e.to_string()),
    })
}

/// Why the client of a game an [`AUTO`] session found cannot be served yet. Start settles the
/// same things for a game that was chosen, before the session begins.
struct Hold {
    /// The status line: what is missing and what to do.
    detail: String,
    /// The AP client row's state.
    client: &'static str,
    setup: Option<ClientSetup>,
}

/// Whether `profile`'s client can be served now: `Ok` with what the page is worth telling (a
/// version of the world AP64 does not recognize, which may still connect), or the [`Hold`]
/// that keeps it from being served.
fn client_ready(profile: &Profile, kind: Kind) -> Result<Option<ClientSetup>, Box<Hold>> {
    match kind {
        Kind::Script(script) => script.listen_on().map(|_| None).map_err(|e| {
            Box::new(Hold {
                detail: e,
                client: NOT_SET_UP,
                setup: None,
            })
        }),
        Kind::Native(Native {
            client_fix: Some(fix),
            ..
        }) => {
            let setup = setup_of(&fix, &(fix.find)());
            match setup.state.as_str() {
                "ready" => Ok(None),
                "needed" => Err(Box::new(Hold {
                    detail: format!(
                        "the AP client for {} needs a one-time fix before it can reach AP64; \
                         press Stop, then Start, to be offered it again",
                        profile.name
                    ),
                    client: NEEDS_FIX,
                    setup: Some(setup),
                })),
                "missing" => Err(Box::new(Hold {
                    detail: format!(
                        "{} is on the console, but its world is not installed in Archipelago \
                         ({}); AP64 carries on once it is",
                        profile.name, fix.file
                    ),
                    client: NOT_INSTALLED,
                    setup: Some(setup),
                })),
                _ => Ok(Some(setup)),
            }
        }
        Kind::Native(_) => Ok(None),
    }
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
        let choice = if game_id == AUTO {
            // Only the games the Play card offers: one without a connector could be found on the
            // cart and then not be played.
            Choice::Auto(
                bundles
                    .iter()
                    .filter(|b| kind(&b.profile.connector).is_some())
                    .cloned()
                    .collect(),
            )
        } else {
            let game = bundles
                .iter()
                .find(|b| b.profile.id == game_id)
                .ok_or("choose a game")?
                .clone();
            let kind = kind(&game.profile.connector).ok_or("unknown connector")?;
            // A client whose port is set up on this PC (Pokémon Snap's, in Project64's settings)
            // cannot be served until it is, and saying so now beats a session that cannot listen.
            // An AUTO session asks once it knows the game (client_ready).
            if let Kind::Script(script) = kind {
                script.listen_on()?;
            }
            Choice::Game(Box::new(game))
        };
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
        let serve_stop = Arc::new(AtomicBool::new(false));
        let status = self.status.clone();
        *status.lock().unwrap() = Status {
            running: true,
            state: "connecting".into(),
            detail: match &choice {
                Choice::Game(g) => g.profile.name.clone(),
                Choice::Auto(_) => String::new(),
            },
            detail_dev: url.clone(),
            auto: matches!(choice, Choice::Auto(_)),
            ..Status::default()
        };
        let _ = app.emit("play://status", status.lock().unwrap().clone());
        // A fix made before this session is not one it is waiting for.
        self.recheck.store(false, Ordering::Relaxed);

        let thread = {
            let stops = Stops {
                session: stop.clone(),
                serve: serve_stop.clone(),
                recheck: self.recheck.clone(),
            };
            std::thread::Builder::new()
                .name("connector".into())
                .spawn(move || run(app, choice, url, stops, status, kept, file))
                .map_err(|e| e.to_string())?
        };
        *self.session.lock().unwrap() = Some(Session {
            stop,
            serve_stop,
            thread,
        });
        Ok(())
    }

    /// A client fix was just made: a session holding for it looks again now rather than at its
    /// next look ([`CLIENT_RECHECK`]).
    pub fn client_fixed(&self) {
        self.recheck.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        if let Some(s) = self.session.lock().unwrap().take() {
            // The session's flag first: a session that sees the connector's flag set checks it
            // to tell a Stop from a change of game.
            s.stop.store(true, Ordering::Relaxed);
            s.serve_stop.store(true, Ordering::Relaxed);
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
/// Nothing answers, the Multi64 app is not running, and it is not where it gets installed:
/// waiting will not fix this one, so it is not "waiting". Also the AP client's, for a game an
/// [`AUTO`] session found whose world is not installed in Archipelago.
const NOT_INSTALLED: &str = "notinstalled";
/// The AP client of a game an [`AUTO`] session found needs AP64's fix before it can connect.
const NEEDS_FIX: &str = "needsfix";
/// The AP client of a game an [`AUTO`] session found has a port that is not set up on this PC
/// (Pokémon Snap's, in Project64's settings).
const NOT_SET_UP: &str = "notsetup";

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
        log(
            "Multi64 is not running, and AP64 cannot find it installed to start it; \
             set MULTI64_APP to its multi64.exe if it is somewhere else"
                .into(),
        );
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
/// How long the cart may be there with its ROM silent before the status suggests why. Longer
/// than a reset or a ROM swap usually takes, so it does not appear for those.
const HINT_AFTER: Duration = Duration::from_secs(15);

/// What a ROM silent for [`HINT_AFTER`] with the cart there most likely means, as far as AP64
/// can tell. A hint, not a finding: a console that is off, or running another ROM, is silent
/// the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Silence {
    /// A cart whose agent no game has been played through: an EverDrive. Its port is open whether
    /// or not anything runs on the console, so a connected cart says nothing about the ROM.
    /// `pak` is whether a missing Expansion Pak is possible too, as for [`Silence::Pak`].
    Untested { cart: Cart, pak: bool },
    /// The stub may have found no Expansion Pak and skipped the agent.
    Pak,
    /// Neither: the game itself needs the Pak, so a game that runs has one.
    Other,
    /// No game was chosen ([`AUTO`]), so either may be why.
    Unknown,
}

impl Silence {
    /// `profile` is the chosen game's, and `None` for an [`AUTO`] session, which cannot know what
    /// is on a cart that does not answer. `cart` is the one the daemon was started for, when it
    /// says.
    fn of(profile: Option<&Profile>, cart: Option<Cart>) -> Self {
        let pak = profile.is_none_or(|p| p.silence_may_be_the_pak());
        match cart {
            Some(cart) if !cart.tested() => Silence::Untested { cart, pak },
            _ if profile.is_none() => Silence::Unknown,
            _ if pak => Silence::Pak,
            _ => Silence::Other,
        }
    }

    /// What the status says.
    fn detail(self) -> String {
        match self {
            Silence::Untested { cart, .. } => format!(
                "the ROM is not answering. AP64's agent for the {cart} is experimental, and no \
                 game has been played through it yet: check the ROM was patched for the {cart}"
            ),
            Silence::Pak => "the ROM is not answering. If it is running, check the console has \
                             an Expansion Pak: AP64 needs one for this game"
                .into(),
            Silence::Other => "the ROM is not answering. If it is running, check it is the one \
                               AP64 added the agent to"
                .into(),
            Silence::Unknown => "the ROM is not answering. If it is running, check it is one AP64 \
                                 added the agent to, and that the console has an Expansion Pak"
                .into(),
        }
    }

    /// What the session log says, once a wait.
    fn log_line(self) -> String {
        let after = HINT_AFTER.as_secs();
        match self {
            Silence::Untested { cart, pak } => format!(
                "the {cart}'s port is open but the ROM has not answered for {after} s; an open \
                 port does not mean a ROM is running, and no game has been played through \
                 AP64's agent for the {cart} yet{}",
                if pak {
                    "; without an Expansion Pak the stub also skips the agent"
                } else {
                    ""
                }
            ),
            Silence::Pak => format!(
                "the cart is connected but the ROM has not answered for {after} s; without an \
                 Expansion Pak the stub skips the agent and nothing answers"
            ),
            Silence::Other => format!(
                "the cart is connected but the ROM has not answered for {after} s; this game \
                 does not run without an Expansion Pak, so a missing one is not why"
            ),
            Silence::Unknown => format!(
                "the cart is connected but the ROM has not answered for {after} s; a ROM \
                 without AP64's agent is silent like this, and so is one whose stub found no \
                 Expansion Pak"
            ),
        }
    }
}

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

/// How often a session holding a game's client ([`Hold`]) looks again, short of being told a fix
/// was made ([`Play::client_fixed`]): a world installed meanwhile, or Project64 set up.
const CLIENT_RECHECK: Duration = Duration::from_secs(30);

/// Probes a held session's cart may miss in a row before it is taken to have gone, and the
/// session goes back to waiting for it. One miss is a scene load as often as anything.
const HOLD_MISSES: u32 = 3;

/// The flags a session is stopped by, and nudged by.
struct Stops {
    /// Stop pressed.
    session: Arc<AtomicBool>,
    /// What the connector serves until (`Session::serve_stop`).
    serve: Arc<AtomicBool>,
    /// A client fix was made ([`Play::client_fixed`]).
    recheck: Arc<AtomicBool>,
}

/// Run `session`, and then end it: log that it stopped and mark the status stopped, which is
/// what hands Start back. Returns the final status, to be sent to the page.
///
/// The end runs whatever the session did, panicking included. A panic on the session thread
/// used to skip it, and the page went on showing a running session with Start disabled until
/// AP64 was restarted. Neither lock can be left unusable either: a panic while one was held
/// poisons it, and `Play::status` takes it on every call from the page.
fn run_to_end(status: &Mutex<Status>, log: &Log, session: impl FnOnce()) -> Status {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(session)) {
        let why = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".into());
        log(format!(
            "the session failed inside AP64 ({why}); press Start to try again"
        ));
    }
    log("stopped".into());
    let mut s = status.lock().unwrap_or_else(PoisonError::into_inner);
    s.running = false;
    s.state = "stopped".into();
    s.detail = String::new();
    s.detail_dev = String::new();
    s.bridge = IDLE.into();
    s.console = IDLE.into();
    s.client = IDLE.into();
    s.game = String::new();
    s.game_name = String::new();
    s.client_setup = None;
    let end = s.clone();
    drop(s);
    status.clear_poison();
    end
}

fn run(
    app: AppHandle,
    choice: Choice,
    url: String,
    stops: Stops,
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
                let mut f = file.lock().unwrap_or_else(PoisonError::into_inner);
                if f.as_mut().is_some_and(|f| writeln!(f, "{line}").is_err()) {
                    *f = None;
                }
            }
            {
                let mut kept = kept_log.lock().unwrap_or_else(PoisonError::into_inner);
                if kept.len() == LOG_LINES {
                    kept.pop_front();
                }
                kept.push_back(line.clone());
            }
            // To every window: the log window listens for these, and may not be open.
            let _ = app.emit("play://log", line);
        })
    };
    let end = run_to_end(&status, &log, || {
        session(&app, choice, &url, &stops, &status, &log, file_path)
    });
    let _ = app.emit("play://status", end);
}

/// A session, from waiting for the cart to Stop. [`run_to_end`] ends it.
fn session(
    app: &AppHandle,
    choice: Choice,
    url: &str,
    stops: &Stops,
    status: &Arc<Mutex<Status>>,
    log: &Log,
    file_path: Result<PathBuf, String>,
) {
    let stop = &stops.session;
    let auto = matches!(choice, Choice::Auto(_));
    // Shared with the cart's check for a change of game, which outlives a borrow (see Checked).
    let choice = Rc::new(choice);
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
        "AP64 {} ({}), {}, through Multi64 at {url}",
        env!("CARGO_PKG_VERSION"),
        env!("AP64_COMMIT"),
        match &*choice {
            Choice::Game(g) => format!("{} ({} profile)", g.profile.name, g.profile.id),
            Choice::Auto(all) => format!("auto-detecting the game among {} profiles", all.len()),
        }
    ));
    log(match &file_path {
        Ok(path) => format!("this log is also saved to {}", path.display()),
        Err(e) => format!("this log is not being saved to a file: {e}"),
    });

    // A session needs Multi64, so start it rather than wait for someone to notice.
    if !multi64_app_running() {
        start_multi64(log);
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
    // Release notes are said once a session for each game, not at every reconnect.
    let mut noted = std::collections::HashSet::new();
    // The game the last link was found running, so a change of game is worth its own line.
    let mut last_game: Option<String> = None;
    'session: while !stop.load(Ordering::Relaxed) {
        if !std::mem::take(&mut first_try) {
            pause();
        }
        // Cleared for this link and Stop looked at after, so a Stop landing in between still
        // reaches the connector: Play::stop sets the session's flag before this one.
        stops.serve.store(false, Ordering::Relaxed);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Wait for the cart: the console may not be on yet, or the daemon not started.
        // Since when the cart has been there and the ROM silent, for the hint at why.
        let mut rom_silent_since: Option<Instant> = None;
        let mut hint_logged = false;
        let cart = loop {
            if stop.load(Ordering::Relaxed) {
                break 'session;
            }
            match Multi64::connect(url, log.clone()) {
                Ok(c) => break c,
                Err(e) => {
                    let msg = e.to_string();
                    // A failed connect on its own says nothing about which end is at fault: the
                    // HELLO it waits for needs the daemon, the serial link and an agent, and any
                    // of the three being absent looks the same from here. The daemon answers a
                    // plain HTTP GET whether or not a cart is attached, so ask it.
                    // One request serves both this and the cart the hint below needs.
                    let status = ap64_cart::transport::daemon_status(url);
                    let daemon = status.as_ref().and_then(|s| s.serial_active);
                    // Only when the bridge is silent is the app's own state worth the look.
                    let app = daemon.is_none() && multi64_app_running();
                    // And when it is not running either, whether there is one to start: asked
                    // each time, so installing it mid-session moves the status on. Found means
                    // only found -- someone running it from elsewhere sees "not installed" until
                    // it is up, and then its bridge answers and none of this is asked.
                    let installed = daemon.is_some() || app || multi64_exe().is_some();
                    // Nothing on the console can say why a ROM stays silent with the cart there.
                    // After long enough for a reset or a ROM swap, suggest the likely cause, which
                    // depends on the game and on the cart the daemon was started for.
                    let silent = match daemon {
                        Some(true) => *rom_silent_since.get_or_insert_with(Instant::now),
                        _ => {
                            rom_silent_since = None;
                            Instant::now()
                        }
                    };
                    let hint = (silent.elapsed() >= HINT_AFTER).then(|| {
                        let cart = status.as_ref().and_then(|s| s.cart.as_deref());
                        Silence::of(choice.profile(), cart.and_then(Cart::parse))
                    });
                    if let Some(hint) = hint {
                        if !std::mem::replace(&mut hint_logged, true) {
                            log(hint.log_line());
                        }
                    }
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
                            None if !installed => {
                                s.detail = "AP64 needs the Multi64 app, which is installed \
                                            separately: install it from the same place as AP64"
                                    .into();
                                s.bridge = NOT_INSTALLED.into();
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
                                s.detail = match hint {
                                    Some(hint) => hint.detail(),
                                    None => "waiting for the ROM on the console".into(),
                                };
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
        // Before anything is asked of the cart, so that Stop is felt, and a console that goes
        // away is given up on within CART_GRACE rather than the backend's own minutes, while the
        // game is being found too: finding it reads the header and every candidate's hooks.
        {
            let stop = stop.clone();
            cart.set_cancelled(Rc::new(move || stop.load(Ordering::Relaxed)));
        }
        cart.set_reconnect_deadline(CART_GRACE);
        // The link the game is found on. A link that restarts while the game is being found may
        // have done so under a reset into another ROM, leaving the answer half one image's and
        // half the other's, so it is found again.
        let mut found_on = cart.generation();
        let found = loop {
            let found = choice.find(&mut cart);
            let now = cart.generation();
            if now == found_on || stop.load(Ordering::Relaxed) {
                break found;
            }
            log(
                "the link to the cart restarted while the game was being found; looking again"
                    .into(),
            );
            found_on = now;
        };
        let (game, on) = match found {
            Ok(found) => found,
            Err(e) => {
                log(e.to_string());
                set(&|s| {
                    s.state = "waiting-console".into();
                    s.detail = e.message.clone();
                    s.detail_dev = e.technical.clone();
                    s.console = FAILED.into();
                    s.game = String::new();
                    s.game_name = String::new();
                    s.client_setup = None;
                });
                continue 'session;
            }
        };
        let kind = kind(&game.profile.connector)
            .expect("a session only plays games with a connector (Play::start)");
        if let Some(was) = last_game.replace(game.profile.name.clone()) {
            if was != game.profile.name {
                log(format!("the console changed games, from {was}"));
            }
        }
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
        if noted.insert(game.profile.id.clone()) {
            for note in ap64_core::installed::release_notes(&game.profile, on.name.trim()) {
                log(format!("note: {note}"));
            }
        }
        set(&|s| {
            s.console = OK.into();
            s.game = game.profile.id.clone();
            s.game_name = game.profile.name.clone();
        });

        // A game that was chosen had its client settled at Start. One that was found has it
        // settled here, and its client is not served until it is: what Start would have refused
        // to begin, this waits out, looking again when told a fix was made, every
        // CLIENT_RECHECK, and going back to waiting for the console if the cart goes quiet,
        // since another ROM may be loaded in the meantime.
        if auto {
            let mut held: Option<String> = None;
            let mut misses = 0;
            loop {
                stops.recheck.store(false, Ordering::Relaxed);
                let hold = match client_ready(&game.profile, kind) {
                    Ok(setup) => {
                        if let Some(setup) = &setup {
                            log(format!("{}: {}", kind.client(), setup.message));
                        }
                        if held.is_some() {
                            log(format!("{} is ready", kind.client()));
                        }
                        set(&|s| s.client_setup = setup.clone());
                        break;
                    }
                    Err(hold) => hold,
                };
                if held.as_deref() != Some(&hold.detail) {
                    log(format!(
                        "not serving {} yet: {}",
                        kind.client(),
                        hold.detail
                    ));
                    held = Some(hold.detail.clone());
                }
                set(&|s| {
                    s.state = "client-setup".into();
                    s.detail = hold.detail.clone();
                    s.detail_dev = hold
                        .setup
                        .as_ref()
                        .map(|c| c.technical.clone())
                        .unwrap_or_default();
                    s.client = hold.client.into();
                    s.client_setup = hold.setup.clone();
                });
                let until = Instant::now() + CLIENT_RECHECK;
                let mut probed = Instant::now();
                while Instant::now() < until && !stops.recheck.load(Ordering::Relaxed) {
                    if stop.load(Ordering::Relaxed) {
                        break 'session;
                    }
                    if probed.elapsed() >= CART_QUIET {
                        probed = Instant::now();
                        misses = if cart.alive() { 0 } else { misses + 1 };
                        // Answering again with another ROM in it: what was found is not there.
                        if cart.generation() != found_on {
                            log("the ROM on the cart changed while its AP client was held; \
                                 checking it again"
                                .into());
                            set(&|s| {
                                s.state = "waiting-console".into();
                                s.detail = "the ROM on the console changed; checking it".into();
                                s.detail_dev = String::new();
                                s.console = WAITING.into();
                                s.client = IDLE.into();
                                s.game = String::new();
                                s.game_name = String::new();
                                s.client_setup = None;
                            });
                            continue 'session;
                        }
                        if misses >= HOLD_MISSES {
                            log("the cart stopped answering while the AP client was held".into());
                            set(&|s| {
                                s.state = "waiting-console".into();
                                s.detail =
                                    "the ROM stopped answering; load it again on the console"
                                        .into();
                                s.detail_dev = String::new();
                                s.console = FAILED.into();
                                s.client = IDLE.into();
                                s.game = String::new();
                                s.game_name = String::new();
                                s.client_setup = None;
                            });
                            continue 'session;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
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
        // A request that fails is retried and then reconnected for as long as the deadline set
        // above, and nothing comes back to this loop in the meantime -- so without this, a
        // console that was reset went on being reported as running until that deadline expired.
        // The client notices immediately, because its own reads are what fail; this is how AP64
        // learns it at the same time.
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
                        "AP client connected"
                    } else {
                        "open the AP client from the Archipelago Launcher"
                    }
                    .into();
                } else {
                    s.state = "waiting-console".into();
                    s.detail = "the ROM stopped answering; load it again on the console".into();
                }
                s.detail_dev = String::new();
                let _ = app.emit("play://status", s.clone());
            }));
        }

        // Shared, so the session can still read counters once the connector holds it. The
        // shared cart is the cart: `Rc<RefCell<_>>` forwards every `Backend` method, the
        // watch included.
        let cart = Rc::new(RefCell::new(cart));
        // What the connector is given: the same cart, asked again what it is running whenever
        // its link changes. The backend rebuilds a link that stops answering by itself, and
        // takes a ROM found changed after a silence as a new link too; either may be a console
        // reset into another game, which the connector would otherwise go on serving as this
        // one. The same game carries on with its client still connected.
        let checked = Checked::new(
            cart.clone(),
            found_on,
            same_game(choice.clone(), game.profile.id.clone()),
            stops.serve.clone(),
        );
        // A native connector has no script to load: AP64 answers its client itself.
        let connector = match kind {
            Kind::Native(_) => None,
            Kind::Script(script) => {
                match Connector::new(&script, Box::new(checked.clone()), log.clone()) {
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
                            s.game = String::new();
                            s.game_name = String::new();
                            s.client_setup = None;
                        });
                        continue 'session;
                    }
                }
            }
        };

        // Which client, for the log; the window says "the AP client" (see [`Kind`]).
        let client = kind.client();
        // Where the client finds AP64, for the log: scripts are TCP, RetroArch's commands UDP. A
        // relay calls the client instead, and says so with Event::Dialing.
        let transport = match kind {
            Kind::Script(_) => "localhost",
            Kind::Native(Native {
                protocol: Protocol::RetroArch(_),
                ..
            }) => "UDP localhost",
            Kind::Native(Native {
                protocol: Protocol::Relay(_),
                ..
            }) => "localhost",
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
                        s.detail = "open the AP client from the Archipelago Launcher".into();
                        s.detail_dev = format!("listening on 127.0.0.1:{port}");
                        s.client = WAITING.into();
                    });
                }
                Event::Dialing(port) => {
                    log(format!(
                        "calling {client} on {transport}:{port}; open it from the Archipelago Launcher"
                    ));
                    set(&|s| {
                        s.state = "waiting-client".into();
                        s.port = Some(port);
                        s.detail = "open the AP client from the Archipelago Launcher".into();
                        s.detail_dev = format!("calling 127.0.0.1:{port}");
                        s.client = WAITING.into();
                    });
                }
                Event::ClientConnected(addr) => {
                    client_here.set(true);
                    log(format!("{client} connected from {addr}"));
                    set(&|s| {
                        s.state = "playing".into();
                        s.detail = "AP client connected".into();
                        s.detail_dev = format!("from {addr}");
                        s.client = OK.into();
                    });
                }
                Event::ClientDisconnected(why) => {
                    client_here.set(false);
                    log(format!("{client} disconnected: {why}"));
                    set(&|s| {
                        s.state = "waiting-client".into();
                        s.detail = "AP client disconnected; waiting for it".into();
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
                        // A probe that finds another ROM is how a session with no client hears
                        // of one, so it is asked about now rather than when a client next asks
                        // something. A change stops the connector.
                        let _ = checked.check();
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
                     cart; {} stalls, {} sent again early, {} reconnects; {client} {}",
                    SUMMARY_EVERY.as_secs() / 60,
                    handled - h,
                    misses - m,
                    stats.stalls - st.stalls,
                    stats.quick_resends - st.quick_resends,
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
            (Kind::Script(script), Some(connector)) => script
                .listen_on()
                .and_then(|ports| server::serve(connector, &ports, &stops.serve, &mut on_event)),
            // The cart stays on this thread, and each call borrows it only for its length, so
            // the handler's own borrows above never overlap one of these.
            (Kind::Native(native), _) => match &native.protocol {
                Protocol::RetroArch(opts) => {
                    retroarch::serve(&mut checked.clone(), opts, &stops.serve, &mut on_event)
                }
                Protocol::Relay(opts) => {
                    relay::serve(&mut checked.clone(), opts, &stops.serve, &mut on_event)
                }
            },
            (Kind::Script(_), None) => unreachable!("a script session loads its script first"),
        };
        // Whatever stalls were being counted belong before what happens next.
        cart.borrow_mut().flush_stalls(true);
        push_stats(cart.borrow().stats(), handled);
        // A game found changed is the reason worth telling, however the serving ended: a
        // connector may have reported it as its cart failing.
        match (result, checked.changed()) {
            // `serve` only returns cleanly when the stop flag it was given is set: by Stop, or
            // because the console turned out to be running something else.
            (Ok(()), _) if stop.load(Ordering::Relaxed) => break 'session,
            (Ok(()), changed) | (Err(_), changed @ Some(_)) => {
                let why = changed.unwrap_or_else(|| "the console changed".into());
                log(format!("{why}; checking the cart again"));
                set(&|s| {
                    s.state = "waiting-console".into();
                    s.detail = "the ROM on the console changed; checking it".into();
                    s.detail_dev = why.clone();
                    s.console = WAITING.into();
                    s.client = IDLE.into();
                    s.game = String::new();
                    s.game_name = String::new();
                    s.client_setup = None;
                });
            }
            (Err(e), None) => {
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
                    s.game = String::new();
                    s.game_name = String::new();
                    s.client_setup = None;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session that panics still ends: the status says stopped, so the page hands Start
    /// back, and the log says what happened. It used to leave Start disabled until restart.
    #[test]
    fn a_session_that_panics_still_hands_start_back() {
        let status = Mutex::new(Status {
            running: true,
            state: "playing".into(),
            console: OK.into(),
            ..Status::default()
        });
        let lines = Arc::new(Mutex::new(Vec::new()));
        let log: Log = {
            let lines = lines.clone();
            Arc::new(move |l| lines.lock().unwrap().push(l))
        };
        let end = run_to_end(&status, &log, || {
            // A poisoned status lock as well, the worst a panic can leave behind.
            let _held = status.lock().unwrap();
            let short: &[u8] = &[1, 2];
            let _ = u32::from_be_bytes(short[..4].try_into().unwrap());
        });
        assert!(!end.running && end.state == "stopped", "{end:?}");
        let now = status.lock().expect("the status lock is usable again");
        assert!(!now.running && now.console == IDLE);
        let lines = lines.lock().unwrap();
        assert!(
            lines[0].contains("the session failed inside AP64"),
            "{lines:?}"
        );
        assert_eq!(lines.last().unwrap(), "stopped");
    }

    /// A session that ends normally ends the same way, with nothing said about a failure.
    #[test]
    fn a_session_that_returns_is_stopped_quietly() {
        let status = Mutex::new(Status {
            running: true,
            ..Status::default()
        });
        let lines = Arc::new(Mutex::new(Vec::new()));
        let log: Log = {
            let lines = lines.clone();
            Arc::new(move |l| lines.lock().unwrap().push(l))
        };
        let end = run_to_end(&status, &log, || {});
        assert!(!end.running);
        assert_eq!(*lines.lock().unwrap(), vec!["stopped".to_string()]);
    }

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

    /// A silent ROM on an EverDrive is put down to the EverDrive, whose open port proves nothing
    /// and whose agent is experimental, and not to the Pak; on the SummerCart64 the Pak is named
    /// only for a game that runs without one. Seen first as OoT on an X7, told to check a Pak
    /// that OoT's randomizer had already needed in order to boot.
    #[test]
    fn a_silent_rom_is_put_down_to_what_could_cause_it() {
        let bundles = ap64_core::builtin().unwrap();
        let profile = |id: &str| &bundles.iter().find(|b| b.profile.id == id).unwrap().profile;
        let (oot, cv64) = (profile("oot"), profile("cv64"));

        let (oot, cv64) = (Some(oot), Some(cv64));
        for cart in [Cart::Ed64, Cart::Ed64pro] {
            let on = Silence::of(oot, Some(cart));
            assert_eq!(on, Silence::Untested { cart, pak: false });
            let (detail, line) = (on.detail(), on.log_line());
            assert!(detail.contains("experimental") && detail.contains(cart.name()));
            assert!(detail.contains("patched for"));
            assert!(
                !detail.contains("Pak") && !line.contains("Pak"),
                "{detail} / {line}"
            );
            // A game that runs without the Pak may be missing it as well, which the log says.
            let on = Silence::of(cv64, Some(cart));
            assert_eq!(on, Silence::Untested { cart, pak: true });
            assert!(!on.detail().contains("Pak") && on.log_line().contains("Expansion Pak"));
        }

        // A daemon that does not say which cart is taken for one whose port means something.
        for cart in [Some(Cart::Sc64), None] {
            assert_eq!(Silence::of(cv64, cart), Silence::Pak);
            assert!(Silence::Pak.detail().contains("Expansion Pak"));
            assert_eq!(Silence::of(oot, cart), Silence::Other);
            assert!(!Silence::Other.detail().contains("Pak"));
        }

        // With no game chosen, either may be why, and an EverDrive may be missing the Pak too.
        assert_eq!(Silence::of(None, Some(Cart::Sc64)), Silence::Unknown);
        let detail = Silence::Unknown.detail();
        assert!(detail.contains("Expansion Pak") && detail.contains("added the agent"));
        assert_eq!(
            Silence::of(None, Some(Cart::Ed64)),
            Silence::Untested {
                cart: Cart::Ed64,
                pak: true
            }
        );
    }

    /// A cart ROM as AP64 leaves it for `profile`, as far as the session's checks look: the
    /// header, and each hook. `None` for a profile whose hooks are found per seed or which stores
    /// its files away from where the game sees them, which only a real seed can stand in for.
    fn cart_rom(profile: &Profile) -> Option<Vec<u8>> {
        if profile.transform.is_some() {
            return None;
        }
        let mut hooks = Vec::new();
        for w in &profile.write {
            match w {
                Write::Jal {
                    at: Addr::Rom(at),
                    target,
                    ..
                } => hooks.push((*at as usize, 0x0C00_0000 | ((target >> 2) & 0x03FF_FFFF))),
                Write::Jal { .. } => return None,
                _ => {}
            }
        }
        // A stub that is told where the agent is (`lui` + `addiu`) is read for the log; it is
        // told the agent's usual place.
        if let Some((Addr::Rom(hi), Some(Addr::Rom(lo)))) = profile.agent_rom_imm() {
            let rom = profile.agent.rom;
            let low = rom & 0xFFFF;
            let high = (rom >> 16) + u32::from(low >= 0x8000);
            hooks.push((*hi as usize, (0x0F << 26) | high));
            hooks.push((*lo as usize, (0x09 << 26) | low));
        }
        let len = hooks
            .iter()
            .map(|(at, _)| at + 4)
            .max()
            .unwrap_or(0)
            .max(0x1000);
        let mut rom = vec![0u8; len];
        rom[0x20..0x24].copy_from_slice(b"TEST");
        rom[0x3B..0x3F].copy_from_slice(profile.game_code.as_bytes());
        rom[0x3F] = profile.version;
        for (at, word) in hooks {
            rom[at..at + 4].copy_from_slice(&word.to_be_bytes());
        }
        Some(rom)
    }

    fn cart_with(rom: Vec<u8>) -> ap64_cart::backend::RamImage {
        ap64_cart::backend::RamImage::with_rom(vec![0; 0x1000], rom)
    }

    /// Banjo-Tooie's world and Star Fox 64's both rewrite the header to `NEDE` version `0x30`, so
    /// the header cannot say which is on the cart. Their hooks can.
    #[test]
    fn auto_detect_tells_two_games_with_one_header_apart() {
        let bundles = ap64_core::builtin().unwrap();
        let profile = |id: &str| &bundles.iter().find(|b| b.profile.id == id).unwrap().profile;
        let (bt, sf64) = (profile("bt"), profile("sf64"));
        assert_eq!(
            (&bt.game_code, bt.version),
            (&sf64.game_code, sf64.version),
            "the premise: one header for both"
        );
        for p in [bt, sf64] {
            let mut cart = cart_with(cart_rom(p).expect("both hook at fixed offsets"));
            let found = identify(&bundles, &mut cart).map(|(b, _)| b.profile.id.as_str());
            assert_eq!(found.ok(), Some(p.id.as_str()));
        }
    }

    /// Every game whose hooks sit at fixed offsets is found as itself, and as no other: a profile
    /// added later with another's header and hook would make Auto-detect refuse both.
    #[test]
    fn auto_detect_finds_every_game_it_can_stand_in_for() {
        let bundles = ap64_core::builtin().unwrap();
        let mut tried = 0;
        for b in &bundles {
            let Some(rom) = cart_rom(&b.profile) else {
                continue;
            };
            tried += 1;
            match identify(&bundles, &mut cart_with(rom)) {
                Ok((found, _)) => assert_eq!(found.profile.id, b.profile.id),
                Err(e) => panic!("{}: {e}", b.profile.id),
            }
        }
        assert!(tried >= 8, "only {tried} games could be stood in for");
    }

    /// A ROM that is no game AP64 knows, and one that is but has no hook, are each told for what
    /// they are.
    #[test]
    fn auto_detect_says_why_it_found_nothing() {
        let bundles = ap64_core::builtin().unwrap();
        let mut rom = vec![0u8; 0x1000];
        rom[0x3B..0x3F].copy_from_slice(b"NXXE");
        let e = identify(&bundles, &mut cart_with(rom)).err().unwrap();
        assert!(e.message.contains("does not support"), "{e}");
        assert!(e.technical.contains("NXXE"), "{e}");

        // Star Fox 64's header with neither game's hook: both are tried, and the ROM has no agent.
        let sf64 = &bundles
            .iter()
            .find(|b| b.profile.id == "sf64")
            .unwrap()
            .profile;
        let mut rom = cart_rom(sf64).unwrap();
        rom[0x4EDC..0x4EE0].fill(0);
        let e = identify(&bundles, &mut cart_with(rom)).err().unwrap();
        assert!(e.message.contains("has no AP64 agent"), "{e}");
    }

    /// A cart whose ROM can be changed under it, as a console reset does, with the link
    /// generation moved or not, as the backend does when it notices.
    struct Swappable {
        image: ap64_cart::backend::RamImage,
        generation: u32,
        /// The link restarts under every ROM read, as one that keeps failing would.
        restart_on_rom_read: bool,
        /// Every RAM read fails, with the link restarting under it, as a cart that has gone does.
        fail_reads: bool,
    }

    impl Swappable {
        fn new(rom: Vec<u8>) -> Self {
            Self {
                image: cart_with(rom),
                generation: 0,
                restart_on_rom_read: false,
                fail_reads: false,
            }
        }
    }

    impl Backend for Swappable {
        fn rdram_size(&self) -> u32 {
            self.image.rdram_size()
        }
        fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            if self.fail_reads {
                self.generation += 1;
                return Err(io::Error::other("the cart has gone"));
            }
            self.image.read_many(regions)
        }
        fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
            self.image.write_many(writes)
        }
        fn rom_window(&self) -> Option<u32> {
            self.image.rom_window()
        }
        fn read_rom_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            if self.restart_on_rom_read {
                self.generation += 1;
            }
            self.image.read_rom_many(regions)
        }
        fn generation(&self) -> u32 {
            self.generation
        }
        fn set_watch(&mut self, watch: ap64_cart::watch::Watch) {
            self.image.set_watch(watch)
        }
        fn take_watched(&mut self) -> Option<Vec<u8>> {
            self.image.take_watched()
        }
        fn sample_watch(&mut self) -> io::Result<()> {
            self.image.sample_watch()
        }
        fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
            self.image.watch_stats()
        }
    }

    /// The cart as an Auto-detect session serving `id` gives it to its connector, on a cart
    /// running `on` whose link restarted `generation` times since `id` was found.
    fn checked_cart(
        id: &str,
        on: &str,
        generation: u32,
    ) -> (
        Checked<Swappable>,
        Rc<RefCell<Swappable>>,
        Arc<AtomicBool>,
        Vec<Bundle>,
    ) {
        let bundles = ap64_core::builtin().unwrap();
        let rom = cart_rom(&bundles.iter().find(|b| b.profile.id == on).unwrap().profile);
        let mut cart = Swappable::new(rom.unwrap());
        cart.generation = generation;
        let cart = Rc::new(RefCell::new(cart));
        let serve = Arc::new(AtomicBool::new(false));
        let choice = Rc::new(Choice::Auto(bundles.clone()));
        let checked = Checked::new(cart.clone(), 0, same_game(choice, id.into()), serve.clone());
        (checked, cart, serve, bundles)
    }

    /// The console reset into another game: the request that finds it fails, as one the link
    /// restarted under does, nothing more is written, and the connector is told to stop.
    #[test]
    fn a_cart_found_running_another_game_is_not_served_as_the_old_one() {
        let (mut checked, cart, serve, bundles) = checked_cart("cv64", "cv64", 0);
        assert!(checked.read_many(&[(0x100, 4)]).is_ok());
        let mk64 = &bundles
            .iter()
            .find(|b| b.profile.id == "mk64")
            .unwrap()
            .profile;
        {
            let mut c = cart.borrow_mut();
            c.image.rom = cart_rom(mk64);
            c.generation += 1;
        }
        let e = checked.read_many(&[(0x100, 4)]).unwrap_err();
        assert!(ap64_cart::backend::link_restarted(&e), "{e}");
        assert!(e.to_string().contains(&mk64.name), "{e}");
        assert!(
            serve.load(Ordering::Relaxed),
            "the connector was not told to stop"
        );
        assert!(checked.changed().is_some());
        // And it stays refused: nothing reaches the other game's RAM.
        assert!(checked.write_many(&[(0x100, &[1, 2, 3, 4])]).is_err());
        assert_eq!(cart.borrow().image.ram[0x100..0x104], [0, 0, 0, 0]);
    }

    /// A link that restarted on the same game carries on, and is not asked again until it
    /// restarts again.
    #[test]
    fn a_link_that_restarted_on_the_same_game_carries_on() {
        let (mut checked, cart, serve, _) = checked_cart("cv64", "cv64", 0);
        cart.borrow_mut().generation += 1;
        assert!(checked.read_many(&[(0x100, 4)]).is_ok());
        assert!(checked.write_many(&[(0x100, &[1, 2, 3, 4])]).is_ok());
        assert_eq!(cart.borrow().image.ram[0x100..0x104], [1, 2, 3, 4]);
        assert!(!serve.load(Ordering::Relaxed));
        assert!(checked.changed().is_none());
    }

    /// The generation the game was found on is what is checked against, so a link that
    /// restarted between finding the game and building its connector is asked about too.
    #[test]
    fn a_link_restarted_since_the_game_was_found_is_checked() {
        let (checked, _, serve, _) = checked_cart("cv64", "mk64", 1);
        assert!(checked.check().is_err());
        assert!(serve.load(Ordering::Relaxed));
    }

    /// A link that restarts under every check is given up on, and the connector is told to stop
    /// as it is for another game: refused requests alone would only drop the client, which
    /// connects again to be refused again.
    #[test]
    fn a_link_that_keeps_restarting_under_the_check_stops_the_connector() {
        let (checked, cart, serve, _) = checked_cart("cv64", "cv64", 1);
        cart.borrow_mut().restart_on_rom_read = true;
        let e = checked.check().unwrap_err();
        assert!(e.to_string().contains("kept restarting"), "{e}");
        assert!(serve.load(Ordering::Relaxed));
    }

    /// A request that fails is reported as what failed it, and the cart is not asked about its
    /// game on the way out, though its link restarted: asking a cart that has just failed would
    /// only wait on it again, and its answer would be told in place of the failure.
    #[test]
    fn a_failed_request_is_not_checked_after() {
        let (mut checked, cart, serve, bundles) = checked_cart("cv64", "cv64", 0);
        {
            let mk64 = &bundles.iter().find(|b| b.profile.id == "mk64").unwrap();
            let mut c = cart.borrow_mut();
            c.image.rom = cart_rom(&mk64.profile);
            c.fail_reads = true;
        }
        let e = checked.read_many(&[(0x100, 4)]).unwrap_err();
        assert_eq!(e.to_string(), "the cart has gone");
        assert!(checked.changed().is_none());
        assert!(!serve.load(Ordering::Relaxed));
    }

    /// A chosen game is checked for and nothing else: the other game with its header fails.
    #[test]
    fn a_chosen_game_is_not_auto_detected() {
        let bundles = ap64_core::builtin().unwrap();
        let bundle = |id: &str| bundles.iter().find(|b| b.profile.id == id).unwrap().clone();
        let rom = cart_rom(&bundle("sf64").profile).unwrap();
        assert!(Choice::Game(Box::new(bundle("sf64")))
            .find(&mut cart_with(rom.clone()))
            .is_ok());
        assert!(Choice::Game(Box::new(bundle("bt")))
            .find(&mut cart_with(rom))
            .is_err());
    }

    /// A game an Auto-detect session found whose world is not installed is held, not served,
    /// and the AP client row says why.
    #[test]
    fn a_found_game_whose_client_is_missing_is_held() {
        let bundles = ap64_core::builtin().unwrap();
        let dk64 = &bundles
            .iter()
            .find(|b| b.profile.id == "dk64")
            .unwrap()
            .profile;
        let native = Native {
            client_fix: Some(TEXT_FIX),
            ..ap64_connector::DK64
        };
        let hold = client_ready(dk64, Kind::Native(native)).err().unwrap();
        assert_eq!(hold.client, NOT_INSTALLED);
        assert!(hold.detail.contains("Donkey Kong 64") && hold.detail.contains("w.apworld"));
        assert_eq!(hold.setup.unwrap().state, "missing");
        // A client with nothing to fix is served at once.
        let plain = Native {
            client_fix: None,
            ..ap64_connector::DK64
        };
        assert!(matches!(client_ready(dk64, Kind::Native(plain)), Ok(None)));
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

    /// A client fix over files that say what they are: `ready`, `needs` (which the fix turns
    /// into `ready`), or anything else, which it does not know.
    const TEXT_FIX: ap64_connector::ClientFix = ap64_connector::ClientFix {
        file: "w.apworld",
        why: "It cannot reach AP64",
        find: Vec::new,
        state: |b| match b {
            b"ready" => ap64_connector::ClientState::Ready,
            b"needs" => ap64_connector::ClientState::NeedsFix,
            _ => ap64_connector::ClientState::Unrecognized("odd".into()),
        },
        apply: |b| match b {
            b"needs" => Ok(b"ready".to_vec()),
            _ => Err("not one this fixes".into()),
        },
    };

    /// Two Archipelago installs in a temporary folder, each with a copy of the world.
    fn two_installs(name: &str, a: &[u8], b: &[u8]) -> (PathBuf, [PathBuf; 2]) {
        let dir = std::env::temp_dir().join(format!("ap64-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = [
            dir.join("ProgramData").join("custom_worlds"),
            dir.join("LocalAppData").join("lib").join("worlds"),
        ]
        .map(|d| {
            std::fs::create_dir_all(&d).unwrap();
            d.join("w.apworld")
        });
        std::fs::write(&paths[0], a).unwrap();
        std::fs::write(&paths[1], b).unwrap();
        (dir, paths)
    }

    /// A copy the fix will not change, beside one it will, is told to the player in plain
    /// words, not only in Developer details: it is left as it is, and why.
    #[test]
    fn a_copy_the_fix_leaves_alone_is_told_to_the_player() {
        let (dir, paths) = two_installs("client-left", b"needs", b"something else");
        let s = setup_of(&TEXT_FIX, &paths);
        assert_eq!(s.state, "needed");
        assert_eq!(s.path, paths[0].display().to_string());
        assert_eq!(
            s.left,
            "One other copy of w.apworld is a version AP64 does not recognize, so AP64 will \
             leave it as it is. If the AP client runs that copy, it may not connect"
        );
        assert!(s.technical.contains("odd"), "{}", s.technical);

        // Two copies AP64 does not know, beside one it fixes.
        let third = dir.join("Other").join("custom_worlds").join("w.apworld");
        std::fs::create_dir_all(third.parent().unwrap()).unwrap();
        std::fs::write(&third, b"another").unwrap();
        let all = [paths[0].clone(), paths[1].clone(), third];
        let s = setup_of(&TEXT_FIX, &all);
        assert!(
            s.left.starts_with(
                "2 other copies of w.apworld are versions AP64 does not recognize, so AP64 will \
                 leave them as they are"
            ),
            "{}",
            s.left
        );

        // Nothing is left out when every other copy is ready.
        std::fs::write(&paths[1], b"ready").unwrap();
        assert_eq!(setup_of(&TEXT_FIX, &paths).left, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Either install may be the one the player opens, so one copy that is ready is not enough.
    #[test]
    fn a_client_is_ready_only_when_every_installed_copy_is() {
        let (dir, paths) = two_installs("client-setup", b"ready", b"needs");
        let s = setup_of(&TEXT_FIX, &paths);
        assert_eq!(s.state, "needed");
        assert_eq!(s.path, paths[1].display().to_string());
        assert!(s
            .message
            .ends_with("add it to w.apworld and keep the original"));

        std::fs::write(&paths[1], b"ready").unwrap();
        assert_eq!(setup_of(&TEXT_FIX, &paths).state, "ready");
        std::fs::write(&paths[1], b"something else").unwrap();
        let s = setup_of(&TEXT_FIX, &paths);
        assert_eq!(
            (s.state.as_str(), s.path),
            ("unknown", paths[1].display().to_string())
        );
        assert_eq!(setup_of(&TEXT_FIX, &[]).state, "missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fix_is_made_to_every_copy_that_needs_it_each_backed_up() {
        let (dir, paths) = two_installs("client-fix", b"needs", b"needs");
        let s = setup_of(&TEXT_FIX, &paths);
        assert!(
            s.message.contains("each of the 2 copies of w.apworld"),
            "{}",
            s.message
        );
        let backups = dir.join("backups");
        let told = fix_copies(&TEXT_FIX, &paths, &backups).unwrap();
        assert!(told.starts_with("Fixed all 2 copies"), "{told}");
        for p in &paths {
            assert_eq!(std::fs::read(p).unwrap(), b"ready");
        }
        let mut saved: Vec<Vec<u8>> = std::fs::read_dir(&backups)
            .unwrap()
            .map(|e| std::fs::read(e.unwrap().path()).unwrap())
            .collect();
        saved.sort();
        assert_eq!(saved, [b"needs".to_vec(), b"needs".to_vec()]);
        assert_eq!(setup_of(&TEXT_FIX, &paths).state, "ready");
        assert!(
            fix_copies(&TEXT_FIX, &paths, &backups).is_err(),
            "nothing left to fix"
        );

        // A copy that is ready, or that the fix does not know, is left as it is.
        std::fs::write(&paths[0], b"needs").unwrap();
        std::fs::write(&paths[1], b"something else").unwrap();
        fix_copies(&TEXT_FIX, &paths, &backups).unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"ready");
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"something else");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A copy AP64 cannot read must not stop it fixing the ones it can: the readable copy is
    /// fixed and backed up, the unreadable one is left alone, and the player is told which.
    #[test]
    fn a_copy_that_cannot_be_read_is_skipped_not_fatal() {
        let (dir, mut paths) = two_installs("client-fix-unread", b"needs", b"needs");
        // A path that names nothing reads as an error, like a locked or vanished file.
        let gone = dir.join("gone").join("w.apworld");
        paths[1] = gone.clone();
        let backups = dir.join("backups");
        let told = fix_copies(&TEXT_FIX, &paths, &backups).unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"ready");
        assert!(told.starts_with("Fixed."), "{told}");
        assert!(
            told.contains("could not read") && told.contains(&gone.display().to_string()),
            "the skipped copy is named: {told}"
        );
        assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 1);

        // With nothing readable that needs it, the reason says so instead of "nothing to fix".
        let err = fix_copies(&TEXT_FIX, &[gone], &backups).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
