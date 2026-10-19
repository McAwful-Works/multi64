//! Run an Archipelago connector against a cart.
//!
//! The scripts in `connectors/` keep the protocol their Archipelago client speaks and call
//! [`ap64`](#the-ap64-table) for memory. Most are Archipelago's own connectors with the
//! emulator taken out; `pj64` is AP64's own, written against the Project64 script it stands
//! in for. AP64 owns the TCP socket the client connects to ([`server`]) and hands each line to
//! the script.
//!
//! A client with no connector script to fork or stand in for is answered by AP64 directly
//! instead: a [`Native`] connector. One that reads emulator memory itself gets RetroArch's Network
//! Commands answered from the cart ([`retroarch`]): Donkey Kong 64 ([`dk64`]) and Banjo-Tooie
//! ([`bt`]). One whose script only relays packets between the client and buffers in the
//! game's RAM, the ROM doing all of the game's work, gets that relay ([`relay`]): Star Fox 64.
//!
//! A client that talks to a Project64 memory-bridge script is a script connector too: AP64
//! answers that script's line protocol in its place ([`PJ64`]), on the port the client keeps in
//! Project64's settings ([`pj64`]): Pokémon Snap.
//!
//! # The `ap64` table
//!
//! | Function | |
//! |---|---|
//! | `read_many({{addr, len}, ...})` | RDRAM blocks as strings, one call, in order |
//! | `write_many({{addr, str}, ...})` | RDRAM writes, one call |
//! | `rdram_size()` | bytes of RDRAM the cart reports |
//! | `rom_size()`, `rom_read(addr, len)` | the cartridge ROM, read from the cart and cached |
//! | `rom_read_many({{addr, len}, ...})` | several ROM regions in one call, to fetch a batch's at once |
//! | `rom_hash()` | a stable identity for the running ROM: SHA-1 of its first 4 KiB |
//! | `watch(addr, len[, at, values])` | follow a slot the game rewrites between polls |
//! | `take_watched()` | the oldest change to it not yet shown, or `nil` for live memory |
//! | `b64encode(str)`, `b64decode(str)` | base64, which the protocols carry bytes in |
//! | `log(str)`, `message(str)` | a line for AP64's log; a message meant for the player |
//!
//! A cart failure (the backend has already retried and reconnected, and given up) is
//! fatal: it ends the session, even if the script caught the error. Any other error the
//! script raises fails only the line it was handling ([`Unanswered`]).
//!
//! A link that restarted is not a failure, but nothing decided before it is written after it:
//! the console may have been reset in between. A line during which the link restarted has its
//! writes refused, and the script is loaded again before the next. The [`retroarch`] snapshot
//! and the [`relay`] start over the same way.

pub mod bt;
pub mod dk64;
pub mod pj64;
pub mod relay;
pub mod retroarch;
pub mod server;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use ap64_cart::backend::{link_restarted, Backend};
use ap64_cart::watch;
use ap64_cart::Log;
use base64::Engine as _;
use mlua::{Lua, Table};
use sha1::{Digest, Sha1};

/// Finds the ports a client will connect to on this PC, or says what the player must do first.
pub type FindPorts = fn() -> Result<Vec<u16>, String>;

/// A connector script shipped with AP64.
#[derive(Debug, Clone, Copy)]
pub struct Script {
    pub id: &'static str,
    /// What it serves, for the user.
    pub name: &'static str,
    /// The Archipelago client to run alongside it.
    pub client: &'static str,
    pub source: &'static str,
    /// Where the client looks for it: the first free one is bound.
    pub ports: &'static [u16],
    /// Where the client looks for it when that is not fixed but set up on the player's PC, in
    /// place of `ports`; an error says what the player has to do first ([`Script::listen_on`]).
    pub find_ports: Option<FindPorts>,
    /// Lua modules it may `require`, beside `json` and `bit`: (name, source).
    pub modules: &'static [(&'static str, &'static str)],
    /// How long the client may stay silent after a reply before it is dropped, as the
    /// upstream script does; `None` if upstream never drops it.
    pub client_timeout: Option<Duration>,
}

