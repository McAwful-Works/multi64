//! M64P over multi64d's WebSocket.
//!
//! `multi64d` brokers a raw L3 octet stream on binary frames (`daemon-api-v1.md`).
//! We wrap M64P payloads in L3 DATA frames on the APPLICATION channel and pick our
//! replies back out of the stream.
//!
//! Synchronous on purpose: the caller is a connector script, which has nowhere useful
//! to await.

use std::io;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::time::{Duration, Instant};

use multi64_l3::{Channel, Frame, FrameFlags, FrameType, StreamDecoder};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

use crate::{m64p, Log};

/// How long to wait for the cart to answer one request.
///
/// This was 3 s, on the reasoning that "the Archipelago connector polls at 2 Hz, so
/// seconds of headroom cost nothing". Both halves are wrong for the clients AP64 actually
/// serves, and the pair of them cost the connection.
///
/// Archipelago's BizHawk Client polls far harder than 2 Hz -- measured over one session,
/// 16,437 requests in 1,492 s, about 11/s -- and it puts its OWN 5 s deadline on each one
/// (worlds/_bizhawk/__init__.py, `_send_message`). It logs nothing when that fires; it just
/// drops the socket and reconnects.
///
/// So this timeout is not free headroom, it is a budget shared with the client, and
/// [`super::backend::SOFT_RETRIES`] spends it up to three times before the reconnect loop is
/// even reached. At 3 s that is 9 s worst case against a 5 s deadline: any hiccup needing
/// two retries lost the client, by arithmetic rather than bad luck.
///
/// The penalty is quantized, which is what makes a smaller value safe. A tap between client
/// and AP64 timed 336 round trips: p50 10 ms, p90 84 ms, and NOTHING between 250 ms and
/// 3 s -- the slow ones sat at 3,078/3,086 ms and 6,053/6,082 ms, exactly one and two of
/// these timeouts. A reply is either serviced within a few frames or missed outright; there
/// is no slow-but-arriving regime for a longer wait to rescue.
///
/// 1.2 s is therefore ~5x the slowest reply ever actually observed to arrive, while
/// 3 x 1.2 = 3.6 s keeps the whole retry budget inside the client's 5 s. The reconnect loop
/// below is untouched: that is the real "cart is gone" path and should stay patient.
///
/// All of that is about a client waiting on the cart. A session whose client never does, one
/// AP64 answers from a snapshot, can spend more of these before reconnecting
/// (`Multi64::set_silence_budget`).
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_millis(1200);

pub struct Multi64Transport {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    decoder: StreamDecoder,
    /// APPLICATION payloads seen while waiting for some other reply.
    pending: Vec<Vec<u8>>,
    next_rid: u16,
    /// PEEKV/POKEV round trips issued. One request may cover many regions, so this
    /// is the number that actually costs latency -- report it rather than the
    /// region count, which flatters the result.
    requests: u32,
    /// Whether an echoed request has already been reported on this transport.
    /// Worth saying once -- it explains the timeout that follows -- and not
    /// worth saying for every read during a reset.
    echo_reported: bool,
    log: Log,
    pub rdram_bytes: u32,
    pub writable: bool,
    /// Whether the agent acknowledges a `POKEV` sent again without applying it twice
    /// (`HELLO_ACK` bit 3, spec 4.4). Only then may a write be sent more than once.
    pub poke_once: bool,
    /// The cart ROM window, when the agent answers `PEEKROM`.
    pub rom_bytes: Option<u32>,
    /// Slots the agent will watch at once, when it watches at all (spec 4.3).
    pub watch_slots: Option<u8>,
    /// What watched slots held between our reads, as responses brought it back.
    events: Vec<m64p::Event>,
    /// The agent's own dropped total, as last reported, and how much of it is new.
    ///
    /// The agent counts since the last `WATCH` and saturates, so this follows the rise
    /// rather than the value: what it reports is events known to have been lost, wherever
    /// they were lost. Once the agent's counter pins at its maximum, further losses there
    /// cannot be counted by anyone.
    dropped_seen: u16,
    dropped_new: u32,
    /// Requests sent again early ([`QUICK_COPY_MIN`]).
    quick_resends: u32,
    /// Whether the link brings each request to the agent whole ([`link_keeps_requests_whole`]),
    /// which an early copy needs as a write sent again does.
    whole_requests: bool,
    /// How long a round trip takes, smoothed over the requests answered on the first copy.
    srtt: Option<Duration>,
    rtt_samples: u32,
    /// When the agent last answered anything.
    last_answer: Option<Instant>,
    /// The request waiting for its answer, kept so it can be sent again early.
    outstanding: Option<Outstanding>,
}

