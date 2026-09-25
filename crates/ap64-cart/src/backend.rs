//! Where RDRAM and the cartridge ROM come from: the console, through multi64d, or images
//! in tests.
//!
//! Every operation takes a whole batch, because over USB the cost is per exchange, not
//! per byte: a connector names every region a request needs and gets them in as few
//! wire requests as the M64P limits allow, in the order asked, with nothing added.
//!
//! The ROM is read from the cart itself (M64P `PEEKROM`) and cached: it cannot change
//! while the console runs, and clients read it far more often than it could be fetched.
//! The cache is dropped whenever the link to the cart restarts, since that is when the
//! console may have been reset or another game loaded.

use std::collections::HashMap;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::m64p;
use crate::transport::Multi64Transport;
use crate::watch::{Watch, WatchStats};
use crate::Log;

pub trait Backend {
    /// RDRAM size the cart reports (8 MiB with an Expansion Pak).
    fn rdram_size(&self) -> u32;

    /// One block per region, in order.
    fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>>;

    /// All of `writes`, in order.
    fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()>;

    /// The cartridge ROM window that can be read, from offset 0; `None` when the cart's
    /// agent predates `PEEKROM`. An upper bound, not the size of the image that booted.
    fn rom_window(&self) -> Option<u32>;

    /// Cartridge ROM, one block per region, in order.
    fn read_rom_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>>;

    /// Changes each time the link to the cart restarts. Anything derived from the ROM
    /// (a hash, a header) must be read again when it does.
    fn generation(&self) -> u32 {
        0
    }

    /// Watch a slot the game rewrites faster than the connector reads it ([`crate::watch`]).
    ///
    /// The region is then read alongside every batch of reads, and each change it catches
    /// is queued for [`Backend::take_watched`]. A backend that does not implement this
    /// serves live memory, which is what a connector falls back to anyway.
    fn set_watch(&mut self, _watch: Watch) {}

    /// The oldest change to the watched slot the connector has not been shown.
    fn take_watched(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// Read the watched slot now, outside any batch. For time that would be spent idle.
    fn sample_watch(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn watch_stats(&self) -> Option<WatchStats> {
        None
    }
}

fn check(addr: u32, len: usize, size: u32) -> io::Result<()> {
    check_in(addr, len, size, "RDRAM")
}

fn check_in(addr: u32, len: usize, size: u32, what: &str) -> io::Result<()> {
    let end = addr as u64 + len as u64;
    if end > size as u64 {
        // Zero-filling would turn a wrong address into a plausible-looking answer,
        // which is the hardest kind of bug to notice.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("0x{addr:X}..0x{end:X} is outside the cart's 0x{size:X} bytes of {what}"),
        ));
    }
    Ok(())
}

/// A flat RDRAM image, and optionally a ROM image. No console involved.
pub struct RamImage {
    pub ram: Vec<u8>,
    pub rom: Option<Vec<u8>>,
    watch: Option<Watch>,
}

impl RamImage {
    pub fn new(ram: Vec<u8>) -> Self {
        Self {
            ram,
            rom: None,
            watch: None,
        }
    }

    /// With a cart ROM, as an agent with `PEEKROM` serves it.
    pub fn with_rom(ram: Vec<u8>, rom: Vec<u8>) -> Self {
        Self {
            ram,
            rom: Some(rom),
            watch: None,
        }
    }

    fn at(&self, addr: u32, len: usize) -> io::Result<Vec<u8>> {
        check(addr, len, self.ram.len() as u32)?;
        Ok(self.ram[addr as usize..addr as usize + len].to_vec())
    }
}

impl Backend for RamImage {
    fn rdram_size(&self) -> u32 {
        self.ram.len() as u32
    }

    fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        let out: io::Result<Vec<Vec<u8>>> = regions
            .iter()
            .map(|&(addr, len)| self.at(addr, len))
            .collect();
        // What the cart does per wire exchange, since a read is what this stands in for.
        self.sample_watch()?;
        out
    }

    fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
        for &(addr, data) in writes {
            check(addr, data.len(), self.rdram_size())?;
        }
        for &(addr, data) in writes {
            self.ram[addr as usize..addr as usize + data.len()].copy_from_slice(data);
        }
        Ok(())
    }

    fn rom_window(&self) -> Option<u32> {
        self.rom.as_ref().map(|r| r.len() as u32)
    }

    fn read_rom_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        let rom = self
            .rom
            .as_ref()
            .ok_or_else(|| io::Error::other("no cart ROM"))?;
        regions
            .iter()
            .map(|&(addr, len)| {
                check_in(addr, len, rom.len() as u32, "cart ROM")?;
                Ok(rom[addr as usize..addr as usize + len].to_vec())
            })
            .collect()
    }

    fn set_watch(&mut self, watch: Watch) {
        self.watch = Some(watch);
    }

    fn take_watched(&mut self) -> Option<Vec<u8>> {
        self.watch.as_mut()?.take()
    }

    fn sample_watch(&mut self) -> io::Result<()> {
        let (addr, len) = match self.watch.as_ref() {
            Some(w) => w.region(),
            None => return Ok(()),
        };
        let bytes = self.at(addr, len)?;
        self.watch.as_mut().expect("just checked").observe(&bytes);
        Ok(())
    }

    fn watch_stats(&self) -> Option<WatchStats> {
        self.watch.as_ref().map(Watch::stats)
    }
}

