//! A fake multi64d with a scripted agent behind it, for driving [`Multi64`] the way a
//! console would: late, short, or not at all.
//!
//! It answers the daemon's `GET /` with `serialActive: true`, accepts WebSocket connections,
//! takes M64P requests out of L3 frames, records each one, and does what the test's agent
//! says: send a reply, wait, or drop the connection. Every connection is numbered from 0, so
//! a test can tell what reached the cart before a reconnect from what reached it after.
//!
//! [`Multi64`]: ap64_cart::backend::Multi64

#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ap64_cart::m64p;
use multi64_l3::{Channel, Frame, FrameFlags, FrameType, StreamDecoder};
use tungstenite::Message;

/// One M64P request as the fake cart received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Which connection it came on, from 0.
    pub conn: usize,
    pub msg: u8,
    pub rid: u16,
    /// `(addr, len)` for each region of a read.
    pub regions: Vec<(u32, u16)>,
    /// `(addr, bytes)` for each region of a write.
    pub writes: Vec<(u32, Vec<u8>)>,
}

/// What the fake agent does about a request.
pub enum Step {
    Send(Vec<u8>),
    Sleep(Duration),
    /// Drop the connection, as a daemon losing its serial port does.
    Close,
}

pub type Agent = Arc<dyn Fn(&Request) -> Vec<Step> + Send + Sync>;

pub struct Fake {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Request>>>,
}

impl Fake {
    /// Requests received so far, HELLOs included.
    pub fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// Requests of type `msg` received so far.
    pub fn seen_of(&self, msg: u8) -> Vec<Request> {
        self.seen().into_iter().filter(|r| r.msg == msg).collect()
    }
}

/// Start a fake daemon whose agent is `agent`. It runs until the test process ends.
pub fn start(agent: Agent) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let conns = Arc::new(Mutex::new(0usize));
    {
        let seen = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (agent, seen, conns) = (agent.clone(), seen.clone(), conns.clone());
                std::thread::spawn(move || serve(stream, agent, seen, conns));
            }
        });
    }
    Fake { url, seen }
}

