//! Answer RetroArch's Network Commands for an Archipelago client, from a cart.
//!
//! Some clients do not use a connector script at all: they read emulator memory directly,
//! and when no emulator is running they fall back to RetroArch's Network Commands, a
//! text protocol over UDP. Two commands, 4 bytes at a time at a KSEG1 address, each word
//! shown little-endian:
//!
//! ```text
//! READ_CORE_MEMORY  <addr> 4           ->  READ_CORE_MEMORY <addr> b0 b1 b2 b3
//! WRITE_CORE_MEMORY <addr> b0 b1 b2 b3 ->  WRITE_CORE_MEMORY <addr> 4
//! ```
//!
//! # Never answer late
//!
//! The client waits a fixed time for each reply, and the protocol carries no request id:
//! a reply that arrives after the client gave up is taken as the answer to its *next*
//! request, and every exchange after that is off by one. On a console that showed as
//! garbage pointers and a false "new ROM" whenever the game changed scenes, because the
//! agent only answers when the game's frame loop runs, and the loop pauses while a scene
//! loads. So the cart is not asked anything on the UDP side:
//!
//! - reads come from a [`Snapshot`] of every 256-byte window the client has used, which
//!   the cart's owner refreshes continuously in batched requests;
//! - writes are acknowledged at once, applied to the snapshot, and queued in order for
//!   the cart's owner to put on the console.
//!
//! The one wait is for a window no one has asked for before, and it is bounded well inside
//! the client's timeout; past it the reply is an error, never a late answer.
//!
//! # Writes
//!
//! The client writes a whole word even to change one byte, so replaying the word would put
//! back whatever the game changed in the other three. Only the bytes the client actually
//! changed are written, with the client's values. In [`Options::bitwise`] ranges -- flag
//! fields the client read-modify-writes one bit at a time -- the live byte is read first
//! and only the bits the client set or cleared are applied.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ap64_cart::backend::Backend;

use crate::server::Event;

/// What a snapshot is kept in, and fetched by.
pub const WINDOW: u32 = 0x100;
/// How long a reply may wait for a window's first fetch. Clients using RetroArch's
/// commands through EmuLoader give up at 0.5 s.
const FIRST_FETCH: Duration = Duration::from_millis(350);
/// Silence after which the client is taken to have gone. Its snapshot is dropped then, so
/// a client that comes back is not answered from memory that has moved on without it.
const CLIENT_GONE: Duration = Duration::from_secs(3);
/// How often the UDP side looks up from its socket to see whether it should stop.
const RECV_POLL: Duration = Duration::from_millis(50);
/// Between [`Event::Idle`]s while nothing is being asked.
const IDLE_EVENT_EVERY: Duration = Duration::from_secs(2);
/// Silence from the client worth a line when it ends, short of [`CLIENT_GONE`]. The client
/// asks many times a second while it runs, and restarts after an error with a pause of its
/// own, so a gap this long is that restart or something holding it up.
const CLIENT_PAUSE: Duration = Duration::from_secs(2);
/// A reply slower than this is worth a line: it is past the 0.5 s EmuLoader waits, so the
/// client has already given up on it and logged "timed out".
const SLOW_REPLY: Duration = Duration::from_millis(500);

/// How a game's client is answered.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The UDP port the client sends to (RetroArch's own is 55355).
    pub port: u16,
    /// RDRAM ranges the client read-modify-writes a bit at a time. See the module docs.
    pub bitwise: &'static [Range<u32>],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Write {
    phys: u32,
    served: [u8; 4],
    client: [u8; 4],
}

#[derive(Default)]
struct Shared {
    windows: BTreeMap<u32, Vec<u8>>,
    wanted: Vec<u32>,
    writes: VecDeque<Write>,
    last_request: Option<Instant>,
    last_peer: Option<SocketAddr>,
    requests: u64,
    /// Replies that were an error because a window's first fetch did not come back in time.
    unfetched: u64,
    /// Lines for the session log, taken by whoever raises events ([`Snapshot::take_notes`]).
    notes: Vec<Note>,
}