/// Give up on a cart unreachable for this long, unless the caller says otherwise.
///
/// Long enough for a console to be power-cycled or the cart re-plugged. A caller with a client
/// waiting on the other side wants far less than this -- see [`Multi64::set_reconnect_deadline`]
/// -- because nothing it asked for can be answered in the meantime.
const RECONNECT_DEADLINE: Duration = Duration::from_secs(300);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(2);
/// Timeouts retried on the same transport before rebuilding it. A silent agent
/// (scene load, reset) answers again once its frame hook runs; a rebuild would redo
/// HELLO for nothing.
///
/// This plus one is how many times a single request can spend a whole
/// [`transport::REPLY_TIMEOUT`], and that product is a budget shared with the Archipelago
/// client, which drops the connection after 5 s of its own. See REPLY_TIMEOUT: the two are
/// only safe together, and `retry_budget_fits_the_clients_deadline` pins the pair.
pub(crate) const SOFT_RETRIES: u32 = 2;

/// Cached ROM is kept in pages of this size, fetched several to a request.
const ROM_PAGE: u32 = 1024;
/// Tries at a `PEEKROM` the cart answered `E_BUSY` (the game held the PI bus).
const ROM_BUSY_TRIES: u32 = 5;

/// After a stall is logged, how long later ones are counted instead of logged one by one.
///
/// A scene load stalls the agent over and over, and a line for each buried everything else in
/// the session log. The first says it has started; one line at the end says how many more there
/// were and the longest.
const STALL_RUN: Duration = Duration::from_secs(30);

/// Stalls counted since the one that was logged.
struct StallRun {
    since: Instant,
    more: u32,
    longest: Duration,
}

/// What the session log says about stalls: the first of a run in full, the rest counted and
/// said in one line once [`STALL_RUN`] has passed. Times are passed in, so tests can move them.
#[derive(Default)]
struct StallLog {
    run: Option<StallRun>,
}

impl StallLog {
    /// A stall of `silent`, answered on `attempt`. The line to log, if this one gets one.
    fn stalled(
        &mut self,
        at: Instant,
        what: &str,
        silent: Duration,
        attempt: u32,
    ) -> Option<String> {
        match &mut self.run {
            Some(run) => {
                run.more += 1;
                run.longest = run.longest.max(silent);
                None
            }
            None => {
                self.run = Some(StallRun {
                    since: at,
                    more: 0,
                    longest: Duration::ZERO,
                });
                Some(format!(
                    "cart {what}: agent silent for {}, answered on attempt {attempt}",
                    secs(silent)
                ))
            }
        }
    }

    /// The line for the stalls counted since the last one logged, once [`STALL_RUN`] has
    /// passed by `at`, or regardless with `now`. `total` is the session's count so far.
    fn flush(&mut self, at: Instant, now: bool, total: u32) -> Option<String> {
        let run = self.run.as_ref()?;
        let spent = at.saturating_duration_since(run.since);
        if !now && spent < STALL_RUN {
            return None;
        }
        let run = self.run.take()?;
        (run.more > 0).then(|| {
            format!(
                "cart: {} more stall{} in the {} after that, the longest {} ({total} this session)",
                run.more,
                if run.more == 1 { "" } else { "s" },
                secs(spent),
                secs(run.longest),
            )
        })
    }
}

fn secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f32())
}

/// Counters worth showing a player: what the session has survived.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// PEEKV/POKEV/PEEKROM round trips.
    pub requests: u64,
    /// Regions put on the wire.
    pub regions: u64,
    /// Timeouts ridden out on the same transport.
    pub stalls: u32,
    /// Transports rebuilt.
    pub reconnects: u32,
}

