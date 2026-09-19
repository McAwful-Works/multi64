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

use std::cell::RefCell;
use std::io;
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

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// idle | connecting | waiting-client | playing | stopped | failed
    pub state: String,
    pub detail: String,
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

#[derive(Default)]
pub struct Play {
    session: Mutex<Option<Session>>,
    status: Arc<Mutex<Status>>,
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
fn verify_cart(bundle: &Bundle, cart: &mut Multi64) -> Result<String, String> {
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
        .map_err(|e| e.to_string())?;
    let h = rom_fmt::header(&got[0]).ok_or("the cart ROM header could not be read")?;
    if h.game_code != p.game_code || h.version != p.version {
        return Err(format!(
            "the cart is running {} [{} v{}], not {} {} [{} v{}]",
            if h.name.is_empty() { "a ROM" } else { &h.name },
            h.game_code,
            h.version,
            p.name,
            p.release,
            p.game_code,
            p.version
        ));
    }
    let without = |why: String| {
        format!(
            "the cart is running {} without AP64's agent ({why}); add the agent to the seed and \
             load that ROM",
            p.name
        )
    };
    let table = match &p.transform {
        None => None,
        Some(Transform::Yaz0Dmadata { table }) => Some(cart_table(cart, *table)?),
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
    let words = cart.read_rom_many(&regions).map_err(|e| e.to_string())?;
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
                .spawn(move || run(app, game, script, url, stop, status))
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

/// How often counters are pushed to the page while playing.
const STATS_EVERY: Duration = Duration::from_secs(1);
/// Between attempts to reach a cart that is not answering yet.
const CART_RETRY: Duration = Duration::from_secs(2);

fn run(
    app: AppHandle,
    game: Bundle,
    script: Script,
    url: String,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
) {
    let log: Log = {
        let app = app.clone();
        Arc::new(move |line: String| {
            let _ = app.emit("play://log", line);
        })
    };
    let set = |f: &dyn Fn(&mut Status)| {
        let mut s = status.lock().unwrap();
        f(&mut s);
        let _ = app.emit("play://status", s.clone());
    };

    // Wait for the cart: the console may not be on yet, or the daemon not started.
    let cart = loop {
        if stop.load(Ordering::Relaxed) {
            set(&|s| {
                s.state = "stopped".into();
                s.detail = String::new();
            });
            return;
        }
        match Multi64::connect(&url, log.clone()) {
            Ok(c) => break c,
            Err(e) => {
                let msg = e.to_string();
                set(&|s| {
                    s.state = "connecting".into();
                    s.detail = format!("waiting for the cart: {msg}");
                });
                let until = Instant::now() + CART_RETRY;
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    };
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
        Ok(name) => log(format!(
            "the cart is running {} ({}) with the agent",
            game.profile.name,
            if name.is_empty() {
                "no internal name"
            } else {
                &name
            }
        )),
        Err(e) => {
            log(e.clone());
            set(&|s| {
                s.state = "failed".into();
                s.detail = e.clone();
            });
            return;
        }
    }

    let cart = Rc::new(RefCell::new(cart));
    let connector = match Connector::new(&script, Box::new(SharedCart(cart.clone())), log.clone()) {
        Ok(c) => c,
        Err(e) => {
            set(&|s| {
                s.state = "failed".into();
                s.detail = e.clone();
            });
            return;
        }
    };

    let ports = script.ports;
    let mut last_stats = Instant::now();
    let mut handled = 0u64;
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
            });
        }
        Event::ClientConnected(addr) => {
            log(format!("{} connected from {addr}", script.client));
            set(&|s| {
                s.state = "playing".into();
                s.detail = format!("{} connected", script.client);
            });
        }
        Event::ClientDisconnected(why) => {
            log(format!("{} disconnected: {why}", script.client));
            set(&|s| {
                s.state = "waiting-client".into();
                s.detail = format!("{} disconnected ({why}); waiting for it", script.client);
            });
        }
        Event::Handled => {
            handled += 1;
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
            });
        }
        Err(e) => {
            log(format!("session ended: {e}"));
            set(&|s| {
                s.state = "failed".into();
                s.detail = e.clone();
            });
        }
    }
}