/// Something the UDP side saw that the session log should say.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Note {
    /// A reply made an error for want of a first fetch: the command, the address as the client
    /// wrote it, so it lines up with the client's own log, and how long it waited.
    Missed(String, String),
    /// A reply that took longer than the client waits.
    Slow(String, Duration),
    /// The client asked again after this long a silence.
    Back(Duration),
}

/// What the client is answered from. Cloned freely: every clone is the same snapshot.
#[derive(Clone)]
pub struct Snapshot {
    inner: Arc<(Mutex<Shared>, Condvar)>,
    rdram: u32,
}

impl Snapshot {
    pub fn new(rdram: u32) -> Self {
        Self {
            inner: Arc::new((Mutex::new(Shared::default()), Condvar::new())),
            rdram,
        }
    }

    /// Requests answered so far, and how many of those were errors for want of a first fetch.
    pub fn counts(&self) -> (u64, u64) {
        let s = self.inner.0.lock().unwrap();
        (s.requests, s.unfetched)
    }

    /// When the client last asked anything, and from where.
    fn last_seen(&self) -> (Option<Instant>, Option<SocketAddr>) {
        let s = self.inner.0.lock().unwrap();
        (s.last_request, s.last_peer)
    }

    /// The big-endian word at `phys`, waiting a bounded time for its window's first fetch.
    /// A miss is noted against `cmd` and `addr_s`, as the client wrote them.
    fn word(&self, phys: u32, cmd: &str, addr_s: &str) -> Option<[u8; 4]> {
        let base = phys & !(WINDOW - 1);
        let off = (phys - base) as usize;
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        if !s.windows.contains_key(&base) {
            if !s.wanted.contains(&base) {
                s.wanted.push(base);
            }
            let deadline = Instant::now() + FIRST_FETCH;
            while !s.windows.contains_key(&base) {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    s.unfetched += 1;
                    s.notes
                        .push(Note::Missed(cmd.to_string(), addr_s.to_string()));
                    return None;
                }
                s = cvar.wait_timeout(s, left).unwrap().0;
            }
        }
        Some(s.windows[&base][off..off + 4].try_into().unwrap())
    }

    /// Answer one command line from `peer`. Never waits on the cart beyond a window's first
    /// fetch, and then only for a bounded time.
    pub fn handle(&self, line: &str, peer: Option<SocketAddr>) -> String {
        {
            let mut s = self.inner.0.lock().unwrap();
            s.requests += 1;
            let now = Instant::now();
            if let Some(gap) = s.last_request.map(|t| now - t) {
                if (CLIENT_PAUSE..CLIENT_GONE).contains(&gap) {
                    s.notes.push(Note::Back(gap));
                }
            }
            s.last_request = Some(now);
            if peer.is_some() {
                s.last_peer = peer;
            }
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (cmd, addr_s) = match parts.as_slice() {
            [c, a, ..] => (*c, *a),
            _ => return format!("ERROR 0 -1 unrecognized: {line:?}"),
        };
        let phys = match u32::from_str_radix(addr_s, 16) {
            Ok(a) => a & 0x1FFF_FFFF,
            Err(_) => return format!("{cmd} {addr_s} -1 bad address"),
        };
        if phys & 3 != 0 || phys as u64 + 4 > self.rdram as u64 {
            return format!("{cmd} {addr_s} -1 address outside RDRAM or unaligned");
        }
        match cmd {
            "READ_CORE_MEMORY" => match self.word(phys, cmd, addr_s) {
                // Big-endian in RDRAM, shown little-endian: reverse the bytes.
                Some(b) => format!(
                    "READ_CORE_MEMORY {addr_s} {:02X} {:02X} {:02X} {:02X}",
                    b[3], b[2], b[1], b[0]
                ),
                None => format!("READ_CORE_MEMORY {addr_s} -1 the console has not answered yet"),
            },
            "WRITE_CORE_MEMORY" if parts.len() == 6 => {
                let le: Result<Vec<u8>, _> = parts[2..]
                    .iter()
                    .map(|p| u8::from_str_radix(p, 16))
                    .collect();
                let Ok(le) = le else {
                    return format!("{cmd} {addr_s} -1 bad data");
                };
                let client = [le[3], le[2], le[1], le[0]];
                let Some(served) = self.word(phys, cmd, addr_s) else {
                    return format!("{cmd} {addr_s} -1 the console has not answered yet");
                };
                let base = phys & !(WINDOW - 1);
                let off = (phys - base) as usize;
                let mut s = self.inner.0.lock().unwrap();
                if let Some(w) = s.windows.get_mut(&base) {
                    w[off..off + 4].copy_from_slice(&client);
                }
                s.writes.push_back(Write {
                    phys,
                    served,
                    client,
                });
                format!("WRITE_CORE_MEMORY {addr_s} 4")
            }
            _ => format!("{cmd} {addr_s} -1 unsupported"),
        }
    }

    /// A reply to `line` took `took`. Noted if the client will have given up on it.
    fn replied(&self, line: &str, took: Duration) {
        if took >= SLOW_REPLY {
            let what = line
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            self.inner
                .0
                .lock()
                .unwrap()
                .notes
                .push(Note::Slow(what, took));
        }
    }

    /// The session-log lines noted since the last call, oldest first, as events.
    pub fn take_notes(&self) -> Vec<Event> {
        let notes = std::mem::take(&mut self.inner.0.lock().unwrap().notes);
        notes
            .into_iter()
            .map(|n| match n {
                Note::Missed(cmd, addr) => {
                    Event::Missed(format!(
                    "answered the client's {} of 0x{addr} with an error: the first in that area, \
                     and the cart gave nothing within {} ms",
                    if cmd == "WRITE_CORE_MEMORY" { "write" } else { "read" },
                    FIRST_FETCH.as_millis()
                ))
                }
                Note::Slow(what, took) => Event::Note(format!(
                    "a reply to the client took {} ms ({what}), past the {} ms it waits",
                    took.as_millis(),
                    SLOW_REPLY.as_millis()
                )),
                Note::Back(gap) => Event::Note(format!(
                    "the client asked again after {:.1} s of silence",
                    gap.as_secs_f32()
                )),
            })
            .collect()
    }

    /// One pass for whoever owns the cart: queued writes, in order, then every window the
    /// client is using, in batched reads. Returns whether there was anything to do.
    pub fn refresh(&self, cart: &mut dyn Backend, bitwise: &[Range<u32>]) -> io::Result<bool> {
        let (lock, cvar) = &*self.inner;
        let (writes, bases) = {
            let mut s = lock.lock().unwrap();
            let writes: Vec<Write> = s.writes.drain(..).collect();
            let gone = s.last_request.map_or(true, |t| t.elapsed() >= CLIENT_GONE);
            if gone {
                // Nothing to keep current, and nothing to serve from once the client is back.
                s.windows.clear();
            }
            let wanted = std::mem::take(&mut s.wanted);
            let mut bases: Vec<u32> = if gone {
                Vec::new()
            } else {
                s.windows.keys().copied().collect()
            };
            bases.extend(wanted);
            bases.sort_unstable();
            bases.dedup();
            (writes, bases)
        };
        for w in &writes {
            apply(cart, w, bitwise)?;
        }
        if bases.is_empty() {
            return Ok(!writes.is_empty());
        }
        let regions: Vec<(u32, usize)> = bases
            .iter()
            .map(|&b| (b, WINDOW.min(self.rdram - b) as usize))
            .collect();
        let blocks = cart.read_many(&regions)?;
        let mut s = lock.lock().unwrap();
        // A write queued while this read was in flight is already in the snapshot, and may not
        // be on the cart yet: keep it over what came back.
        let queued: Vec<(u32, [u8; 4])> = s.writes.iter().map(|w| (w.phys, w.client)).collect();
        for (base, mut data) in bases.into_iter().zip(blocks) {
            for &(phys, bytes) in &queued {
                if phys & !(WINDOW - 1) == base {
                    let off = (phys - base) as usize;
                    data[off..off + 4].copy_from_slice(&bytes);
                }
            }
            s.windows.insert(base, data);
        }
        cvar.notify_all();
        Ok(true)
    }
}