/// The request head, without consuming it: a WebSocket upgrade is handed to tungstenite whole.
fn head(stream: &TcpStream) -> String {
    let mut buf = vec![0u8; 4096];
    for _ in 0..500 {
        let n = stream.peek(&mut buf).unwrap_or(0);
        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
        if text.contains("\r\n\r\n") {
            return text;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    String::new()
}

fn serve(
    mut stream: TcpStream,
    agent: Agent,
    seen: Arc<Mutex<Vec<Request>>>,
    conns: Arc<Mutex<usize>>,
) {
    if !head(&stream)
        .to_ascii_lowercase()
        .contains("upgrade: websocket")
    {
        // The daemon's status page, which the transport asks before connecting.
        let mut req = [0u8; 4096];
        let _ = stream.read(&mut req);
        let body = r#"{"service":"multi64d","serialActive":true,"cart":"sc64"}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        return;
    }
    let Ok(mut ws) = tungstenite::accept(stream) else {
        return;
    };
    let conn = {
        let mut n = conns.lock().unwrap();
        *n += 1;
        *n - 1
    };
    let mut decoder = StreamDecoder::new();
    loop {
        let bytes = match ws.read() {
            Ok(Message::Binary(b)) => b,
            Ok(_) => continue,
            Err(_) => return,
        };
        let mut apps = Vec::new();
        decoder.push_bytes(&bytes, |f: Frame| {
            if f.ty == FrameType::Data && f.channel == Channel::Application {
                apps.push(f.payload);
            }
        });
        for app in apps {
            let Some(req) = parse(conn, &app) else {
                continue;
            };
            seen.lock().unwrap().push(req.clone());
            for step in agent(&req) {
                match step {
                    Step::Send(payload) => {
                        let wire = frame(payload);
                        if ws.send(Message::Binary(wire.into())).is_err() {
                            return;
                        }
                    }
                    Step::Sleep(d) => std::thread::sleep(d),
                    Step::Close => return,
                }
            }
        }
    }
}

fn frame(payload: Vec<u8>) -> Vec<u8> {
    Frame {
        ty: FrameType::Data,
        channel: Channel::Application,
        flags: FrameFlags::FINAL,
        request_id: 0,
        payload,
    }
    .encode()
    .unwrap()
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn parse(conn: usize, app: &[u8]) -> Option<Request> {
    if app.len() < 5 || app[..4] != m64p::MAGIC {
        return None;
    }
    let msg = app[4];
    let mut req = Request {
        conn,
        msg,
        rid: 0,
        regions: Vec::new(),
        writes: Vec::new(),
    };
    if msg == m64p::MSG_HELLO {
        return Some(req);
    }
    let b = &app[5..];
    req.rid = be16(b);
    let n = b[2] as usize;
    let mut off = 3;
    for _ in 0..n {
        match msg {
            m64p::MSG_PEEKV | m64p::MSG_PEEKROM => {
                req.regions.push((be32(&b[off..]), be16(&b[off + 4..])));
                off += 6;
            }
            m64p::MSG_POKEV => {
                let (addr, len) = (be32(&b[off..]), be16(&b[off + 4..]) as usize);
                off += 6;
                req.writes.push((addr, b[off..off + len].to_vec()));
                off += len;
            }
            _ => break,
        }
    }
    Some(req)
}

fn head_of(msg: u8) -> Vec<u8> {
    let mut v = m64p::MAGIC.to_vec();
    v.push(msg);
    v
}

/// A writable SummerCart64 agent with 8 MiB and a 64 MiB ROM window, and no watch.
pub fn hello_ack() -> Vec<u8> {
    let mut v = head_of(m64p::MSG_HELLO_ACK);
    v.push(0); // proto
    v.extend_from_slice(&1u16.to_be_bytes()); // agent_ver
    v.extend_from_slice(&0x80_0000u32.to_be_bytes());
    v.push(m64p::FLAG_WRITABLE | m64p::FLAG_CART_ROM);
    v.extend_from_slice(&0x0400_0000u32.to_be_bytes());
    v
}

/// A `PEEKV_RESP` (or with `rom`, a `PEEKROM_RESP`) carrying `blocks` as given.
pub fn peek_resp(rid: u16, blocks: &[Vec<u8>], rom: bool) -> Vec<u8> {
    let mut v = head_of(if rom {
        m64p::MSG_PEEKROM_RESP
    } else {
        m64p::MSG_PEEKV_RESP
    });
    v.extend_from_slice(&rid.to_be_bytes());
    v.push(blocks.len() as u8);
    for b in blocks {
        v.extend_from_slice(&(b.len() as u16).to_be_bytes());
        v.extend_from_slice(b);
    }
    v
}

/// Every region asked for, filled with `fill`, at the length asked.
pub fn whole(req: &Request, fill: u8) -> Vec<Vec<u8>> {
    req.regions
        .iter()
        .map(|&(_, len)| vec![fill; len as usize])
        .collect()
}

pub fn poke_ack(rid: u16, applied: u8) -> Vec<u8> {
    let mut v = head_of(m64p::MSG_POKE_ACK);
    v.extend_from_slice(&rid.to_be_bytes());
    v.push(applied);
    v
}

/// What a working agent answers: HELLO, reads filled with `fill`, and every write applied.
pub fn answer(req: &Request, fill: u8) -> Vec<Step> {
    match req.msg {
        m64p::MSG_HELLO => vec![Step::Send(hello_ack())],
        m64p::MSG_PEEKV => vec![Step::Send(peek_resp(req.rid, &whole(req, fill), false))],
        m64p::MSG_PEEKROM => vec![Step::Send(peek_resp(req.rid, &whole(req, fill), true))],
        m64p::MSG_POKEV => vec![Step::Send(poke_ack(req.rid, req.writes.len() as u8))],
        _ => Vec::new(),
    }
}