/// Asked whether to give up on a cart that is not answering.
///
/// Reconnecting rides out a power cycle, which is why it is allowed to take minutes -- but the
/// session it belongs to can be stopped by the person waiting, and they should not have to wait
/// out a console they have already decided to give up on.
pub type Cancelled = Rc<dyn Fn() -> bool>;

/// Told when the cart stops answering, and when it answers again.
///
/// The caller cannot see this for itself: a request that fails is retried and then reconnected
/// for as long as [`RECONNECT_DEADLINE`], and nothing returns to the caller in the meantime. A
/// console that was reset would otherwise go unreported for minutes, while the session sat
/// inside one call looking exactly as it did when everything worked.
pub type Health = Rc<dyn Fn(bool)>;

/// The console, through multi64d.
pub struct Multi64 {
    t: Multi64Transport,
    url: String,
    log: Log,
    retired_requests: u64,
    stats: Stats,
    /// Cart ROM pages read so far, by page number. Dropped on reconnect.
    rom_pages: HashMap<u32, Vec<u8>>,
    generation: u32,
    /// A slot read alongside every batch of reads, if a connector asked for one.
    watch: Option<Watch>,
    /// True once the agent has been asked to watch the slot itself (spec 4.3).
    ///
    /// The agent looks every frame, which is what the host cannot do at any poll rate, so
    /// while this holds nothing here samples: the events ride back on responses.
    watch_on_cart: bool,
    /// Told on the way down and on the way back up, never on every request.
    health: Option<Health>,
    /// Asked before each attempt at a cart that is not answering.
    cancelled: Option<Cancelled>,
    /// How long to keep trying to reach a cart that has stopped answering.
    reconnect_deadline: Duration,
    /// Whether the last request had to be retried, so recovery is reported once.
    ailing: bool,
    /// Stalls being counted rather than logged ([`STALL_RUN`]).
    stall_log: StallLog,
    /// The link generation the watch was last set up for, so a cart that restarted is
    /// asked again before anything relies on it.
    watch_armed_at: Option<u32>,
}

impl Multi64 {
    /// Connect and complete HELLO. Fails if the daemon holds no serial link or no agent
    /// answers.
    pub fn connect(url: &str, log: Log) -> io::Result<Self> {
        Ok(Self {
            t: Multi64Transport::connect(url, log.clone())?,
            url: url.to_string(),
            log,
            retired_requests: 0,
            stats: Stats::default(),
            rom_pages: HashMap::new(),
            generation: 0,
            watch: None,
            watch_on_cart: false,
            watch_armed_at: None,
            health: None,
            cancelled: None,
            reconnect_deadline: RECONNECT_DEADLINE,
            ailing: false,
            stall_log: StallLog::default(),
        })
    }

    /// Be told when the cart stops and starts answering ([`Health`]).
    pub fn set_health(&mut self, health: Health) {
        self.health = Some(health);
    }

    /// How long to keep trying before giving up on a cart that has stopped answering.
    ///
    /// Shorter is kinder when something is waiting on the answer: a request that cannot be
    /// served is better failed than held, since failing it ends the session and lets whoever
    /// was waiting start again, while holding it looks to them exactly like a hang.
    pub fn set_reconnect_deadline(&mut self, deadline: Duration) {
        self.reconnect_deadline = deadline;
    }

    /// Give up on an unanswering cart when `cancelled` says so ([`Cancelled`]).
    pub fn set_cancelled(&mut self, cancelled: Cancelled) {
        self.cancelled = Some(cancelled);
    }

    fn give_up(&self) -> bool {
        self.cancelled.as_ref().is_some_and(|c| c())
    }

    pub fn writable(&self) -> bool {
        self.t.writable
    }

    pub fn stats(&self) -> Stats {
        Stats {
            requests: self.retired_requests + self.t.requests() as u64,
            ..self.stats
        }
    }

    /// Log a stall: the first of a run in full, the rest counted ([`STALL_RUN`]).
    fn note_stall(&mut self, what: &str, silent: Duration, attempt: u32) {
        self.stats.stalls += 1;
        if let Some(line) = self
            .stall_log
            .stalled(Instant::now(), what, silent, attempt)
        {
            (self.log)(line);
        }
    }

    /// Say how many stalls were counted since the last one logged, once [`STALL_RUN`] has
    /// passed, or at once with `now` (before a line that should follow them, or at the end).
    pub fn flush_stalls(&mut self, now: bool) {
        if let Some(line) = self.stall_log.flush(Instant::now(), now, self.stats.stalls) {
            (self.log)(line);
        }
    }