/// The word to put on the cart for `w`, given the `live` word where the change is bitwise,
/// and which byte indices changed. See the module docs.
fn merge(w: &Write, live: Option<[u8; 4]>, bitwise: &[Range<u32>]) -> ([u8; 4], Vec<usize>) {
    let changed: Vec<usize> = (0..4).filter(|&i| w.client[i] != w.served[i]).collect();
    let mut out = w.client;
    if let Some(live) = live {
        for &i in &changed {
            if bitwise.iter().any(|r| r.contains(&(w.phys + i as u32))) {
                let set = w.client[i] & !w.served[i];
                let clear = w.served[i] & !w.client[i];
                out[i] = (live[i] & !clear) | set;
            }
        }
    }
    (out, changed)
}

fn apply(cart: &mut dyn Backend, w: &Write, bitwise: &[Range<u32>]) -> io::Result<()> {
    let needs_live = (0..4).any(|i| {
        w.client[i] != w.served[i] && bitwise.iter().any(|r| r.contains(&(w.phys + i as u32)))
    });
    let live = if needs_live {
        let got = cart.read_many(&[(w.phys, 4)])?;
        Some(got[0][..4].try_into().unwrap())
    } else {
        None
    };
    let (out, changed) = merge(w, live, bitwise);
    if changed.is_empty() {
        return Ok(());
    }
    // One region per run of adjacent changed bytes.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        match runs.last_mut() {
            Some((_, end)) if *end == i => *end = i + 1,
            _ => runs.push((i, i + 1)),
        }
    }
    let writes: Vec<(u32, &[u8])> = runs
        .iter()
        .map(|&(a, b)| (w.phys + a as u32, &out[a..b]))
        .collect();
    cart.write_many(&writes)
}