/// For games whose Archipelago client is the generic BizHawk Client.
pub const GENERIC: Script = Script {
    id: "generic",
    name: "Generic (BizHawk Client games)",
    client: "BizHawk Client",
    source: include_str!("../connectors/generic/connector.lua"),
    ports: &[43055, 43056, 43057, 43058, 43059, 43060],
    find_ports: None,
    modules: &[],
    // Was Some(5s), on the reasoning that "BizHawk Client pings well inside it". True of a
    // client attached to an emulator, where a read returns in microseconds. False over a
    // cart: BizHawk Client is request/response, so while it waits for OUR reply it sends
    // nothing, and `last_heard` ages by however long the round trip took.
    //
    // The check in server::serve_until runs AFTER handle() returns, so a reply that took
    // longer than the timeout was answered and then the client evicted for the silence we
    // ourselves caused. Observed on a console: Castlevania 64 dropped the client roughly
    // every few minutes, always recovering about a second later, never losing a check --
    // the signature of eviction rather than a fault.
    //
    // Nothing needed this. A timeout's job is to free the slot for a new client, and
    // accept_newest() already replaces the old one the moment a new connection arrives.
    // oot has always been None, as was Banjo-Tooie's script before it went native, and
    // neither showed the behavior.
    client_timeout: None,
};

/// Ocarina of Time, for Archipelago's OoT Client.
pub const OOT: Script = Script {
    id: "oot",
    name: "Ocarina of Time",
    client: "OoT Client",
    source: include_str!("../connectors/oot/connector.lua"),
    ports: &[28921],
    find_ports: None,
    modules: &[("cartmem", include_str!("../connectors/oot/cartmem.lua"))],
    // Upstream's script never drops OoT Client, so neither do we. It would survive it
    // now -- every connection this server lets go of is reset rather than closed, which
    // is the one ending its socket task catches (see `server::Client`) -- but there is
    // still nothing to gain by dropping a client upstream would have kept.
    client_timeout: None,
};

/// For a client that talks to a Project64 memory-bridge script: Pokémon Snap's, whose world's
/// ROM does all of the game's work and whose script only reads and writes RAM. Its client
/// connects to a port it picked and keeps in Project64's settings file ([`pj64`]).
pub const PJ64: Script = Script {
    id: "pj64",
    name: "Project64 memory bridge (Pokémon Snap)",
    client: "Pokemon Snap Client",
    source: include_str!("../connectors/pj64/connector.lua"),
    ports: &[],
    find_ports: Some(pj64::snap_ports),
    modules: &[],
    // Upstream's script never drops its client: Project64's server keeps a connection until
    // the client closes it.
    client_timeout: None,
};

pub const SCRIPTS: &[Script] = &[GENERIC, OOT, PJ64];

impl Script {
    /// The ports to listen on: `ports`, or what `find_ports` finds on this PC.
    pub fn listen_on(&self) -> Result<Vec<u16>, String> {
        match self.find_ports {
            Some(find) => find(),
            None => Ok(self.ports.to_vec()),
        }
    }
}

/// A connector that is not a script: AP64 answers the client itself, from the cart.
#[derive(Debug, Clone, Copy)]
pub struct Native {
    pub id: &'static str,
    /// What it serves, for the user.
    pub name: &'static str,
    /// The Archipelago client to run alongside it.
    pub client: &'static str,
    pub protocol: Protocol,
    /// A change the installed client needs before it can reach AP64, if any.
    pub client_fix: Option<ClientFix>,
}

/// How a [`Native`] connector reaches its client.
#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    /// AP64 answers RetroArch's Network Commands over UDP, and the client asks.
    RetroArch(retroarch::Options),
    /// AP64 connects to the client over TCP and relays packets to and from the game's RAM.
    Relay(relay::Options),
}

/// Whether an installed Archipelago client can reach AP64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientState {
    Ready,
    /// It cannot yet, and [`ClientFix::apply`] knows how to change that.
    NeedsFix,
    /// Not the version the fix was written against; the reason says what was missing.
    Unrecognized(String),
}

