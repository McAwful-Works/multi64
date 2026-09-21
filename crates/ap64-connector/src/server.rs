//! The TCP side: where Archipelago's client finds the connector.
//!
//! The client scans a port range on localhost and speaks newline-delimited text. This
//! binds the first free port in the range (IPv4, and IPv6 where it can, since
//! "localhost" may resolve to either), accepts one client at a time, and hands each
//! line to the [`Connector`].
//!
//! A newer connection replaces the current one. A client that restarts is often
//! seen connecting again before its old socket is noticed closed, and the old one
//! will never speak again.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Connector;

const IDLE: Duration = Duration::from_millis(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to read a watched slot while the client has nothing to say.
///
/// A slot the game rewrites between polls is missed by as much as it goes unread, and a
/// session waiting on its client is not using the cart for anything else. 250 ms buys
/// several extra looks per poll out of time that was being spent in `IDLE` sleeps.
const SAMPLE_EVERY: Duration = Duration::from_millis(250);

/// How often [`Event::Idle`] is raised while a session is quiet.
const IDLE_EVENT_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Listening(u16),
    ClientConnected(SocketAddr),
    ClientDisconnected(String),
    /// A request line was answered.
    Handled,
    /// Nothing has happened for a moment.
    ///
    /// Emitted while a session is up and quiet, so a caller can check on things that only
    /// change when nobody is asking: a console that was reset answers nothing, and silence on
    /// its own is indistinguishable from a client that simply has nothing to say.
    Idle,
}

struct Listeners {
    port: u16,
    sockets: Vec<TcpListener>,
}

fn bind(ports: &[u16]) -> io::Result<Listeners> {
    let mut last = None;
    for &port in ports {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(v4) => {
                v4.set_nonblocking(true)?;
                let mut sockets = vec![v4];
                // Best effort: a machine without IPv6 still works over IPv4.
                if let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
                    if v6.set_nonblocking(true).is_ok() {
                        sockets.push(v6);
                    }
                }
                return Ok(Listeners { port, sockets });
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no ports to listen on")))
        .map_err(|e| io::Error::new(e.kind(), format!("every port in {ports:?} is in use: {e}")))
}

/// The newest pending connection on any listener, if one arrived.
fn accept_newest(l: &Listeners) -> io::Result<Option<(TcpStream, SocketAddr)>> {
    let mut newest = None;
    for s in &l.sockets {
        loop {
            match s.accept() {
                Ok(c) => newest = Some(c),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(newest)
}

struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
    last_heard: Instant,
}

/// Every connection this server lets go of is reset rather than closed politely.
///
/// Archipelago's clients read a line and hand it straight to `json.loads`. A clean close
/// delivers the empty string, and the decode error that follows is not one their socket
/// task catches: it dies without a word and the client goes on showing itself connected
/// until someone restarts it. A reset arrives as `ConnectionResetError`, which they do
/// catch, log as "Read failed due to Connection Lost, Reconnecting", and recover from on
/// their own. `SO_LINGER` of zero is what makes the close send an RST -- an explicit
/// `shutdown` would send a FIN whatever the linger says, which is the polite close being
/// avoided here.
///
/// On the paths where the peer has already gone this changes nothing, and on the ones
/// where it has not -- a session stopping, a client dropped for going silent -- it is the
/// difference between a client that comes back by itself and one that has to be restarted.
impl Drop for Client {
    fn drop(&mut self) {
        let sock = socket2::SockRef::from(&self.stream);
        let _ = sock.set_linger(Some(Duration::ZERO));
    }
}

impl Client {
    fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            buf: Vec::new(),
            last_heard: Instant::now(),
        })
    }

    /// Complete lines received so far; `Err` once the peer has gone.
    fn lines(&mut self) -> Result<Vec<String>, String> {
        let mut chunk = [0u8; 8192];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("client closed the connection".into()),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    self.last_heard = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("client connection: {e}")),
            }
        }
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            out.push(text.trim_end_matches('\r').to_string());
        }
        Ok(out)
    }

    /// End the connection now, with a reset the client can act on.
    ///
    /// A *reset* specifically, not a polite close. Archipelago's clients read a line and
    /// hand it straight to `json.loads`; a clean shutdown delivers the empty string, which
    /// raises a decode error their socket task does not catch, so it dies silently and the
    /// client goes on showing itself connected until it is restarted. A reset arrives as
    /// `ConnectionResetError`, which they do catch ("Read failed due to Connection Lost,
    /// Reconnecting"), and they reconnect by themselves once a session is running again.
    ///
    /// Note that an unanswered request still sitting unread in the receive buffer resets on
    /// any close -- which is exactly why this was easy to miss, since the case that matters
    /// is the one where the line had been read and was never going to be answered.
    ///
    /// The reset itself is in [`Drop`], so that no path can hand a client anything else.
    /// Taking `self` by value is what this method is for: it ends the connection here,
    /// rather than whenever the caller's binding happens to go out of scope.
    fn close(self) {}

    fn send(&mut self, line: &str) -> Result<(), String> {
        // Blocking with a deadline for the write: replies are small, and a client
        // that stops reading is as good as gone.
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        self.stream
            .set_nonblocking(false)
            .and_then(|_| self.stream.set_write_timeout(Some(SEND_TIMEOUT)))
            .and_then(|_| self.stream.write_all(&data))
            .and_then(|_| self.stream.set_nonblocking(true))
            .map_err(|e| format!("sending to the client: {e}"))?;
        // The client's silence is timed from the reply, not its request: a slow poll
        // over the cart is not the client going quiet.
        self.last_heard = Instant::now();
        Ok(())
    }
}