/// A request sent and not yet answered.
struct Outstanding {
    rid: u16,
    app: Vec<u8>,
    sent: Instant,
    /// Whether this request may go twice: a read, or a write to an agent that recognizes a copy.
    may_copy: bool,
    copied: bool,
}

/// The shortest wait before a request is sent again early, and the longest.
///
/// Measured on a SummerCart64 in Ocarina of Time (2026-10-18): the agent handled every request,
/// and now and then its reply never reached the serial port, between 0.1% and 2% of requests
/// depending on where the game was. Every round trip that was answered took one game frame, 47 ms
/// at the median and 53 ms at the 99th percentile, so a reply that has not come in four of them,
/// from an agent that was answering every frame, has almost certainly been dropped. Sent again
/// then, the request costs about 0.2 s where waiting out [`REPLY_TIMEOUT`] cost 1.2, and three
/// dropped in a row no longer add up to a reconnect.
pub(crate) const QUICK_COPY_MIN: Duration = Duration::from_millis(150);
const QUICK_COPY_MAX: Duration = Duration::from_millis(600);
/// Round trips per early wait: room for a lag frame or two before a request goes twice.
const QUICK_COPY_RTTS: u32 = 4;
/// Round trips answered before one is trusted to say how long the next should take.
const QUICK_COPY_SAMPLES: u32 = 8;
/// The agent counts as answering every frame only if it answered this recently when the request
/// went out. Shorter than [`REPLY_TIMEOUT`], so the retry after a timeout, at a scene load say, is
/// never sent twice: one early copy per silence, not one per retry.
const QUICK_COPY_LIVE: Duration = Duration::from_millis(1000);

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// The spec §5 code of an `ERR` the agent answered, when that is what `e` is.
///
/// An agent that refuses has answered: it is there and serving, and the same request gets the
/// same answer however often it is sent or on however many links. `E_BUSY` is not one of these
/// once [`Multi64Transport::peek_rom`] has turned it into a `WouldBlock`.
pub fn refused(e: &io::Error) -> Option<u8> {
    match e.get_ref()?.downcast_ref::<m64p::Error>()? {
        m64p::Error::Refused { code, .. } => Some(*code),
        _ => None,
    }
}

/// The response [`Multi64Transport::await_response`] waits for.
#[derive(Debug, Clone, Copy)]
enum Want {
    /// `HELLO_ACK`, which carries no rid. Anything else is a late answer to a request sent
    /// before: multi64d hands every client the whole stream from the cart, so a link rebuilt
    /// after a stall hears the old link's late replies first.
    Hello,
    /// The answer to the request sent as this rid, an `ERR` included.
    Rid(u16),
}

impl Want {
    fn matches(self, resp: &m64p::Response) -> bool {
        match self {
            Want::Hello => matches!(resp, m64p::Response::HelloAck { .. }),
            Want::Rid(rid) => resp.rid() == Some(rid),
        }
    }
}

/// A reply holds a block for every region asked for, each exactly as long as asked.
///
/// The reply carries its own lengths, so a block can parse cleanly and still be short. Every
/// caller slices what comes back by the length it asked for, and a short block taken as an
/// answer panicked the thread that took it, which in AP64 is the whole session.
fn check_regions(what: &str, asked: &[m64p::Region], got: &[Vec<u8>]) -> io::Result<()> {
    if got.len() != asked.len() {
        return Err(other(format!(
            "asked for {} {what}regions, cart returned {}",
            asked.len(),
            got.len()
        )));
    }
    for (r, block) in asked.iter().zip(got) {
        if block.len() != r.len as usize {
            return Err(other(format!(
                "asked for {} bytes at {what}0x{:X}, cart returned {}",
                r.len,
                r.addr,
                block.len()
            )));
        }
    }
    Ok(())
}

/// Which cart the daemon was started for (`sc64`, `ed64` or `ed64pro`: `multi64d --cart`), or
/// `None` when it does not answer. AP64 defaults its patch to it, so the agent in the ROM talks
/// to the cart Multi64 is set up for.
pub fn daemon_cart(ws_url: &str) -> Option<String> {
    daemon_status(ws_url)?.cart
}

