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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Listening(u16),
    ClientConnected(SocketAddr),
    ClientDisconnected(String),
    /// A request line was answered.
    Handled,
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

    while !stop.load(Ordering::Relaxed) {
        let mut busy = false;
        match accept_newest(&listeners) {
            Ok(Some((stream, addr))) => match Client::new(stream) {
                Ok(c) => {
                    if client.is_some() {
                        on_event(Event::ClientDisconnected(
                            "replaced by a newer connection".into(),
                        ));
                    }
                    client = Some(c);
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
                    client = None;
                    on_event(Event::ClientDisconnected(why));
                    continue;
                }
            };
            for line in lines {
                busy = true;
                let reply = connector.handle(&line)?;
                if let Err(why) = c.send(&reply) {
                    client = None;
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
                client = None;
                on_event(Event::ClientDisconnected("client timed out".into()));
            }
        }

        if !busy {
            std::thread::sleep(IDLE);
        }
    }
    Ok(())
}
