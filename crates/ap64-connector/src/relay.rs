//! A client that is itself the server: the world's own client listens, and its emulator
//! script connects to it and moves packets between the socket and two buffers in the game's
//! RAM. The ROM does all of the game's work. AP64 does what the script does, from the cart.
//!
//! Star Fox 64's world ([`crate::SF64`]) is the one this was written for. Its ROM publishes
//! the addresses of an input and an output buffer at `0x80400000` and `0x80400004`, and each
//! buffer holds one packet: `u16 size` (the bytes after it), `u16 cmd`, then data. Its Lua,
//! `connector_sf64_bizhawk.lua`, is the whole protocol:
//!
//! - **ROM to client.** When the output buffer's `cmd` is not zero, send `size + 2` bytes,
//!   then set `cmd` to zero. The ROM writes a new packet only once `cmd` is zero again.
//! - **Client to ROM.** When the input buffer's `cmd` is zero and a whole packet has arrived,
//!   write it in. The ROM reads it on its next frame and sets `cmd` back to zero.
//! - It connects once the ROM has something to say (its handshake), tries again every few
//!   seconds while nobody is listening, and lets go when the buffers disappear.
//!
//! Each pass is one cart read, of both pointers and both buffers, and at most one write,
//! clearing the packet just sent and putting in the next one received. The agent answers one
//! request per game frame, and the ROM drops a handshake that takes more than a second, so
//! the number of requests per pass is what decides whether a connection holds. The game's
//! frame only sees the agent's reads and writes at its own hook, so a packet written in one
//! request is whole when the ROM reads it.

use std::io::{self, Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ap64_cart::backend::Backend;

use crate::server::Event;

/// Where the relay finds the ROM's buffers and the client.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The client's TCP port, on localhost.
    pub port: u16,
    /// The KSEG0 address of the input buffer's address; the output buffer's follows it.
    pub pointers: u32,
    /// The most a buffer holds: `size` plus 2 never exceeds it.
    pub max_packet: usize,
}

/// How long to leave between tries at a client that is not listening, as the Lua does.
const REDIAL: Duration = Duration::from_secs(3);
const DIAL_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a send may block: the client reads as fast as it can, so this is a dead one.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
/// How often [`Event::Idle`] is raised while nothing moves.
const IDLE_EVENT_EVERY: Duration = Duration::from_secs(2);
/// How long to wait for the client's reply to a packet just sent, so the two can share a cart
/// write. The client is on this machine and answers in a millisecond or two.
const REPLY_WAIT: Duration = Duration::from_millis(20);
/// How long to wait before looking again while the ROM has no buffers yet (booting, or not
/// this game's ROM at all).
const NOT_READY_WAIT: Duration = Duration::from_millis(250);

/// A KSEG0 or KSEG1 address as the cart's physical one.
fn physical(addr: u32) -> u32 {
    addr & 0x1FFF_FFFF
}

