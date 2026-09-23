//! Run a forked Archipelago connector script against a cart.
//!
//! The scripts in `connectors/` are Archipelago's own connectors with the emulator
//! taken out: they keep the protocol their Archipelago client speaks and call
//! [`ap64`](#the-ap64-table) for memory. AP64 owns the TCP socket the client connects
//! to ([`server`]) and hands each line to the script.
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
//! fatal: it ends the session, even if the script caught the error.

pub mod server;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ap64_cart::backend::Backend;
use ap64_cart::watch;
use ap64_cart::Log;
use base64::Engine as _;
use mlua::{Lua, Table};
use sha1::{Digest, Sha1};

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
    // oot and bt have always been None and neither shows the behavior.
    client_timeout: None,
};

/// Ocarina of Time, for Archipelago's OoT Client.
pub const OOT: Script = Script {
    id: "oot",
    name: "Ocarina of Time",
    client: "OoT Client",
    source: include_str!("../connectors/oot/connector.lua"),
    ports: &[28921],
    modules: &[("cartmem", include_str!("../connectors/oot/cartmem.lua"))],
    // Upstream's script never drops OoT Client, so neither do we. It would survive it
    // now -- every connection this server lets go of is reset rather than closed, which
    // is the one ending its socket task catches (see `server::Client`) -- but there is
    // still nothing to gain by dropping a client upstream would have kept.
    client_timeout: None,
};

/// Banjo-Tooie, for the randomizer's own client.
pub const BT: Script = Script {
    id: "bt",
    name: "Banjo-Tooie",
    client: "Banjo-Tooie Client",
    source: include_str!("../connectors/bt/connector.lua"),
    ports: &[21221],
    modules: &[("cartmem", include_str!("../connectors/bt/cartmem.lua"))],
    // Upstream never drops the client: its receive is non-blocking and silence is a
    // `timeout` it counts and carries on from. The client is the end that gives up, with a
    // 10 s read timeout, and it reconnects itself.
    client_timeout: None,
};

pub const SCRIPTS: &[Script] = &[GENERIC, OOT, BT];

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
        let lua = load(script, &backend, &log, &fatal)?;
        Ok(Self {
            script: *script,
            backend,
            log,
            lua: RefCell::new(lua),
            loaded_at: RefCell::new(generation),
            fatal,
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
    pub fn handle(&self, message: &str) -> Result<String, String> {
        let generation = self.backend.borrow().generation();
        if generation != *self.loaded_at.borrow() {
            (self.log)(format!(
                "the cart link restarted: loading the {} connector again",
                self.script.id
            ));
            *self.lua.borrow_mut() = load(&self.script, &self.backend, &self.log, &self.fatal)?;
            *self.loaded_at.borrow_mut() = generation;
        }
        let lua = self.lua.borrow();
        let handle: mlua::Function = lua
            .globals()
            .get("handle")
            .map_err(|_| "the connector script defines no handle()".to_string())?;
        let result = handle.call::<String>(message);
        if let Some(e) = self.fatal.borrow_mut().take() {
            return Err(e);
        }
        result.map_err(|e| format!("connector: {e}"))
    }
}

fn load(
    script: &Script,
    backend: &SharedBackend,
    log: &Log,
    fatal: &Rc<RefCell<Option<String>>>,
) -> Result<Lua, String> {
    let lua = Lua::new();
    install(&lua, script, backend.clone(), log.clone(), fatal.clone())
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
) -> mlua::Result<()> {
    let ap64 = lua.create_table()?;

    // A cart failure is remembered as well as raised: scripts catch errors per request
    // and would otherwise answer a dead cart with an endless stream of ERRORs. A bad
    // address is the request's fault, not the cart's, and only fails that request.
    let cart_err = {
        let fatal = fatal.clone();
        move |e: std::io::Error| {
            let msg = e.to_string();
            if e.kind() != std::io::ErrorKind::InvalidInput {
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