    /// Run `op`, riding out a silent agent, then rebuilding the transport with backoff.
    ///
    /// Both operations are idempotent -- `PEEKV` is a read, and a repeated `POKEV`
    /// writes the same bytes -- so retrying a half-finished exchange is safe.
    fn with_retry<T>(
        &mut self,
        what: &str,
        mut op: impl FnMut(&mut Multi64Transport) -> io::Result<T>,
    ) -> io::Result<T> {
        let health = self.health.clone();
        self.flush_stalls(false);
        let started = Instant::now();
        let mut last = match op(&mut self.t) {
            Ok(v) => {
                if self.ailing {
                    self.ailing = false;
                    if let Some(h) = &health {
                        h(true);
                    }
                }
                return Ok(v);
            }
            Err(e) => e,
        };

        // Silence first, on the same transport. A late reply to the request we gave up
        // on is discarded by rid, so asking again is safe.
        for attempt in 1..=SOFT_RETRIES {
            if last.kind() != io::ErrorKind::TimedOut {
                break;
            }
            // Each of these costs a whole reply timeout, and they run before the reconnect
            // loop below is ever reached -- so without this, a stop asked for while the
            // cart was silent could not be felt for several seconds, which is precisely
            // when someone is most likely to ask for one.
            if self.give_up() {
                return Err(cancelled_err(what, &self.url));
            }
            match op(&mut self.t) {
                Ok(v) => {
                    self.note_stall(what, started.elapsed(), attempt + 1);
                    if self.ailing {
                        self.ailing = false;
                        if let Some(h) = &health {
                            h(true);
                        }
                    }
                    return Ok(v);
                }
                Err(e) => last = e,
            }
        }

        self.flush_stalls(true);
        (self.log)(format!(
            "cart {what} failed after {} silent: {last}; reconnecting to {}",
            secs(started.elapsed()),
            self.url
        ));
        let down = Instant::now();
        // Past the soft retries: whatever the caller asked for, the cart is not answering, and
        // from here this call may not return for minutes.
        if !self.ailing {
            self.ailing = true;
            if let Some(h) = &health {
                h(false);
            }
        }
        let deadline = Instant::now() + self.reconnect_deadline;
        let mut wait = Duration::from_millis(250);
        loop {
            if self.give_up() {
                return Err(cancelled_err(what, &self.url));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "cart {what}: gave up after {}s trying to reach {}: {last}",
                    self.reconnect_deadline.as_secs(),
                    self.url
                )));
            }
            wait_unless(wait, &|| self.give_up());
            wait = (wait * 2).min(RECONNECT_BACKOFF_MAX);
            match Multi64Transport::connect(&self.url, self.log.clone()) {
                Ok(t) => {
                    self.retired_requests += self.t.requests() as u64;
                    self.t = t;
                    self.stats.reconnects += 1;
                    // A new HELLO: the console may have been reset, or be running
                    // another image. Nothing read from the old one can be trusted.
                    self.rom_pages.clear();
                    self.generation += 1;
                    // Connecting is a blocking connect and a HELLO, neither of which can be
                    // interrupted; check again on the way out rather than going on to serve
                    // a request nobody is waiting for any more.
                    if self.give_up() {
                        return Err(cancelled_err(what, &self.url));
                    }
                    // A new HELLO: the agent has forgotten what it was watching, and this
                    // may not even be the same agent. Ask again before anything reads.
                    self.watch_on_cart = false;
                    (self.log)(format!(
                        "cart reconnected after {} (reconnect #{})",
                        secs(down.elapsed()),
                        self.stats.reconnects
                    ));
                    match op(&mut self.t) {
                        Ok(v) => {
                            self.ailing = false;
                            if let Some(h) = &health {
                                h(true);
                            }
                            return Ok(v);
                        }
                        Err(e) => last = e,
                    }
                }
                Err(e) => last = e,
            }
        }
    }
}

impl Multi64 {
    /// Is the agent answering right now?
    ///
    /// One small read, sent once: no soft retries, no reconnect, no waiting out a deadline. A
    /// caller asking "is the ROM still there" wants an answer in one round trip, and the retry
    /// machinery exists for requests that must not be lost -- this one may be lost freely, and
    /// asked again in a moment.
    pub fn alive(&mut self) -> bool {
        let probe = [m64p::Region { addr: 0, len: 4 }];
        let ok = self.t.peek(&probe).is_ok();
        if ok {
            self.collect_events();
        }
        ok
    }