/// What one `GET /` of the daemon says, for a caller that wants both answers from one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    /// Whether the daemon holds the cart's serial port. Without it, a released or absent link
    /// looks exactly like a ROM with no agent, because both are just a timeout. `None` when the
    /// answer does not say.
    pub serial_active: Option<bool>,
    /// See [`daemon_cart`]. `None` when the answer does not say.
    pub cart: Option<String>,
}

/// The daemon's `GET /`, read once, or `None` when it does not answer at all.
///
/// A raw GET rather than an HTTP client dependency. `None` is a different problem from either
/// answer: it is the one thing that tells "Multi64 is not running" from "the cart has not
/// answered yet", and a caller reporting those separately needs it.
pub fn daemon_status(ws_url: &str) -> Option<DaemonStatus> {
    Some(parse_status(&daemon_root(ws_url)?))
}

fn parse_status(body: &str) -> DaemonStatus {
    let serial_active = if body.contains("\"serialActive\":true") {
        Some(true)
    } else if body.contains("\"serialActive\":false") {
        Some(false)
    } else {
        None
    };
    let cart = body.find("\"cart\":\"").and_then(|at| {
        let rest = &body[at + "\"cart\":\"".len()..];
        Some(rest[..rest.find('"')?].to_string())
    });
    DaemonStatus {
        serial_active,
        cart,
    }
}

/// Whether the cart link brings each request to the agent whole, so that a write sent again
/// (`HELLO_ACK` bit 3, spec 4.4) can only ever arrive as itself.
///
/// The SummerCart64's does. An EverDrive's delivers a request in pieces, and a piece lost in a way
/// its driver cannot see leaves the agent filling that request out with whatever arrives next, for
/// as long as anything keeps arriving within 60 of its ticks (`docs/integration/cart-agent.md`).
/// A copy of a write sent in that window would be read as the rest of the broken request and
/// written under its `rid`, with bytes that are not the write's. A cart this does not know counts
/// as an EverDrive.
fn link_keeps_requests_whole(cart: Option<&str>) -> bool {
    cart == Some("sc64")
}

