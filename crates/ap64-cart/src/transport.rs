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
use std::net::TcpStream;
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
}

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
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

/// Ask the daemon whether it is currently holding the cart's serial port.
///
/// A raw loopback GET rather than an HTTP client dependency. Worth the few lines:
/// without it a released or absent link looks exactly like a ROM that is simply not
/// in MEM_AGENT mode, because both are just a timeout.
///
/// `None` means the daemon did not answer at all, which is a different problem from either:
/// it is the one thing that distinguishes "Multi64 is not running" from "the cart has not
/// answered yet", and a caller reporting those separately needs it.
pub fn serial_active(ws_url: &str) -> Option<bool> {
    daemon_status(ws_url)?.serial_active
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
    /// See [`serial_active`]. `None` when the answer does not say.
    pub serial_active: Option<bool>,
    /// See [`daemon_cart`]. `None` when the answer does not say.
    pub cart: Option<String>,
}

/// The daemon's `GET /`, read once, or `None` when it does not answer at all.
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
/// written under its `rid`, with bytes that are not the write's. A cart this does not know counts as
/// an EverDrive.
fn link_keeps_requests_whole(cart: Option<&str>) -> bool {
    cart == Some("sc64")
}

/// The daemon's `GET /`, response and all.
fn daemon_root(ws_url: &str) -> Option<String> {
    let hostport = ws_url.strip_prefix("ws://")?.split('/').next()?.to_string();
    let mut s = TcpStream::connect(&hostport).ok()?;
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
        };

        t.send_app(&m64p::encode_hello())?;
        match t.await_response(None)? {
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
                t.poke_once = flags & m64p::FLAG_POKE_ONCE != 0;
                if t.poke_once && !link_keeps_requests_whole(cart.as_deref()) {
                    t.poke_once = false;
                    (t.log)(format!(
                        "the agent can take a write sent again, but over {} a copy could be                          joined to a request cut short, so each write is sent once",
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

    /// Read until an M64P response arrives, optionally requiring a specific `rid`.
    ///
    /// Replies for other rids are dropped rather than queued: every caller here is
    /// synchronous and one-request-at-a-time, so an unmatched rid means a reply to a
    /// request we already gave up on, and keeping it would only desynchronize us
    /// further.
    fn await_response(&mut self, want_rid: Option<u16>) -> io::Result<m64p::Response> {
        let deadline = Instant::now() + REPLY_TIMEOUT;

        loop {
            // Drain anything already decoded before going back to the socket.
            while let Some(app) = self.pending.pop() {
                match m64p::parse(&app) {
                    Ok(resp) => {
                        if want_rid.is_none() || resp.rid().is_none() || resp.rid() == want_rid {
                            return match resp {
                                m64p::Response::Err { rid, code } => {
                                    Err(other(m64p::Error::Refused { rid, code }.to_string()))
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
        self.send_app(&app)?;
        match self.await_response(Some(rid))? {
            m64p::Response::PeekV {
                regions: got,
                events,
                dropped,
                ..
            } => {
                check_regions("", regions, &got)?;
                // Only ever non-empty once this host has set a watch, and it rides the
                // response it was already waiting for. See take_events.
                self.events.extend(events);
                if dropped > self.dropped_seen {
                    self.dropped_new += u32::from(dropped - self.dropped_seen);
                    self.dropped_seen = dropped;
                }
                Ok(got)
            }
            other_msg => Err(other(format!("expected PEEKV_RESP, got {other_msg:?}"))),
        }
    }

    /// Ask the cart to watch `slots` from now on, replacing what it watched before.
    ///
    /// An empty list stops it watching. Fails if the agent does not watch slots at all,
    /// which a caller should check through `watch_slots` first.
    pub fn watch(&mut self, slots: &[m64p::Slot]) -> io::Result<u8> {
        let rid = self.rid();
        let app = m64p::encode_watch(rid, slots).map_err(|e| other(e.to_string()))?;
        self.send_app(&app)?;
        match self.await_response(Some(rid))? {
            m64p::Response::WatchAck { watching, .. } => {
                // The agent starts a new history on WATCH, counters included.
                self.events.clear();
                self.dropped_seen = 0;
                Ok(watching)
            }
            m64p::Response::Err { code, .. } => Err(other(format!(
                "the cart refused to watch {} slot(s): M64P error 0x{code:02X}",
                slots.len()
            ))),
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
        self.send_app(&app)?;
        match self.await_response(Some(rid)) {
            Ok(m64p::Response::PeekRom { regions: got, .. }) => {
                check_regions("ROM ", regions, &got)?;
                Ok(got)
            }
            Ok(other_msg) => Err(other(format!("expected PEEKROM_RESP, got {other_msg:?}"))),
            Err(e) if e.to_string().contains(m64p::err_name(m64p::E_BUSY)) => {
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
        self.send_app(&app)?;
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
        match self.await_response(Some(rid))? {
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