    /// Ask the agent to watch the slot itself, if it can (spec 4.3).
    ///
    /// The host's own sampling catches what it happens to look at; an agent looks every
    /// frame, so this is the difference between narrowing the window and closing it. An
    /// agent that predates `WATCH` leaves `watch_slots` unset and the host keeps sampling.
    fn arm_watch(&mut self) -> io::Result<()> {
        let Some(w) = self.watch.as_ref() else {
            return Ok(());
        };
        if self.t.watch_slots.unwrap_or(0) == 0 {
            // An agent from before WATCH: the host samples, as it always did.
            self.watch_armed_at = Some(self.generation);
            return Ok(());
        }
        let (addr, len) = w.region();
        let (at, values) = match w.filter() {
            Some(f) => (f.at as u8, f.values.clone()),
            None => (0, Vec::new()),
        };
        let slot = m64p::Slot {
            addr,
            len: len as u8,
            at,
            values,
        };
        let watching = self.with_retry("watch", |t| t.watch(std::slice::from_ref(&slot)))?;
        self.watch_on_cart = watching > 0;
        self.watch_armed_at = Some(self.generation);
        if self.watch_on_cart {
            (self.log)(format!(
                "the cart is watching 0x{addr:X}+{len} itself, every frame"
            ));
        }
        Ok(())
    }

    /// Move whatever the last responses brought back into the watch's queue.
    fn collect_events(&mut self) {
        if !self.watch_on_cart {
            return;
        }
        let (events, dropped) = self.t.take_events();
        if let Some(w) = self.watch.as_mut() {
            for e in events {
                w.push_event(&e.bytes);
            }
            // An event the agent's queue lost is as lost as one this one lost, and a
            // session that reports only its own would understate it.
            w.add_dropped(dropped);
        }
    }

    /// Fetch the ROM pages in `pages` that are not cached, as few requests as the limits
    /// allow.
    fn fill_rom_pages(&mut self, pages: &[u32]) -> io::Result<()> {
        let missing: Vec<m64p::Region> = pages
            .iter()
            .filter(|p| !self.rom_pages.contains_key(p))
            .map(|&p| m64p::Region {
                addr: p * ROM_PAGE,
                len: ROM_PAGE as u16,
            })
            .collect();
        for batch in batches(&missing) {
            self.stats.regions += batch.len() as u64;
            let mut tries = 0;
            let got = loop {
                match self.with_retry("ROM read", |t| t.peek_rom(&batch)) {
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock && tries + 1 < ROM_BUSY_TRIES =>
                    {
                        // The game held the PI bus past the agent's bound: it is streaming
                        // from ROM. Give it a frame or two.
                        tries += 1;
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    other => break other?,
                }
            };
            for (region, bytes) in batch.iter().zip(got) {
                self.rom_pages.insert(region.addr / ROM_PAGE, bytes);
            }
        }
        Ok(())
    }
}

impl Backend for Multi64 {
    fn rdram_size(&self) -> u32 {
        self.t.rdram_bytes
    }

    fn rom_window(&self) -> Option<u32> {
        self.t.rom_bytes
    }

    fn generation(&self) -> u32 {
        self.generation
    }

    fn read_rom_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        let window = self.rom_window().ok_or_else(|| {
            io::Error::other(
                "this cart agent cannot read the cart ROM (it predates PEEKROM); \
                 add the agent to the seed again with this version of AP64",
            )
        })?;
        let mut pages = Vec::new();
        for &(addr, len) in regions {
            check_in(addr, len, window, "cart ROM window")?;
            if len > 0 {
                let first = addr / ROM_PAGE;
                let last = (addr + len as u32 - 1) / ROM_PAGE;
                pages.extend(first..=last);
            }
        }
        pages.sort_unstable();
        pages.dedup();
        self.fill_rom_pages(&pages)?;
        regions
            .iter()
            .map(|&(addr, len)| {
                let mut out = Vec::with_capacity(len);
                let mut at = addr;
                let end = addr + len as u32;
                while at < end {
                    let page = &self.rom_pages[&(at / ROM_PAGE)];
                    let off = (at % ROM_PAGE) as usize;
                    let take = ((end - at) as usize).min(ROM_PAGE as usize - off);
                    out.extend_from_slice(&page[off..off + take]);
                    at += take as u32;
                }
                Ok(out)
            })
            .collect()
    }

    fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        if self.watch.is_some() && self.watch_armed_at != Some(self.generation) {
            // First reads of this link, or the first since it restarted.
            let _ = self.arm_watch();
        }
        let (chunks, counts) = split(regions, self.rdram_size())?;
        let mut flat = Vec::with_capacity(chunks.len());
        for mut batch in batches(&chunks) {
            // The probe rides a request that was going out anyway, so it costs no
            // exchange and cannot slow the poll rate it exists to compensate for. A batch
            // already at a wire limit goes without it rather than becoming two requests.
            // An agent watching the slot itself needs no probe: it reports what it saw on
            // this very response, which is strictly more than a probe could catch.
            let probe = self
                .watch
                .as_ref()
                .filter(|_| !self.watch_on_cart)
                .map(|w| w.region())
                .filter(|&(_, len)| fits(&batch, len));
            if let Some((addr, len)) = probe {
                batch.push(m64p::Region {
                    addr,
                    len: len as u16,
                });
            }
            self.stats.regions += batch.len() as u64;
            let mut done = self.with_retry("read", |t| t.peek(&batch))?;
            if probe.is_some() {
                let bytes = done.pop().unwrap_or_default();
                if let Some(w) = self.watch.as_mut() {
                    w.observe(&bytes);
                }
            }
            self.collect_events();
            flat.extend(done);
        }
        stitch(flat, &counts)
    }

    fn set_watch(&mut self, watch: Watch) {
        self.watch = Some(watch);
        self.watch_on_cart = false;
        self.watch_armed_at = None;
        if let Err(e) = self.arm_watch() {
            // Not fatal: the host can still sample the slot itself, which is what it did
            // before agents could watch. Worth a line, since the two differ in what they
            // catch.
            (self.log)(format!(
                "cart watch refused ({e}); sampling from here instead"
            ));
        }
    }

    fn take_watched(&mut self) -> Option<Vec<u8>> {
        self.watch.as_mut()?.take()
    }

    fn sample_watch(&mut self) -> io::Result<()> {
        if self.watch_on_cart {
            // The agent is looking every frame; a read from here would cost an exchange
            // and see less than it already has.
            return Ok(());
        }
        let Some((addr, len)) = self.watch.as_ref().map(|w| w.region()) else {
            return Ok(());
        };
        let region = [m64p::Region {
            addr,
            len: len as u16,
        }];
        self.stats.regions += 1;
        let done = self.with_retry("read", |t| t.peek(&region))?;
        if let (Some(w), Some(bytes)) = (self.watch.as_mut(), done.first()) {
            w.observe(bytes);
        }
        Ok(())
    }

    fn watch_stats(&self) -> Option<WatchStats> {
        self.watch.as_ref().map(Watch::stats)
    }

    fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
        // Validate and split everything first, so a rejected address fails before any
        // of the batch has been sent.
        let size = self.rdram_size();
        let mut chunks: Vec<(u32, &[u8])> = Vec::new();
        for &(addr, data) in writes {
            check(addr, data.len(), size)?;
            chunks.extend(
                data.chunks(m64p::MAX_REGION_BYTES)
                    .enumerate()
                    .map(|(i, c)| (addr + (i * m64p::MAX_REGION_BYTES) as u32, c)),
            );
        }
        let mut batch: Vec<(u32, &[u8])> = Vec::new();
        let mut bytes = 0usize;
        for c in chunks {
            if batch.len() == m64p::MAX_REGIONS || bytes + c.1.len() > m64p::MAX_TOTAL_BYTES {
                self.stats.regions += batch.len() as u64;
                self.with_retry("write", |t| t.poke(&batch))?;
                batch.clear();
                bytes = 0;
            }
            bytes += c.1.len();
            batch.push(c);
        }
        if !batch.is_empty() {
            self.stats.regions += batch.len() as u64;
            self.with_retry("write", |t| t.poke(&batch))?;
        }
        Ok(())
    }
}

/// Split regions into wire-legal chunks, remembering how many chunks each became so
/// the replies can be stitched back together.
fn split(regions: &[(u32, usize)], size: u32) -> io::Result<(Vec<m64p::Region>, Vec<usize>)> {
    let mut chunks = Vec::new();
    let mut counts = Vec::with_capacity(regions.len());
    for &(addr, len) in regions {
        check(addr, len, size)?;
        let before = chunks.len();
        let mut done = 0usize;
        while done < len {
            let take = (len - done).min(m64p::MAX_REGION_BYTES);
            chunks.push(m64p::Region {
                addr: addr + done as u32,
                len: take as u16,
            });
            done += take;
        }
        counts.push(chunks.len() - before);
    }
    Ok((chunks, counts))
}

/// What a caller gets when it asked to be stopped rather than waited for.
fn cancelled_err(what: &str, url: &str) -> io::Error {
    io::Error::other(format!("cart {what}: stopped while reaching {url}"))
}