/// How long to wait for the daemon to accept a connection.
///
/// A plain connect to a loopback port nobody listens on takes about 2 s to be refused on Windows,
/// and Multi64 not running is the common case this is asked to tell apart: AP64 asks at every
/// launch. One that is accepted completes in well under a millisecond, and across a LAN in tens.
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// The daemon's `GET /`, response and all.
fn daemon_root(ws_url: &str) -> Option<String> {
    let hostport = ws_url.strip_prefix("ws://")?.split('/').next()?.to_string();
    let mut s = hostport
        .to_socket_addrs()
        .ok()?
        .find_map(|addr| TcpStream::connect_timeout(&addr, DAEMON_CONNECT_TIMEOUT).ok())?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let req = format!("GET / HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut body = String::new();
    s.read_to_string(&mut body).ok()?;
    Some(body)
}

impl Multi64Transport {
    /// Connect and complete the M64P `HELLO` exchange.
    ///
    /// A successful connect proves only that the daemon is up; the `HELLO_ACK` is
    /// what proves a cart is present and running an agent.
    pub fn connect(url: &str, log: Log) -> io::Result<Self> {
        let status = daemon_status(url);
        if status.as_ref().and_then(|s| s.serial_active) == Some(false) {
            return Err(other(format!(
                "{url}: daemon is up but holds no serial link (serialActive=false). \
                 Connect the cart, or POST /v1/serial/resume if the port was released."
            )));
        }

        // No Origin header: the daemon allows origin-less native clients and 403s
        // anything else that is not allow-listed (daemon-api-v1.md 1.4).
        let (ws, _resp) =
            tungstenite::connect(url).map_err(|e| other(format!("connect {url}: {e}")))?;

        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(REPLY_TIMEOUT))
                .map_err(|e| other(format!("set read timeout: {e}")))?;
        }

        let mut t = Self {
            ws,
            decoder: StreamDecoder::new(),
            pending: Vec::new(),
            next_rid: 1,
            requests: 0,
            echo_reported: false,
            log,
            rdram_bytes: 0,
            writable: false,
            poke_once: false,
            rom_bytes: None,
            watch_slots: None,
            events: Vec::new(),
            dropped_seen: 0,
            dropped_new: 0,
            quick_resends: 0,
            whole_requests: false,
            srtt: None,
            rtt_samples: 0,
            last_answer: None,
            outstanding: None,
        };

        t.send_app(&m64p::encode_hello())?;
        match t.await_response(Want::Hello)? {
            m64p::Response::HelloAck {
                proto,
                rdram_bytes,
                flags,
                rom_bytes,
                watch_slots,
                ..
            } => {
                if proto != 0 {
                    return Err(other(format!(
                        "cart speaks M64P proto {proto}, this client speaks 0"
                    )));
                }
                t.rdram_bytes = rdram_bytes;
                t.writable = flags & m64p::FLAG_WRITABLE != 0;
                let cart = status.and_then(|s| s.cart);
                t.whole_requests = link_keeps_requests_whole(cart.as_deref());
                t.poke_once = flags & m64p::FLAG_POKE_ONCE != 0;
                if t.poke_once && !t.whole_requests {
                    t.poke_once = false;
                    (t.log)(format!(
                        "the agent can take a write sent again, but over {} a copy could be \
                         joined to a request cut short, so each write is sent once",
                        cart.as_deref().unwrap_or("this cart")
                    ));
                }
                t.rom_bytes = rom_bytes;
                t.watch_slots = watch_slots;
                Ok(t)
            }
            other_msg => Err(other(format!("expected HELLO_ACK, got {other_msg:?}"))),
        }
    }

    pub fn requests(&self) -> u32 {
        self.requests
    }

    /// Requests sent again early ([`QUICK_COPY_MIN`]).
    pub fn quick_resends(&self) -> u32 {
        self.quick_resends
    }

    fn rid(&mut self) -> u16 {
        self.requests += 1;
        let r = self.next_rid;
        // 0 is reserved for "no request" in cart-side error paths.
        self.next_rid = self.next_rid.wrapping_add(1).max(1);
        r
    }

    fn send_app(&mut self, app: &[u8]) -> io::Result<()> {
        let frame = Frame {
            ty: FrameType::Data,
            channel: Channel::Application,
            flags: FrameFlags::FINAL,
            request_id: 0,
            payload: app.to_vec(),
        };
        let wire = frame
            .encode()
            .map_err(|e| other(format!("encode L3: {e:?}")))?;
        self.ws
            .send(Message::Binary(wire.into()))
            .map_err(|e| other(format!("ws send: {e}")))
    }

    /// Send request `rid` and keep it as the one waiting for its answer. With `may_copy` it may go
    /// again early, under the same rid ([`QUICK_COPY_MIN`]): a read, which a copy cannot change,
    /// or a write to an agent that recognizes one ([`Self::poke_once`]).
    fn send_request(&mut self, rid: u16, app: Vec<u8>, may_copy: bool) -> io::Result<()> {
        self.send_app(&app)?;
        self.outstanding = Some(Outstanding {
            rid,
            app,
            sent: Instant::now(),
            may_copy,
            copied: false,
        });
        Ok(())
    }

    /// When request `rid` goes again early, if it does.
    ///
    /// Only over a link that brings each request whole, as for a write sent again: over an
    /// EverDrive a copy could be joined to a request a lost piece cut short. Only once enough
    /// round trips have been answered to say how long one takes, and only when the agent had
    /// answered within [`QUICK_COPY_LIVE`] of this request going out: an agent gone quiet for a
    /// scene load gets one early copy, not one with every retry.
    fn quick_copy_at(&self, rid: u16) -> Option<Instant> {
        let o = self
            .outstanding
            .as_ref()
            .filter(|o| o.rid == rid && o.may_copy && !o.copied)?;
        if !self.whole_requests || self.rtt_samples < QUICK_COPY_SAMPLES {
            return None;
        }
        let live = self
            .last_answer
            .is_some_and(|t| o.sent.saturating_duration_since(t) < QUICK_COPY_LIVE);
        if !live {
            return None;
        }
        let wait = (self.srtt? * QUICK_COPY_RTTS).clamp(QUICK_COPY_MIN, QUICK_COPY_MAX);
        Some(o.sent + wait)
    }

    /// Send the request waiting for its answer again, byte for byte.
    fn send_quick_copy(&mut self) -> io::Result<()> {
        let Some(o) = self.outstanding.as_mut() else {
            return Ok(());
        };
        o.copied = true;
        let app = o.app.clone();
        self.quick_resends += 1;
        self.send_app(&app)
    }

    /// The agent answered `want`. A request answered on its first copy says how long a round
    /// trip takes; one sent twice cannot say which copy was answered.
    fn answered(&mut self, want: Want) {
        let now = Instant::now();
        self.last_answer = Some(now);
        let Want::Rid(rid) = want else {
            return;
        };
        if let Some(o) = self.outstanding.take_if(|o| o.rid == rid) {
            if !o.copied {
                let rtt = now.saturating_duration_since(o.sent);
                self.srtt = Some(match self.srtt {
                    None => rtt,
                    Some(s) => (s * 7 + rtt) / 8,
                });
                self.rtt_samples = self.rtt_samples.saturating_add(1);
            }
        }
    }

    /// Keep what a `PEEKV_RESP` brought back from the watch, whichever request it answers.
    ///
    /// The agent hands each event out once, on whichever response goes next, so a response
    /// thrown away is an event lost: one answering a read given up on, or one whose regions do
    /// not add up. Taken here, before anything decides whether the response is wanted.
    fn keep_watch_trailer(&mut self, resp: &mut m64p::Response) {
        if let m64p::Response::PeekV {
            events, dropped, ..
        } = resp
        {
            self.events.append(events);
            if *dropped > self.dropped_seen {
                self.dropped_new += u32::from(*dropped - self.dropped_seen);
                self.dropped_seen = *dropped;
            }
        }
    }

    /// Read until the M64P response `want` arrives.
    ///
    /// Anything else is dropped rather than queued: every caller here is synchronous and
    /// one-request-at-a-time, so a response to another rid answers a request we already gave up
    /// on, and keeping it would only desynchronize us further. What it carried from the watch is
    /// kept ([`Self::keep_watch_trailer`]).
    ///
    /// A request kept by [`Self::send_request`] may go again early, once ([`Self::quick_copy_at`]);
    /// the answer to either copy is the answer, and the deadline stays the first copy's.
    fn await_response(&mut self, want: Want) -> io::Result<m64p::Response> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut quick_at = match want {
            Want::Rid(rid) => self.quick_copy_at(rid),
            Want::Hello => None,
        };

        loop {
            // Drain anything already decoded before going back to the socket.
            while let Some(app) = self.pending.pop() {
                match m64p::parse(&app) {
                    Ok(mut resp) => {
                        self.keep_watch_trailer(&mut resp);
                        if want.matches(&resp) {
                            self.answered(want);
                            return match resp {
                                m64p::Response::Err { rid, code } => {
                                    Err(io::Error::other(m64p::Error::Refused { rid, code }))
                                }
                                ok => Ok(ok),
                            };
                        }
                    }
                    // M64T and other traffic share the channel; skip quietly.
                    Err(m64p::Error::NotM64p) => {}
                    // Our own request read back, because the agent has not
                    // written a reply over it. That is not a reply either, so
                    // keep waiting and let the deadline below decide -- which
                    // yields a plain timeout, and a timeout is soft-retried on
                    // this transport instead of rebuilding it.
                    //
                    // Treating it as a bad payload is what made every console
                    // reset cost a reconnect, and a reconnect redoes HELLO and
                    // drops the Archipelago client too.
                    Err(m64p::Error::EchoedRequest(m)) => {
                        if !self.echo_reported {
                            self.echo_reported = true;
                            (self.log)(format!(
                                "cart echoed our request 0x{m:02X}: the agent is not \
                                 answering (reset or loading), waiting it out"
                            ));
                        }
                    }
                    Err(e) => return Err(other(format!("bad M64P payload: {e}"))),
                }
            }

            if quick_at.is_some_and(|q| Instant::now() >= q) {
                quick_at = None;
                self.send_quick_copy()?;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no M64P reply within timeout; is the ROM with the agent running? \
                     (The agent also goes quiet for good if its code in RAM is overwritten.)",
                ));
            }

            // The socket carries a read timeout so a silent cart cannot wedge us.
            // That surfaces here as an IO error, which is not a failure -- it just
            // means nothing arrived yet, so let the deadline above decide.
            //
            // Each read waits only for what is left of the deadline. With the whole of it, a read
            // that came back a moment early (Windows rounds socket timeouts to its timer tick)
            // found the deadline not quite passed and waited the whole of it again, so one
            // attempt could take twice REPLY_TIMEOUT, and its retries outlast the client.
            // Or for what is left until the early copy is due.
            let until = quick_at.map_or(deadline, |q| q.min(deadline));
            let left = until.saturating_duration_since(Instant::now());
            if let MaybeTlsStream::Plain(s) = self.ws.get_ref() {
                s.set_read_timeout(Some(left.max(Duration::from_millis(1))))
                    .map_err(|e| other(format!("set read timeout: {e}")))?;
            }
            let msg = match self.ws.read() {
                Ok(m) => m,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(other(format!("ws read: {e}"))),
            };
            match msg {
                Message::Binary(bytes) => {
                    let mut apps = Vec::new();
                    self.decoder.push_bytes(&bytes, |f: Frame| {
                        if f.ty == FrameType::Data && f.channel == Channel::Application {
                            apps.push(f.payload);
                        }
                    });
                    // Reversed so the pop() loop above consumes them in arrival order.
                    apps.reverse();
                    self.pending = apps;
                }
                // Text frames are the daemon's own JSON control messages (hello,
                // ping/pong); they are not part of the L3 stream.
                Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => return Err(other("daemon closed the WebSocket")),
                Message::Frame(_) => {}
            }
        }
    }

    /// One vectored read. This is the whole reason M64P batches.
    pub fn peek(&mut self, regions: &[m64p::Region]) -> io::Result<Vec<Vec<u8>>> {
        if regions.is_empty() {
            return Ok(Vec::new());
        }
        let rid = self.rid();
        let app = m64p::encode_peekv(rid, regions).map_err(|e| other(e.to_string()))?;
        self.send_request(rid, app, true)?;
        match self.await_response(Want::Rid(rid))? {
            // What it brought back from the watch is already kept: see keep_watch_trailer.
            m64p::Response::PeekV { regions: got, .. } => {
                check_regions("", regions, &got)?;
                Ok(got)
            }
            other_msg => Err(other(format!("expected PEEKV_RESP, got {other_msg:?}"))),
        }
    }

    /// Ask the cart to watch `slots` from now on, replacing what it watched before.
    ///
    /// An empty list stops it watching. Fails if the agent does not watch slots at all,
    /// which a caller should check through `watch_slots` first, and with a [`refused`] error
    /// if the agent will not watch these.
    pub fn watch(&mut self, slots: &[m64p::Slot]) -> io::Result<u8> {
        let rid = self.rid();
        let app = m64p::encode_watch(rid, slots).map_err(|e| other(e.to_string()))?;
        // Never twice: the agent starts a new history on each WATCH, and a copy would clear it.
        self.send_request(rid, app, false)?;
        match self.await_response(Want::Rid(rid))? {
            m64p::Response::WatchAck { watching, .. } => {
                // The agent starts a new history on WATCH, counters included.
                self.events.clear();
                self.dropped_seen = 0;
                Ok(watching)
            }
            other_msg => Err(other(format!("expected WATCH_ACK, got {other_msg:?}"))),
        }
    }

    /// Everything the watched slots held since this was last called, oldest first, and
    /// how many the agent's own queue lost in that time.
    pub fn take_events(&mut self) -> (Vec<m64p::Event>, u32) {
        (
            std::mem::take(&mut self.events),
            std::mem::take(&mut self.dropped_new),
        )
    }

    /// One vectored read of the cartridge ROM (`PEEKROM`).
    ///
    /// A cart that could not get the PI bus answers `E_BUSY`, reported as `WouldBlock` so a
    /// caller can tell "try again" from a real refusal.
    pub fn peek_rom(&mut self, regions: &[m64p::Region]) -> io::Result<Vec<Vec<u8>>> {
        if regions.is_empty() {
            return Ok(Vec::new());
        }
        if self.rom_bytes.is_none() {
            return Err(other(
                "this cart agent cannot read the cart ROM (it predates PEEKROM)",
            ));
        }
        let rid = self.rid();
        let app = m64p::encode_peekrom(rid, regions).map_err(|e| other(e.to_string()))?;
        self.send_request(rid, app, true)?;
        match self.await_response(Want::Rid(rid)) {
            Ok(m64p::Response::PeekRom { regions: got, .. }) => {
                check_regions("ROM ", regions, &got)?;
                Ok(got)
            }
            Ok(other_msg) => Err(other(format!("expected PEEKROM_RESP, got {other_msg:?}"))),
            Err(e) if refused(&e) == Some(m64p::E_BUSY) => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, e.to_string()))
            }
            Err(e) => Err(e),
        }
    }

    /// One vectored write, sent and acknowledged.
    pub fn poke(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let rid = self.send_poke(writes)?;
        self.await_poke(rid, writes.len())
    }

    /// Send a `POKEV` without waiting for it, and return its rid for [`Self::await_poke`].
    ///
    /// Apart from [`Self::poke`] because a write may not simply be sent again. A request the host
    /// gave up on may still be served, and a second copy lands on the frame after it: a mailbox
    /// the game emptied in between would be filled again. Only an agent with
    /// [`Self::poke_once`] recognizes the copy ([`Self::resend_poke`]); for any other, waiting
    /// longer for the first copy's reply is the only retry that cannot do that.
    pub fn send_poke(&mut self, writes: &[(u32, &[u8])]) -> io::Result<u16> {
        if !self.writable {
            return Err(other("cart reports a read-only agent"));
        }
        let rid = self.rid();
        let app = m64p::encode_pokev(rid, writes).map_err(|e| other(e.to_string()))?;
        let copy = self.poke_once;
        self.send_request(rid, app, copy)?;
        Ok(rid)
    }

    /// Send the `POKEV` [`Self::send_poke`] sent as `rid` again, byte for byte, to an agent that
    /// acknowledges a copy without applying it twice ([`Self::poke_once`], spec 4.4).
    ///
    /// The agent holds only the request it served last, so the copy must go before anything
    /// else does: the caller sends nothing on this transport between the two.
    pub fn resend_poke(&mut self, rid: u16, writes: &[(u32, &[u8])]) -> io::Result<()> {
        if !self.poke_once {
            return Err(other(
                "this cart agent cannot tell a write sent again from a new one, so it is not \
                 sent again",
            ));
        }
        let app = m64p::encode_pokev(rid, writes).map_err(|e| other(e.to_string()))?;
        self.send_app(&app)
    }

    /// Wait one [`REPLY_TIMEOUT`] for the `POKE_ACK` to `rid`, a `POKEV` of `count` regions.
    pub fn await_poke(&mut self, rid: u16, count: usize) -> io::Result<()> {
        match self.await_response(Want::Rid(rid))? {
            m64p::Response::PokeAck { applied, .. } => {
                if applied as usize != count {
                    return Err(other(format!(
                        "sent {count} writes, cart applied {applied}"
                    )));
                }
                Ok(())
            }
            other_msg => Err(other(format!("expected POKE_ACK, got {other_msg:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_both_fields_from_one_answer() {
        let body = "HTTP/1.1 200 OK\r\n\r\n{\"service\":\"multi64d\",\"serial\":\"COM4\",\
                    \"serialActive\":true,\"serialBusy\":false,\"cart\":\"ed64\"}";
        assert_eq!(
            parse_status(body),
            DaemonStatus {
                serial_active: Some(true),
                cart: Some("ed64".into()),
            }
        );
    }

    #[test]
    fn a_reply_must_hold_every_region_at_the_length_asked() {
        let asked = [
            m64p::Region { addr: 0x10, len: 4 },
            m64p::Region { addr: 0x20, len: 2 },
        ];
        assert!(check_regions("", &asked, &[vec![0; 4], vec![0; 2]]).is_ok());
        let short = check_regions("", &asked, &[vec![0; 4], vec![0; 1]]).unwrap_err();
        assert!(
            short
                .to_string()
                .contains("2 bytes at 0x20, cart returned 1"),
            "{short}"
        );
        assert!(check_regions("ROM ", &asked, &[vec![0; 4], vec![0; 3]]).is_err());
        assert!(check_regions("", &asked, &[vec![0; 4]]).is_err());
    }

    #[test]
    fn status_from_an_older_daemon_has_no_cart() {
        let status = parse_status("{\"serialActive\":false}");
        assert_eq!(status.serial_active, Some(false));
        assert_eq!(status.cart, None);
        assert_eq!(parse_status("not json at all").serial_active, None);
    }
}