fn be16(b: &[u8]) -> usize {
    u16::from_be_bytes([b[0], b[1]]) as usize
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Relay between the client and the ROM's buffers until `stop` is set or the cart fails.
/// `cart` stays on the calling thread.
///
/// Raises the same [`Event`]s as [`crate::server::serve`], except that it dials rather than
/// listens: [`Event::Dialing`] says where it looks for the client.
pub fn serve(
    cart: &mut dyn Backend,
    opts: &Options,
    stop: &AtomicBool,
    on_event: &mut dyn FnMut(Event),
) -> Result<(), String> {
    on_event(Event::Dialing(opts.port));
    let mut link = Link::default();
    let mut last_idle = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let moved = pass(cart, opts, &mut link, on_event).map_err(|e| e.to_string())?;
        if !moved {
            if last_idle.elapsed() >= IDLE_EVENT_EVERY {
                last_idle = Instant::now();
                on_event(Event::Idle);
            }
            // A cart's round trip paces this on its own; a fake one does not.
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Link {
    /// The input and output buffers' addresses, once read: they only change with a reset.
    buffers: Option<(u32, u32)>,
    conn: Option<TcpStream>,
    /// Bytes from the client not yet written to the ROM.
    inbox: Vec<u8>,
    last_dial: Option<Instant>,
    /// Said once, not every three seconds.
    told_not_listening: bool,
}

impl Link {
    fn drop_client(&mut self, why: String, on_event: &mut dyn FnMut(Event)) {
        if self.conn.take().is_some() {
            on_event(Event::ClientDisconnected(why));
        }
        self.inbox.clear();
    }
}

/// Find the buffers: the pointers alone, once, then every pass reads them with the buffers.
fn find_buffers(
    cart: &mut dyn Backend,
    opts: &Options,
    link: &mut Link,
    on_event: &mut dyn FnMut(Event),
) -> io::Result<bool> {
    let ptrs = cart.read_many(&[(physical(opts.pointers), 8)])?.remove(0);
    let (input, output) = (be32(&ptrs[0..4]), be32(&ptrs[4..8]));
    let ram = 0x8000_0000..0x8000_0000 + cart.rdram_size();
    let fits = |p: u32| p > ram.start && (p as u64 + opts.max_packet as u64) <= ram.end as u64;
    if fits(input) && fits(output) {
        link.buffers = Some((input, output));
    } else {
        link.drop_client(
            "the game is not running its Archipelago code (reset, or still booting)".into(),
            on_event,
        );
        std::thread::sleep(NOT_READY_WAIT);
    }
    Ok(false)
}

/// One look at the ROM, and whatever that lets through. Whether anything moved.
///
/// At most one cart read and one cart write: the agent answers one request per game frame,
/// and the ROM gives up on a handshake that takes more than a second, so a pass that asked
/// four times, as this once did, lost every handshake it relayed.
fn pass(
    cart: &mut dyn Backend,
    opts: &Options,
    link: &mut Link,
    on_event: &mut dyn FnMut(Event),
) -> io::Result<bool> {
    let Some((input, output)) = link.buffers else {
        return find_buffers(cart, opts, link, on_event);
    };
    let r = cart.read_many(&[
        (physical(opts.pointers), 8),
        (physical(output), opts.max_packet),
        (physical(input), 4),
    ])?;
    if (be32(&r[0][0..4]), be32(&r[0][4..8])) != (input, output) {
        // Reset, or another ROM: look for the buffers again.
        link.buffers = None;
        link.drop_client("the game was reset".into(), on_event);
        return Ok(false);
    }
    let out = &r[1];
    let (out_len, out_cmd) = (be16(&out[0..2]) + 2, be16(&out[2..4]));
    let inbound_free = be16(&r[2][2..4]) == 0;

    // Nothing to do until the ROM speaks: its first packet is the handshake.
    if link.conn.is_none() && out_cmd != 0 {
        dial(opts, link, on_event);
    }
    let Some(conn) = link.conn.as_mut() else {
        return Ok(false);
    };

    let mut writes: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut sent = false;
    if out_cmd != 0 {
        if !(4..=opts.max_packet).contains(&out_len) {
            on_event(Event::Note(format!(
                "the game's output buffer holds a packet of {out_len} bytes, more than {}",
                opts.max_packet
            )));
            link.drop_client("the game sent a packet too large to relay".into(), on_event);
            return Ok(false);
        }
        if let Err(e) = send(conn, &out[..out_len]) {
            link.drop_client(format!("sending to it failed: {e}"), on_event);
            return Ok(false);
        }
        writes.push((physical(output) + 2, vec![0, 0]));
        on_event(Event::Handled);
        sent = true;
    }

    // Having just sent, give the client a moment to answer, so its reply goes in the same
    // write as the clear rather than a pass later.
    let wait = (sent && inbound_free).then_some(REPLY_WAIT);
    if let Err(why) = receive(conn, &mut link.inbox, wait) {
        link.drop_client(why, on_event);
        // The packet it was sent is gone either way; the clear still belongs to the ROM.
        write_all(cart, &writes)?;
        return Ok(sent);
    }

    if inbound_free && link.inbox.len() >= 4 {
        let len = be16(&link.inbox[0..2]) + 2;
        if !(4..=opts.max_packet).contains(&len) {
            link.drop_client(
                format!("it sent a packet of {len} bytes, more than the game holds"),
                on_event,
            );
            write_all(cart, &writes)?;
            return Ok(sent);
        }
        if link.inbox.len() >= len {
            writes.push((physical(input), link.inbox.drain(..len).collect()));
            on_event(Event::Handled);
        }
    }
    let moved = !writes.is_empty();
    write_all(cart, &writes)?;
    Ok(moved)
}

fn write_all(cart: &mut dyn Backend, writes: &[(u32, Vec<u8>)]) -> io::Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    let refs: Vec<(u32, &[u8])> = writes.iter().map(|(a, b)| (*a, b.as_slice())).collect();
    cart.write_many(&refs)
}

/// Send a whole packet. Blocking, with a timeout: a non-blocking socket ignores the timeout
/// and could leave half a packet behind.
fn send(conn: &mut TcpStream, packet: &[u8]) -> io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.write_all(packet)
}

/// Take whatever the client has sent, first waiting up to `wait` for something if given.
/// An error is why the client is gone.
fn receive(
    conn: &mut TcpStream,
    inbox: &mut Vec<u8>,
    wait: Option<Duration>,
) -> Result<(), String> {
    let mut buf = [0u8; 1024];
    let mut take = |conn: &mut TcpStream| -> Result<bool, String> {
        match conn.read(&mut buf) {
            Ok(0) => Err("it closed the connection".into()),
            Ok(n) => {
                inbox.extend_from_slice(&buf[..n]);
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(format!("reading from it failed: {e}")),
        }
    };
    if let Some(wait) = wait {
        conn.set_nonblocking(false)
            .and_then(|()| conn.set_read_timeout(Some(wait)))
            .map_err(|e| format!("reading from it failed: {e}"))?;
        take(conn)?;
    }
    conn.set_nonblocking(true)
        .map_err(|e| format!("reading from it failed: {e}"))?;
    while take(conn)? {}
    Ok(())
}

fn dial(opts: &Options, link: &mut Link, on_event: &mut dyn FnMut(Event)) {
    if link.last_dial.is_some_and(|t| t.elapsed() < REDIAL) {
        return;
    }
    link.last_dial = Some(Instant::now());
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, opts.port));
    let conn = TcpStream::connect_timeout(&at, DIAL_TIMEOUT).and_then(|c| {
        c.set_nonblocking(true)?;
        c.set_nodelay(true)?;
        c.set_write_timeout(Some(SEND_TIMEOUT))?;
        Ok(c)
    });
    match conn {
        Ok(c) => {
            link.told_not_listening = false;
            link.inbox.clear();
            link.conn = Some(c);
            on_event(Event::ClientConnected(at));
        }
        Err(e) if !link.told_not_listening => {
            link.told_not_listening = true;
            on_event(Event::Note(format!(
                "nothing is listening on localhost:{} yet ({e}); trying every {} s",
                opts.port,
                REDIAL.as_secs()
            )));
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap64_cart::backend::RamImage;
    use std::net::TcpListener;

    const PTRS: u32 = 0x8040_0000;
    const INPUT: u32 = 0x8040_1000;
    const OUTPUT: u32 = 0x8040_2000;
    const MAX: usize = 512;

    /// The ROM's side, run between the relay's cart calls: it takes a packet from the input
    /// buffer as the game does each frame, and puts its next packet out once the last one is
    /// gone.
    struct Rom {
        ram: RamImage,
        to_send: Vec<Vec<u8>>,
        received: Vec<Vec<u8>>,
    }

    impl Rom {
        fn new(to_send: Vec<Vec<u8>>) -> Self {
            let mut ram = vec![0u8; 0x80_0000];
            let p = (PTRS & 0x1FFF_FFFF) as usize;
            ram[p..p + 4].copy_from_slice(&INPUT.to_be_bytes());
            ram[p + 4..p + 8].copy_from_slice(&OUTPUT.to_be_bytes());
            Rom {
                ram: RamImage::new(ram),
                to_send,
                received: Vec::new(),
            }
        }

        fn frame(&mut self) {
            let (i, o) = (
                (INPUT & 0x1FFF_FFFF) as usize,
                (OUTPUT & 0x1FFF_FFFF) as usize,
            );
            let ram = &mut self.ram.ram;
            if be16(&ram[i + 2..i + 4]) != 0 {
                let len = be16(&ram[i..i + 2]) + 2;
                self.received.push(ram[i..i + len].to_vec());
                ram[i + 2..i + 4].copy_from_slice(&[0, 0]);
            }
            if be16(&ram[o + 2..o + 4]) == 0 && !self.to_send.is_empty() {
                let p = self.to_send.remove(0);
                ram[o..o + p.len()].copy_from_slice(&p);
            }
        }
    }

    impl Backend for Rom {
        fn rdram_size(&self) -> u32 {
            self.ram.rdram_size()
        }
        fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            self.frame();
            self.ram.read_many(regions)
        }
        fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
            self.ram.write_many(writes)
        }
        fn rom_window(&self) -> Option<u32> {
            None
        }
        fn read_rom_many(&mut self, _: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// Pass until the relay has dialed: the first pass only finds the buffers.
    fn connect(rom: &mut Rom, opts: &Options, link: &mut Link, events: &mut Vec<Event>) {
        let t = Instant::now();
        while link.conn.is_none() && t.elapsed() < Duration::from_secs(5) {
            pass(rom, opts, link, &mut |e| events.push(e)).unwrap();
        }
        assert!(link.conn.is_some(), "it dials once the ROM speaks");
    }

    fn packet(cmd: u16, data: &[u8]) -> Vec<u8> {
        let mut p = ((data.len() + 2) as u16).to_be_bytes().to_vec();
        p.extend_from_slice(&cmd.to_be_bytes());
        p.extend_from_slice(data);
        p
    }

    /// Read one framed packet from the relay, as the client does.
    fn read_packet(s: &mut TcpStream) -> Vec<u8> {
        let mut head = [0u8; 2];
        s.read_exact(&mut head).unwrap();
        let mut rest = vec![0u8; u16::from_be_bytes(head) as usize];
        s.read_exact(&mut rest).unwrap();
        [head.to_vec(), rest].concat()
    }

    #[test]
    fn packets_cross_both_ways_and_it_stops_promptly() {
        let client = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let opts = Options {
            port: client.local_addr().unwrap().port(),
            pointers: PTRS,
            max_packet: MAX,
        };
        let hello = packet(1, b"\x00\x00\x04\x01HELO");
        let ping = packet(2, &[]);
        let big = packet(8, &[0x5A; 508]);
        let stop = AtomicBool::new(false);
        let (got, received, events, stopped_in) = std::thread::scope(|scope| {
            let serving = scope.spawn(|| {
                let mut rom = Rom::new(vec![hello.clone(), ping.clone()]);
                let mut events = Vec::new();
                serve(&mut rom, &opts, &stop, &mut |e| events.push(e)).unwrap();
                (rom.received, events)
            });
            let (mut s, _) = client.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let got = vec![read_packet(&mut s), read_packet(&mut s)];
            s.write_all(&packet(1, b"\x00\x00\x04\x01'LO!")).unwrap();
            s.write_all(&big).unwrap();
            // Both queued at once: the second waits for the ROM to take the first.
            std::thread::sleep(Duration::from_millis(300));
            let t = Instant::now();
            stop.store(true, Ordering::Relaxed);
            let (received, events) = serving.join().unwrap();
            (got, received, events, t.elapsed())
        });
        assert_eq!(
            got,
            vec![hello, ping],
            "the ROM's packets, in order and whole"
        );
        assert_eq!(
            received,
            vec![packet(1, b"\x00\x00\x04\x01'LO!"), big],
            "the client's packets, one at a time"
        );
        assert!(matches!(events[0], Event::Dialing(_)));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ClientConnected(_))));
        assert!(stopped_in < Duration::from_secs(1), "{stopped_in:?}");
    }

    #[test]
    fn nothing_is_dialed_until_the_rom_speaks() {
        let client = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        client.set_nonblocking(true).unwrap();
        let opts = Options {
            port: client.local_addr().unwrap().port(),
            pointers: PTRS,
            max_packet: MAX,
        };
        let mut rom = Rom::new(vec![]);
        let mut link = Link::default();
        for _ in 0..5 {
            pass(&mut rom, &opts, &mut link, &mut |_| {}).unwrap();
        }
        assert!(link.conn.is_none());
        assert!(
            client.accept().is_err(),
            "no connection while the ROM is silent"
        );
    }

    #[test]
    fn a_game_without_its_buffers_is_waited_for() {
        let opts = Options {
            port: 1,
            pointers: PTRS,
            max_packet: MAX,
        };
        let mut rom = Rom::new(vec![packet(1, b"HELO")]);
        let p = (PTRS & 0x1FFF_FFFF) as usize;
        rom.ram.ram[p..p + 8].fill(0);
        let mut link = Link::default();
        assert!(!pass(&mut rom, &opts, &mut link, &mut |_| {}).unwrap());
        assert!(
            link.last_dial.is_none(),
            "not dialed before the game is ready"
        );
    }

    #[test]
    fn a_client_that_leaves_is_let_go_and_its_bytes_dropped() {
        let client = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let opts = Options {
            port: client.local_addr().unwrap().port(),
            pointers: PTRS,
            max_packet: MAX,
        };
        let mut rom = Rom::new(vec![packet(1, b"HELO")]);
        let mut link = Link::default();
        let mut events = Vec::new();
        connect(&mut rom, &opts, &mut link, &mut events);
        let (mut s, _) = client.accept().unwrap();
        s.write_all(&[0, 10, 0]).unwrap(); // part of a packet, then gone
        drop(s);
        let t = Instant::now();
        while link.conn.is_some() && t.elapsed() < Duration::from_secs(5) {
            pass(&mut rom, &opts, &mut link, &mut |e| events.push(e)).unwrap();
        }
        assert!(link.conn.is_none());
        assert!(link.inbox.is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ClientDisconnected(_))));
    }

    #[test]
    fn a_packet_larger_than_the_buffer_is_refused_not_written() {
        let client = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let opts = Options {
            port: client.local_addr().unwrap().port(),
            pointers: PTRS,
            max_packet: MAX,
        };
        let mut rom = Rom::new(vec![packet(1, b"HELO")]);
        let before = rom.ram.ram.clone();
        let mut link = Link::default();
        let mut events = Vec::new();
        connect(&mut rom, &opts, &mut link, &mut events);
        let (mut s, _) = client.accept().unwrap();
        // One byte past what the ROM's input buffer holds.
        s.write_all(&packet(3, &[0xEE; MAX - 3])).unwrap();
        let t = Instant::now();
        while link.conn.is_some() && t.elapsed() < Duration::from_secs(5) {
            pass(&mut rom, &opts, &mut link, &mut |e| events.push(e)).unwrap();
        }
        assert!(link.conn.is_none(), "the client is let go");
        let i = (INPUT & 0x1FFF_FFFF) as usize;
        assert_eq!(
            rom.ram.ram[i..i + MAX + 16],
            before[i..i + MAX + 16],
            "nothing written"
        );
        assert!(rom.received.is_empty());
    }

    /// Counts the cart requests a pass makes: the agent answers one per game frame, and the
    /// ROM drops a handshake that takes over a second, so this is the budget that matters.
    struct Counting {
        rom: Rom,
        reads: usize,
        writes: usize,
    }

    impl Backend for Counting {
        fn rdram_size(&self) -> u32 {
            self.rom.rdram_size()
        }
        fn read_many(&mut self, regions: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            self.reads += 1;
            self.rom.read_many(regions)
        }
        fn write_many(&mut self, writes: &[(u32, &[u8])]) -> io::Result<()> {
            self.writes += 1;
            self.rom.write_many(writes)
        }
        fn rom_window(&self) -> Option<u32> {
            None
        }
        fn read_rom_many(&mut self, _: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    #[test]
    fn a_pass_is_one_read_and_at_most_one_write_and_a_reply_rides_along() {
        let client = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let opts = Options {
            port: client.local_addr().unwrap().port(),
            pointers: PTRS,
            max_packet: MAX,
        };
        let mut cart = Counting {
            rom: Rom::new(vec![packet(1, b"HELO"), packet(2, &[])]),
            reads: 0,
            writes: 0,
        };
        let mut link = Link::default();
        let t = Instant::now();
        while link.conn.is_none() && t.elapsed() < Duration::from_secs(5) {
            pass(&mut cart, &opts, &mut link, &mut |_| {}).unwrap();
        }
        let (mut s, _) = client.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // The client answers each packet at once, as a local client does.
        let answering = std::thread::spawn(move || {
            let hello = read_packet(&mut s);
            s.write_all(&packet(1, b"'LO!")).unwrap();
            let ping = read_packet(&mut s);
            s.write_all(&packet(3, &[])).unwrap();
            (hello, ping, s)
        });
        // The pass that dialed already sent the handshake. Each pass from here is one read and
        // one write, and the client's answer to a packet goes in with the clear of it.
        let (reads, writes) = (cart.reads, cart.writes);
        for _ in 0..3 {
            pass(&mut cart, &opts, &mut link, &mut |_| {}).unwrap();
        }
        let (hello, ping, _s) = answering.join().unwrap();
        assert_eq!((hello, ping), (packet(1, b"HELO"), packet(2, &[])));
        assert_eq!(cart.reads - reads, 3, "one read per pass");
        assert!(cart.writes - writes <= 3, "at most one write per pass");
        cart.rom.frame();
        assert_eq!(
            cart.rom.received,
            vec![packet(1, b"'LO!"), packet(3, &[])],
            "both answers delivered within those passes"
        );
    }
}