/// The UDP side: answer every datagram from the snapshot until `stop`.
fn answer(sock: &UdpSocket, snap: &Snapshot, stop: &AtomicBool) {
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        let (n, peer) = match sock.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            // Windows reports an earlier reply's ICMP port-unreachable on the next receive.
            // It says nothing about this datagram, and clients open a fresh socket each time
            // they attach, so it happens as a matter of course.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
            Err(_) => continue,
        };
        let got = Instant::now();
        let line = String::from_utf8_lossy(&buf[..n]);
        let reply = snap.handle(&line, Some(peer));
        let _ = sock.send_to(reply.as_bytes(), peer);
        snap.replied(&line, got.elapsed());
    }
}

/// Serve the client from `cart` until `stop` is set or the cart fails. `cart` stays on the
/// calling thread; only the snapshot crosses to the thread that answers UDP.
///
/// Raises the same [`Event`]s as [`crate::server::serve`], so a session reports either the
/// same way. There is no connection to see, so the client is taken to be here while it is
/// asking and gone after [`CLIENT_GONE`] of silence.
pub fn serve(
    cart: &mut dyn Backend,
    opts: &Options,
    stop: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
) -> Result<(), String> {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, opts.port)).map_err(|e| {
        format!(
            "listening on UDP localhost:{} for the Archipelago client: {e}",
            opts.port
        )
    })?;
    sock.set_read_timeout(Some(RECV_POLL))
        .map_err(|e| e.to_string())?;
    let snap = Snapshot::new(cart.rdram_size());
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| answer(&sock, &snap, &done));
        on_event(Event::Listening(opts.port));
        let mut here = false;
        let mut counted = 0u64;
        let mut last_idle = Instant::now();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            let worked = match snap.refresh(cart, opts.bitwise) {
                Ok(w) => w,
                Err(e) => break Err(e.to_string()),
            };
            let (requests, _) = snap.counts();
            for _ in counted..requests {
                on_event(Event::Handled);
            }
            counted = requests;
            for note in snap.take_notes() {
                on_event(note);
            }
            let (last, peer) = snap.last_seen();
            let asking = last.is_some_and(|t| t.elapsed() < CLIENT_GONE);
            if asking && !here {
                here = true;
                on_event(Event::ClientConnected(
                    peer.unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
                ));
            } else if !asking && here {
                here = false;
                on_event(Event::ClientDisconnected(format!(
                    "no request for {} s",
                    CLIENT_GONE.as_secs()
                )));
            }
            if !worked {
                if last_idle.elapsed() >= IDLE_EVENT_EVERY {
                    last_idle = Instant::now();
                    on_event(Event::Idle);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        done.store(true, Ordering::Relaxed);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap64_cart::backend::RamImage;

    const FLAGS: Range<u32> = 0x1000..0x1010;

    fn ram() -> RamImage {
        let mut r = vec![0u8; 0x80_0000];
        r[0x2000..0x2004].copy_from_slice(&[0x80, 0x5E, 0x33, 0x28]);
        r[0x1000] = 0b0000_0001;
        RamImage::new(r)
    }

    /// The snapshot filled for `addrs`, as the cart's owner would after the client asked.
    fn primed(cart: &mut RamImage, addrs: &[u32]) -> Snapshot {
        let snap = Snapshot::new(cart.rdram_size());
        {
            let mut s = snap.inner.0.lock().unwrap();
            s.last_request = Some(Instant::now());
            s.wanted = addrs.iter().map(|a| a & !(WINDOW - 1)).collect();
        }
        snap.refresh(cart, &[FLAGS]).unwrap();
        snap
    }

    fn word(cart: &mut RamImage, at: u32) -> [u8; 4] {
        cart.read_many(&[(at, 4)]).unwrap()[0][..4]
            .try_into()
            .unwrap()
    }

    #[test]
    fn a_read_is_the_word_shown_little_endian_at_a_kseg1_address() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x2000]);
        assert_eq!(
            snap.handle("READ_CORE_MEMORY A0002000 4", None),
            "READ_CORE_MEMORY A0002000 28 33 5E 80"
        );
    }

    /// Nothing is ever answered late: a window that has not been fetched is an error after a
    /// bounded wait, which the client survives, where a late answer shifts every later one.
    #[test]
    fn a_window_never_fetched_is_an_error_not_a_late_answer() {
        let snap = Snapshot::new(0x80_0000);
        let t = Instant::now();
        let reply = snap.handle("READ_CORE_MEMORY A0003000 4", None);
        assert!(reply.starts_with("READ_CORE_MEMORY A0003000 -1"), "{reply}");
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(snap.counts(), (1, 1));
    }

    /// Each error reply is a line in the session log, with the address as the client wrote it,
    /// so it can be found in the client's own log.
    #[test]
    fn a_miss_is_a_line_naming_the_address_the_client_used() {
        let snap = Snapshot::new(0x80_0000);
        snap.handle("READ_CORE_MEMORY A07FFF1C 4", None);
        let notes = snap.take_notes();
        assert!(
            matches!(notes.as_slice(), [Event::Missed(m)]
                if m.contains("read of 0xA07FFF1C") && !m.contains("  ")),
            "{notes:?}"
        );
        assert!(snap.take_notes().is_empty(), "each note is taken once");
    }

    /// A pause short of the client being gone is how its restart after an error shows.
    #[test]
    fn a_client_back_after_a_pause_is_a_line() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x2000]);
        snap.inner.0.lock().unwrap().last_request =
            Some(Instant::now() - Duration::from_millis(2500));
        snap.handle("READ_CORE_MEMORY A0002000 4", None);
        let notes = snap.take_notes();
        assert!(
            matches!(notes.as_slice(), [Event::Note(n)] if n.contains("after 2.5 s of silence")),
            "{notes:?}"
        );
        // Asking at the usual pace says nothing.
        snap.handle("READ_CORE_MEMORY A0002000 4", None);
        assert!(snap.take_notes().is_empty());
    }

    /// A reply the client has already given up on is the one thing that makes it log "timed out".
    #[test]
    fn a_reply_slower_than_the_client_waits_is_a_line() {
        let snap = Snapshot::new(0x80_0000);
        snap.replied("READ_CORE_MEMORY A0002000 4", Duration::from_millis(120));
        assert!(snap.take_notes().is_empty());
        snap.replied("READ_CORE_MEMORY A0002000 4", Duration::from_millis(640));
        let notes = snap.take_notes();
        assert!(
            matches!(notes.as_slice(), [Event::Note(n)] if n.contains("took 640 ms (READ_CORE_MEMORY A0002000)")),
            "{notes:?}"
        );
    }

    #[test]
    fn out_of_range_and_unaligned_addresses_are_refused() {
        let snap = Snapshot::new(0x80_0000);
        assert!(snap
            .handle("READ_CORE_MEMORY A0800000 4", None)
            .contains("-1"));
        assert!(snap
            .handle("READ_CORE_MEMORY A0002001 4", None)
            .contains("-1"));
    }

    /// The client writes a whole word to change one byte. The other three are the game's,
    /// and it may have changed them since the client read them.
    #[test]
    fn only_the_bytes_the_client_changed_are_written() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x2000]);
        // The game moves on after the client read the word.
        cart.write_many(&[(0x2003, &[0x99])]).unwrap();
        // The client changes the first byte and hands back the rest as it read them.
        let reply = snap.handle("WRITE_CORE_MEMORY A0002000 28 33 5E 81", None);
        assert_eq!(reply, "WRITE_CORE_MEMORY A0002000 4");
        snap.refresh(&mut cart, &[FLAGS]).unwrap();
        assert_eq!(word(&mut cart, 0x2000), [0x81, 0x5E, 0x33, 0x99]);
    }

    /// A value is a value: a byte the client sets to 0xFF lands as 0xFF, whatever the game
    /// did to it in between. Merging bits here turned a heartbeat's 0xFF into 0xF7.
    #[test]
    fn outside_the_flag_ranges_the_client_value_lands_as_is() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x3000]);
        cart.write_many(&[(0x3001, &[0xC6])]).unwrap();
        snap.handle("WRITE_CORE_MEMORY A0003000 00 00 FF 00", None);
        snap.refresh(&mut cart, &[FLAGS]).unwrap();
        assert_eq!(word(&mut cart, 0x3000)[1], 0xFF);
    }

    /// In a flag range the client sets one bit by rewriting the byte it read; a bit the game
    /// set in the meantime survives.
    #[test]
    fn inside_a_flag_range_only_the_client_bits_are_applied() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x1000]);
        cart.write_many(&[(0x1000, &[0b0000_0101])]).unwrap(); // the game sets bit 2
        snap.handle("WRITE_CORE_MEMORY A0001000 00 00 00 03", None); // client: bit 1 on byte 0x1000
        snap.refresh(&mut cart, &[FLAGS]).unwrap();
        assert_eq!(word(&mut cart, 0x1000)[0], 0b0000_0111);
    }

    /// The client reads back what it wrote before the cart has it.
    #[test]
    fn a_write_is_visible_to_the_next_read_at_once() {
        let mut cart = ram();
        let snap = primed(&mut cart, &[0x2000]);
        snap.handle("WRITE_CORE_MEMORY A0002000 01 02 03 04", None);
        assert_eq!(
            snap.handle("READ_CORE_MEMORY A0002000 4", None),
            "READ_CORE_MEMORY A0002000 01 02 03 04"
        );
    }

    #[test]
    fn merge_writes_nothing_for_an_unchanged_word() {
        let w = Write {
            phys: 0x1000,
            served: [1, 2, 3, 4],
            client: [1, 2, 3, 4],
        };
        assert!(merge(&w, None, &[FLAGS]).1.is_empty());
    }
}