/// A change made to an installed Archipelago client's own files, because as released it
/// cannot reach AP64. Only ever applied when asked, and only to a file [`ClientFix::state`]
/// reports as [`ClientState::NeedsFix`].
#[derive(Debug, Clone, Copy)]
pub struct ClientFix {
    /// The installed file the change is made in.
    pub file: &'static str,
    /// Why the client cannot reach AP64 as released, in a clause for the player.
    pub why: &'static str,
    /// Every installed copy of that file: none when it is not installed, and more than one
    /// when more than one Archipelago install has it, any of which may be the one that runs
    /// ([`ap64_core::installed::installed_apworlds`]).
    pub find: fn() -> Vec<std::path::PathBuf>,
    pub state: fn(&[u8]) -> ClientState,
    /// The file's new contents; an error if it is not one this can fix.
    pub apply: fn(&[u8]) -> Result<Vec<u8>, String>,
}

/// Donkey Kong 64's client, answered as RetroArch ([`dk64`]).
pub const DK64: Native = Native {
    id: "dk64",
    name: "Donkey Kong 64 (RetroArch Network Commands)",
    client: "DK64 Client",
    protocol: Protocol::RetroArch(dk64::OPTIONS),
    client_fix: Some(ClientFix {
        file: dk64::APWORLD,
        why: "Its copy of EmuLoader is missing a method it calls on every loop, so it cannot reach AP64",
        find: dk64::installed_apworlds,
        state: dk64::client_state,
        apply: dk64::fix_client,
    }),
};

/// Banjo-Tooie's client, answered as RetroArch ([`bt`]).
pub const BT: Native = Native {
    id: "bt",
    name: "Banjo-Tooie (RetroArch Network Commands)",
    client: "Banjo-Tooie Client",
    protocol: Protocol::RetroArch(bt::OPTIONS),
    client_fix: Some(ClientFix {
        file: bt::APWORLD,
        why: "Its copy of EmuLoader stops it as soon as it attaches to anything but an emulator, so it cannot reach AP64",
        find: bt::installed_apworlds,
        state: bt::client_state,
        apply: bt::fix_client,
    }),
};

/// Star Fox 64's client, relayed to ([`relay`]). Its world's ROM keeps the addresses of its
/// two packet buffers at 0x80400000, and its client listens on port 0x5F64 (24420). Each
/// buffer is 512 bytes, header included (`ap_packet_t`, `PACKET_SIZE` 508 in its `ap.h`); the
/// client splits what it sends to fit, and anything larger is refused rather than written
/// past the input buffer into the output one.
pub const SF64: Native = Native {
    id: "sf64",
    name: "Star Fox 64 (relayed)",
    client: "Star Fox 64 Client",
    protocol: Protocol::Relay(relay::Options {
        port: 0x5F64,
        pointers: 0x8040_0000,
        max_packet: 512,
    }),
    client_fix: None,
};

pub const NATIVES: &[Native] = &[DK64, BT, SF64];

pub use ap64_core::installed::{installed_apworld, installed_apworlds};

const LIBS: &[(&str, &str)] = &[
    ("json", include_str!("../connectors/lib/json.lua")),
    ("bit", include_str!("../connectors/lib/bit.lua")),
];

/// Bytes of ROM the hash covers: the header (with the boot checksum, which covers the
/// first megabyte of code) and the boot code. One request, and cached like any ROM read.
const HASH_BYTES: usize = 0x1000;

type SharedBackend = Rc<RefCell<Box<dyn Backend>>>;

/// One script, loaded against one cart. Not `Send`: build it on the thread that runs it.
///
/// Scripts read state at load (OoT's finds its context pointers then). When the link to
/// the cart restarts, the console may have been reset or be running another image, so
/// the script is loaded again before the next line.
pub struct Connector {
    script: Script,
    backend: SharedBackend,
    log: Log,
    lua: RefCell<Lua>,
    loaded_at: RefCell<u32>,
    fatal: Rc<RefCell<Option<String>>>,
    /// The link generation the line being handled began on. A write the script makes once
    /// the link has restarted since is refused: it was decided from reads of the console as it
    /// was, and the console may have been reset in between.
    batch: Rc<Cell<u32>>,
}