/// Serve `connector` until `stop` is set or the cart fails. Returns `Err` only for a
/// failure that ends the session (no port to bind, or a fatal cart error).
pub fn serve(
    connector: &Connector,
    ports: &[u16],
    stop: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
) -> Result<(), String> {
    let listeners =
        bind(ports).map_err(|e| format!("listening for the Archipelago client: {e}"))?;
    on_event(Event::Listening(listeners.port));
    let mut client: Option<Client> = None;

    // Whatever ends this loop -- a stop, or a cart failure raised from a request -- the client
    // is told by closing the socket under it, not left waiting for a reply.
    let result = serve_until(connector, &listeners, stop, on_event, &mut client);
    if let Some(c) = client.take() {
        c.close();
    }
    result
}

fn serve_until(
    connector: &Connector,
    listeners: &Listeners,
    stop: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
    client: &mut Option<Client>,
) -> Result<(), String> {
    let mut sampled = Instant::now();
    let mut idled = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut busy = false;
        match accept_newest(listeners) {
            Ok(Some((stream, addr))) => match Client::new(stream) {
                Ok(c) => {
                    if client.is_some() {
                        on_event(Event::ClientDisconnected(
                            "replaced by a newer connection".into(),
                        ));
                    }
                    *client = Some(c);
                    on_event(Event::ClientConnected(addr));
                }
                Err(e) => on_event(Event::ClientDisconnected(format!("accepting: {e}"))),
            },
            Ok(None) => {}
            Err(e) => on_event(Event::ClientDisconnected(format!("accepting: {e}"))),
        }

        if let Some(c) = client.as_mut() {
            let lines = match c.lines() {
                Ok(lines) => lines,
                Err(why) => {
                    *client = None;
                    on_event(Event::ClientDisconnected(why));
                    continue;
                }
            };
            for line in lines {
                // A batch can hold several lines and each one is a round trip to the cart.
                // Once a stop has been asked for, the rest are work nobody wants.
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                busy = true;
                let reply = connector.handle(&line)?;
                if let Err(why) = c.send(&reply) {
                    *client = None;
                    on_event(Event::ClientDisconnected(why));
                    break;
                }
                on_event(Event::Handled);
            }
            let timeout = connector.script().client_timeout;
            if client
                .as_ref()
                .zip(timeout)
                .is_some_and(|(c, t)| c.last_heard.elapsed() > t)
            {
                *client = None;
                on_event(Event::ClientDisconnected("client timed out".into()));
            }
        }

        if busy {
            continue;
        }
        // Only while a client is connected: with nobody listening there is nothing a
        // replayed event could be handed to, and the cart is better left alone.
        if client.is_some() && sampled.elapsed() >= SAMPLE_EVERY {
            sampled = Instant::now();
            connector.sample_watch()?;
            continue;
        }
        if idled.elapsed() >= IDLE_EVENT_EVERY {
            idled = Instant::now();
            on_event(Event::Idle);
            continue;
        }
        std::thread::sleep(IDLE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read whatever the client has said, giving it a moment to arrive.
    fn drain(c: &mut Client) -> Vec<String> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let lines = c.lines().expect("peer still connected");
            if !lines.is_empty() || Instant::now() >= until {
                return lines;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Ending a session has to reset the connection, not close it politely.
    ///
    /// Archipelago's OoT Client reads with `readline()` and hands the result straight to
    /// `json.loads`. A clean close gives it `b""`, and `json.loads("")` raises
    /// `JSONDecodeError` -- which its N64 task does not catch, so the task dies without a
    /// word and the client goes on showing itself connected, with no way back but a
    /// restart. A reset raises `ConnectionResetError`, which it does catch, logs as
    /// "Read failed due to Connection Lost, Reconnecting", and recovers from by itself.
    #[test]
    fn closing_a_client_resets_it_rather_than_ending_the_stream() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (served, _) = listener.accept().unwrap();
        let mut client = Client::new(served).unwrap();

        // The request has to be consumed first. An *unread* request resets on any close,
        // which is what hid this: the case that matters is the one where the line was
        // taken and never answered, which is what Stop does mid-request.
        peer.write_all(b"poll\n").unwrap();
        assert_eq!(drain(&mut client).len(), 1);

        client.close();

        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 64];
        match peer.read(&mut buf) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Ok(0) => panic!(
                "clean end of stream: OoT Client turns this into an uncaught \
                 JSONDecodeError and never reconnects"
            ),
            other => panic!("expected a reset, got {other:?}"),
        }
    }

    /// The same has to hold for a client we let go of without calling `close`: one
    /// replaced by a newer connection, or dropped for going silent. Those reach the peer
    /// through `Drop` alone, and a polite close there is the same silent death.
    #[test]
    fn dropping_a_client_resets_it_too() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (served, _) = listener.accept().unwrap();
        let mut client = Client::new(served).unwrap();
        peer.write_all(b"poll\n").unwrap();
        assert_eq!(drain(&mut client).len(), 1);

        drop(client);

        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(
            peer.read(&mut buf).map_err(|e| e.kind()),
            Err(io::ErrorKind::ConnectionReset),
        );
    }
}