/// Sleep up to `total`, giving up as soon as `cancelled` says so.
///
/// In slices rather than one sleep: the backoff between reconnect attempts runs to seconds, and
/// a session being stopped should be felt in a moment rather than at the end of one.
fn wait_unless(total: Duration, cancelled: &dyn Fn() -> bool) {
    const SLICE: Duration = Duration::from_millis(50);
    let until = Instant::now() + total;
    while Instant::now() < until {
        if cancelled() {
            return;
        }
        std::thread::sleep(SLICE.min(total));
    }
}

/// Whether one more region of `len` bytes still fits `batch` within the wire limits.
fn fits(batch: &[m64p::Region], len: usize) -> bool {
    let bytes: usize = batch.iter().map(|r| r.len as usize).sum();
    batch.len() < m64p::MAX_REGIONS && bytes + len <= m64p::MAX_TOTAL_BYTES
}

/// Pack chunks into requests in their original order, cutting only where the
/// per-request region or byte limit forces it.
fn batches(chunks: &[m64p::Region]) -> Vec<Vec<m64p::Region>> {
    let mut out = Vec::new();
    let mut batch: Vec<m64p::Region> = Vec::new();
    let mut bytes = 0usize;
    for &c in chunks {
        if batch.len() == m64p::MAX_REGIONS || bytes + c.len as usize > m64p::MAX_TOTAL_BYTES {
            out.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        bytes += c.len as usize;
        batch.push(c);
    }
    if !batch.is_empty() {
        out.push(batch);
    }
    out
}

/// Join per-chunk replies back into one block per requested region.
fn stitch(flat: Vec<Vec<u8>>, counts: &[usize]) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::with_capacity(counts.len());
    let mut it = flat.into_iter();
    for &n in counts {
        let mut joined = Vec::new();
        for _ in 0..n {
            joined.extend(
                it.next().ok_or_else(|| {
                    io::Error::other("cart returned fewer regions than requested")
                })?,
            );
        }
        out.push(joined);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(addr: u32, len: u16) -> m64p::Region {
        m64p::Region { addr, len }
    }

    fn spans(chunks: &[m64p::Region]) -> Vec<(u32, u16)> {
        chunks.iter().map(|c| (c.addr, c.len)).collect()
    }

    /// A scene load's stalls come to two lines: the first, and one for the rest of the run.
    #[test]
    fn a_run_of_stalls_is_two_lines() {
        let t0 = Instant::now();
        let ms = |n| Duration::from_millis(n);
        let mut log = StallLog::default();
        let first = log.stalled(t0, "read", ms(1400), 2).unwrap();
        assert_eq!(
            first,
            "cart read: agent silent for 1.4 s, answered on attempt 2"
        );
        assert_eq!(log.stalled(t0 + ms(900), "write", ms(2100), 2), None);
        assert_eq!(log.stalled(t0 + ms(1800), "read", ms(700), 2), None);
        assert_eq!(
            log.flush(t0 + ms(5000), false, 3),
            None,
            "still inside the run"
        );
        assert_eq!(
            log.flush(t0 + STALL_RUN, false, 3).unwrap(),
            "cart: 2 more stalls in the 30.0 s after that, the longest 2.1 s (3 this session)"
        );
        // The next stall starts a new run, logged in full.
        assert!(log
            .stalled(t0 + STALL_RUN + ms(1), "read", ms(500), 2)
            .is_some());
    }

    /// A stall on its own says nothing more, and a flush forced early says what it has.
    #[test]
    fn a_lone_stall_is_one_line_and_a_forced_flush_is_immediate() {
        let t0 = Instant::now();
        let mut log = StallLog::default();
        assert!(log
            .stalled(t0, "read", Duration::from_millis(600), 2)
            .is_some());
        assert_eq!(log.flush(t0 + STALL_RUN, false, 1), None);
        assert!(log
            .stalled(t0, "read", Duration::from_millis(600), 2)
            .is_some());
        assert_eq!(log.stalled(t0, "read", Duration::from_millis(900), 2), None);
        let line = log.flush(t0 + Duration::from_secs(2), true, 2).unwrap();
        assert!(
            line.starts_with("cart: 1 more stall in the 2.0 s"),
            "{line}"
        );
    }

    /// The three regions read from Paper Mario on hardware go out as one request, as
    /// asked, with nothing added.
    #[test]
    fn batches_send_exactly_the_chunks_given_in_order() {
        let chunks = [r(0x0040_11E0, 4), r(0x0040_51C0, 32), r(0x0010_F290, 16)];
        let out = batches(&chunks);
        assert_eq!(out.len(), 1);
        assert_eq!(spans(&out[0]), spans(&chunks));
    }

    /// A stop must not wait out a backoff meant for a console being power-cycled.
    #[test]
    fn a_cancelled_wait_returns_at_once() {
        let started = Instant::now();
        wait_unless(Duration::from_secs(30), &|| true);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_wait_nobody_cancels_runs_its_course() {
        let started = Instant::now();
        wait_unless(Duration::from_millis(150), &|| false);
        assert!(
            started.elapsed() >= Duration::from_millis(140),
            "{:?}",
            started.elapsed()
        );
    }

    /// The probe must never turn one request into two: the whole point is that it is free.
    #[test]
    fn a_probe_rides_a_batch_only_while_the_wire_limits_leave_room() {
        assert!(fits(&[r(0x0040_11E0, 4)], 4));
        let full: Vec<m64p::Region> = (0..m64p::MAX_REGIONS as u32)
            .map(|i| r(i * 16, 1))
            .collect();
        assert!(!fits(&full, 4), "a batch already at the region limit");
        assert!(
            !fits(&[r(0, m64p::MAX_TOTAL_BYTES as u16)], 4),
            "a batch already at the byte limit"
        );
        assert!(fits(&[r(0, (m64p::MAX_TOTAL_BYTES - 4) as u16)], 4));
    }

    #[test]
    fn batches_cut_only_where_the_wire_limits_force_it() {
        let many: Vec<m64p::Region> = (0..=m64p::MAX_REGIONS as u32)
            .map(|i| r(i * 16, 1))
            .collect();
        let out = batches(&many);
        assert_eq!(
            out.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![m64p::MAX_REGIONS, 1]
        );
        assert_eq!(spans(&out.concat()), spans(&many));
        assert_eq!(
            batches(&[r(0, 4096), r(0x1000, 4096)]).len(),
            2,
            "8192 B > 7936 B"
        );
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn split_cuts_large_regions_and_counts_them() {
        let (chunks, counts) = split(&[(0x100, 9000), (0x10, 4)], 0x80_0000).unwrap();
        assert_eq!(
            spans(&chunks),
            vec![(0x100, 4096), (0x1100, 4096), (0x2100, 808), (0x10, 4)]
        );
        assert_eq!(counts, vec![3, 1]);
        assert!(
            split(&[(0x7F_FFFF, 2)], 0x80_0000).is_err(),
            "past the end of RDRAM"
        );
    }

    #[test]
    fn stitch_rejoins_split_regions_and_rejects_a_short_reply() {
        let flat = vec![vec![1, 2], vec![3], vec![4, 5]];
        assert_eq!(
            stitch(flat, &[2, 1]).unwrap(),
            vec![vec![1, 2, 3], vec![4, 5]]
        );
        assert!(stitch(vec![vec![1]], &[2]).is_err());
    }

    #[test]
    fn a_ram_image_refuses_out_of_range_and_writes_nothing_on_refusal() {
        let mut ram = RamImage::new(vec![0; 16]);
        assert!(ram.read_many(&[(12, 8)]).is_err());
        assert!(ram.write_many(&[(0, &[1]), (15, &[2, 3])]).is_err());
        assert_eq!(ram.ram[0], 0, "a batch with a bad write applies none of it");
        ram.write_many(&[(4, &[9, 8])]).unwrap();
        assert_eq!(
            ram.read_many(&[(4, 2), (0, 1)]).unwrap(),
            vec![vec![9, 8], vec![0]]
        );
    }

    /// The whole retry budget must stay inside the deadline the client enforces.
    ///
    /// AP64 does not own this number on its own. Archipelago's BizHawk Client puts a 5 s
    /// deadline on every request (worlds/_bizhawk/__init__.py, `_send_message`), fires it
    /// silently, and reconnects -- so a request that AP64 is still patiently retrying past
    /// 5 s has no one left to answer. REPLY_TIMEOUT was 3 s and SOFT_RETRIES is 2, which is
    /// 9 s worst case: any hiccup needing two retries lost the client by arithmetic.
    ///
    /// Pinned as a pair because neither constant is wrong alone, and a later change to
    /// either one can quietly put the product back over the line.
    #[test]
    fn retry_budget_fits_the_clients_deadline() {
        /// worlds/_bizhawk/__init__.py, `_send_message`.
        const CLIENT_DEADLINE: Duration = Duration::from_secs(5);

        let attempts = SOFT_RETRIES + 1;
        let budget = crate::transport::REPLY_TIMEOUT * attempts;
        assert!(
            budget < CLIENT_DEADLINE,
            "a request can take {budget:?} ({attempts} x {:?}) before the reconnect loop is even reached, but the Archipelago client gives up at {CLIENT_DEADLINE:?} and silently reconnects",
            crate::transport::REPLY_TIMEOUT,
        );
    }
}