impl Connector {
    pub fn new(script: &Script, backend: Box<dyn Backend>, log: Log) -> Result<Self, String> {
        if backend.rom_window().is_none() {
            return Err(
                "the cart agent cannot read the cart ROM (it predates PEEKROM): add \
                        the agent to the seed again with this version of AP64"
                    .into(),
            );
        }
        let generation = backend.generation();
        let backend: SharedBackend = Rc::new(RefCell::new(backend));
        let fatal = Rc::new(RefCell::new(None));
        let batch = Rc::new(Cell::new(generation));
        let lua = load(script, &backend, &log, &fatal, &batch)?;
        Ok(Self {
            script: *script,
            backend,
            log,
            lua: RefCell::new(lua),
            loaded_at: RefCell::new(generation),
            fatal,
            batch,
        })
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    /// Read the watched slot, if the script asked for one ([`ap64_cart::watch`]).
    ///
    /// For time the session would spend idle: a slot the game rewrites between polls is
    /// missed by exactly as much as it goes unread, and a connector waiting on its client
    /// is not using the cart for anything else. Does nothing when no watch is set.
    pub fn sample_watch(&self) -> Result<(), String> {
        if self.backend.borrow().watch_stats().is_none() {
            return Ok(());
        }
        self.backend
            .borrow_mut()
            .sample_watch()
            .map_err(|e| format!("cart read: {e}"))
    }

    /// What the watch has seen and handed over, if one is set.
    pub fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
        self.backend.borrow().watch_stats()
    }

    /// One line from the client in, the line to send back out (no newline).
    ///
    /// [`Unanswered::Line`] when only this line failed: a script error, a line that is not
    /// what the script expects, a bad address, a write the link restarted under. The cart is
    /// fine, and the next line is answered as usual. [`Unanswered::Fatal`] when the session is
    /// over: the cart has failed, or the script cannot be run against it.
    pub fn handle(&self, message: &str) -> Result<String, Unanswered> {
        let generation = self.backend.borrow().generation();
        if generation != *self.loaded_at.borrow() {
            (self.log)(format!(
                "the cart link restarted: loading the {} connector again",
                self.script.id
            ));
            *self.lua.borrow_mut() = load(
                &self.script,
                &self.backend,
                &self.log,
                &self.fatal,
                &self.batch,
            )
            .map_err(Unanswered::Fatal)?;
            *self.loaded_at.borrow_mut() = generation;
        }
        self.batch.set(generation);
        let lua = self.lua.borrow();
        let handle: mlua::Function = lua
            .globals()
            .get("handle")
            .map_err(|_| Unanswered::Fatal("the connector script defines no handle()".into()))?;
        let result = handle.call::<String>(message);
        if let Some(e) = self.fatal.borrow_mut().take() {
            return Err(Unanswered::Fatal(e));
        }
        result.map_err(|e| Unanswered::Line(format!("connector: {e}")))
    }
}

/// Why [`Connector::handle`] has no line to send back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unanswered {
    /// The session is over: the cart failed (the backend retried, reconnected and gave up),
    /// or the script cannot be loaded or run against it.
    Fatal(String),
    /// This line, and only this line, could not be answered. Not the cart's failure, so not
    /// the session's: an Archipelago client that sends a malformed line, or a script that
    /// raises on one, would otherwise cost a full reconnect to the cart.
    Line(String),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unanswered::Fatal(e) | Unanswered::Line(e) => f.write_str(e),
        }
    }
}

fn load(
    script: &Script,
    backend: &SharedBackend,
    log: &Log,
    fatal: &Rc<RefCell<Option<String>>>,
    batch: &Rc<Cell<u32>>,
) -> Result<Lua, String> {
    let lua = Lua::new();
    install(
        &lua,
        script,
        backend.clone(),
        log.clone(),
        fatal.clone(),
        batch.clone(),
    )
    .map_err(|e| e.to_string())?;
    let loaded = lua
        .load(script.source)
        .set_name(format!("connectors/{}", script.id))
        .exec();
    if let Some(e) = fatal.borrow_mut().take() {
        return Err(e);
    }
    loaded.map_err(|e| format!("loading the {} connector: {e}", script.id))?;
    Ok(lua)
}

fn install(
    lua: &Lua,
    script: &Script,
    backend: SharedBackend,
    log: Log,
    fatal: Rc<RefCell<Option<String>>>,
    batch: Rc<Cell<u32>>,
) -> mlua::Result<()> {
    let ap64 = lua.create_table()?;

    // A cart failure is remembered as well as raised: scripts catch errors per request
    // and would otherwise answer a dead cart with an endless stream of ERRORs. A bad
    // address is the request's fault, not the cart's, and only fails that request. So
    // does a write the link restarted under: the cart is back, and the next request is
    // answered from the console as it now is.
    let cart_err = {
        let fatal = fatal.clone();
        move |e: std::io::Error| {
            let msg = e.to_string();
            if e.kind() != std::io::ErrorKind::InvalidInput && !link_restarted(&e) {
                fatal.borrow_mut().get_or_insert(msg.clone());
            }
            mlua::Error::RuntimeError(msg)
        }
    };

    let size = backend.borrow().rdram_size();
    ap64.set("rdram_size", lua.create_function(move |_, ()| Ok(size))?)?;

    {
        let backend = backend.clone();
        let cart_err = cart_err.clone();
        ap64.set(
            "read_many",
            lua.create_function(move |lua, list: Table| {
                let mut regions = Vec::new();
                for r in list.sequence_values::<Table>() {
                    let r = r?;
                    regions.push((r.get::<u32>(1)?, r.get::<usize>(2)?));
                }
                let blocks = backend
                    .borrow_mut()
                    .read_many(&regions)
                    .map_err(cart_err.clone())?;
                let out = lua.create_table_with_capacity(blocks.len(), 0)?;
                for b in blocks {
                    out.push(lua.create_string(&b)?)?;
                }
                Ok(out)
            })?,
        )?;
    }

    {
        let backend = backend.clone();
        let cart_err = cart_err.clone();
        ap64.set(
            "write_many",
            lua.create_function(move |_, list: Table| {
                let mut owned: Vec<(u32, Vec<u8>)> = Vec::new();
                for w in list.sequence_values::<Table>() {
                    let w = w?;
                    let data: mlua::LuaString = w.get(2)?;
                    owned.push((w.get::<u32>(1)?, data.as_bytes().to_vec()));
                }
                let writes: Vec<(u32, &[u8])> =
                    owned.iter().map(|(a, d)| (*a, d.as_slice())).collect();
                if backend.borrow().generation() != batch.get() {
                    // The link restarted during this line, under a read: what the script
                    // decided to write came from the console before it. Not fatal, since the
                    // cart is back; the next line is loaded and answered afresh.
                    return Err(mlua::Error::RuntimeError(
                        "the cart link restarted during this request, so its writes were not \
                         sent: the console may have been reset since it was read"
                            .into(),
                    ));
                }
                backend
                    .borrow_mut()
                    .write_many(&writes)
                    .map_err(cart_err.clone())
            })?,
        )?;
    }

    {
        // A script asks for this where the game gives it no better option: one slot
        // holding the most recent event, overwritten by the next. `at`/`values` name a
        // byte the script would act on, so changes it could never match stay out of the
        // queue. See ap64_cart::watch for why replaying them is sound.
        let backend = backend.clone();
        ap64.set(
            "watch",
            lua.create_function(
                move |_, (addr, len, at, values): (u32, usize, Option<usize>, Option<Table>)| {
                    let filter = match (at, values) {
                        (Some(at), Some(values)) => Some(watch::Filter {
                            at,
                            values: values.sequence_values::<u8>().collect::<Result<_, _>>()?,
                        }),
                        _ => None,
                    };
                    backend
                        .borrow_mut()
                        .set_watch(watch::Watch::new(addr, len, filter));
                    Ok(())
                },
            )?,
        )?;
    }
    {
        let backend = backend.clone();
        ap64.set(
            "take_watched",
            lua.create_function(move |lua, ()| {
                match backend.borrow_mut().take_watched() {
                    Some(bytes) => Ok(mlua::Value::String(lua.create_string(bytes)?)),
                    // Nothing waiting: the caller reads live memory, as it always did.
                    None => Ok(mlua::Value::Nil),
                }
            })?,
        )?;
    }

    let window = backend.borrow().rom_window().unwrap_or(0);
    ap64.set("rom_size", lua.create_function(move |_, ()| Ok(window))?)?;

    // Reads a list of {addr, len} from the cart ROM (cached in the backend).
    let rom_regions = |list: &Table| -> mlua::Result<Vec<(u32, usize)>> {
        let mut regions = Vec::new();
        for r in list.clone().sequence_values::<Table>() {
            let r = r?;
            regions.push((r.get::<u32>(1)?, r.get::<usize>(2)?));
        }
        Ok(regions)
    };
    {
        let backend = backend.clone();
        let cart_err = cart_err.clone();
        ap64.set(
            "rom_read_many",
            lua.create_function(move |lua, list: Table| {
                let blocks = backend
                    .borrow_mut()
                    .read_rom_many(&rom_regions(&list)?)
                    .map_err(cart_err.clone())?;
                let out = lua.create_table_with_capacity(blocks.len(), 0)?;
                for b in blocks {
                    out.push(lua.create_string(&b)?)?;
                }
                Ok(out)
            })?,
        )?;
    }
    {
        let backend = backend.clone();
        let cart_err = cart_err.clone();
        ap64.set(
            "rom_read",
            lua.create_function(move |lua, (addr, len): (u32, usize)| {
                let mut blocks = backend
                    .borrow_mut()
                    .read_rom_many(&[(addr, len)])
                    .map_err(cart_err.clone())?;
                lua.create_string(blocks.pop().unwrap_or_default())
            })?,
        )?;
    }
    {
        // Recomputed only when the link restarted, since that is when the ROM may differ.
        let backend = backend.clone();
        let cart_err = cart_err.clone();
        let cached: RefCell<Option<(u32, String)>> = RefCell::new(None);
        ap64.set(
            "rom_hash",
            lua.create_function(move |_, ()| {
                let generation = backend.borrow().generation();
                if let Some((g, h)) = cached.borrow().as_ref() {
                    if *g == generation {
                        return Ok(h.clone());
                    }
                }
                let head = backend
                    .borrow_mut()
                    .read_rom_many(&[(0, HASH_BYTES)])
                    .map_err(cart_err.clone())?;
                let hash: String = Sha1::digest(&head[0])
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                *cached.borrow_mut() = Some((generation, hash.clone()));
                Ok(hash)
            })?,
        )?;
    }

    ap64.set(
        "b64encode",
        lua.create_function(|_, s: mlua::LuaString| {
            Ok(base64::engine::general_purpose::STANDARD.encode(s.as_bytes()))
        })?,
    )?;
    ap64.set(
        "b64decode",
        lua.create_function(|lua, s: mlua::LuaString| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes().trim_ascii())
                .map_err(|e| mlua::Error::RuntimeError(format!("bad base64: {e}")))?;
            lua.create_string(bytes)
        })?,
    )?;

    {
        let log = log.clone();
        ap64.set(
            "log",
            lua.create_function(move |_, s: String| {
                log(s);
                Ok(())
            })?,
        )?;
    }
    {
        let log = log.clone();
        ap64.set(
            "message",
            lua.create_function(move |_, s: String| {
                log(format!("Archipelago: {s}"));
                Ok(())
            })?,
        )?;
    }
    // A stray print goes to the log too, rather than to a console the app does not have.
    lua.globals().set(
        "print",
        lua.create_function(move |_, args: mlua::Variadic<mlua::Value>| {
            let parts: Vec<String> = args
                .iter()
                .map(|v| v.to_string().unwrap_or_else(|_| "?".into()))
                .collect();
            log(parts.join("\t"));
            Ok(())
        })?,
    )?;

    lua.globals().set("ap64", ap64)?;

    let preload: Table = lua.globals().get::<Table>("package")?.get("preload")?;
    for (name, source) in LIBS.iter().chain(script.modules) {
        let chunk = lua
            .load(*source)
            .set_name(format!("connectors/{name}"))
            .into_function()?;
        preload.set(*name, chunk)?;
    }
    Ok(())
}
