//! The forked generic connector over real TCP, spoken to exactly as Archipelago's
//! BizHawk Client speaks to it, against a RAM image instead of a console.

mod common;

use std::cell::RefCell;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ap64_cart::backend::{Backend, RamImage};
use ap64_connector::server::{serve, Event};
use ap64_connector::{Connector, GENERIC};
use common::{on_a_free_port, port_was_taken};
use sha1::{Digest, Sha1};

/// A cart that has gone for good: every read fails, as after the reconnect deadline.
struct Dead;

impl Backend for Dead {
    fn rdram_size(&self) -> u32 {
        0x80_0000
    }
    fn read_many(&mut self, _: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        Err(io::Error::other("cart read: gave up after 300s"))
    }
    fn write_many(&mut self, _: &[(u32, &[u8])]) -> io::Result<()> {
        Err(io::Error::other("cart write: gave up after 300s"))
    }
    fn rom_window(&self) -> Option<u32> {
        Some(0x0400_0000)
    }
    fn read_rom_many(&mut self, _: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        Err(io::Error::other("cart ROM read: gave up after 300s"))
    }
    fn generation(&self) -> u32 {
        0
    }
    // No watch: live memory, which is what a connector falls back to.
    fn set_watch(&mut self, _: ap64_cart::watch::Watch) {}
    fn take_watched(&mut self) -> Option<Vec<u8>> {
        None
    }
    fn sample_watch(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
        None
    }
}

/// The cart's ROM, as PEEKROM serves it: an 8 KiB window here.
fn rom() -> Vec<u8> {
    let mut image = vec![0u8; 0x2000];
    image[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
    image[0x20..0x25].copy_from_slice(b"PAPER");
    image[0x1800] = 0x42; // past what the hash covers
    image
}

/// Connect, send each line, collect each reply, then set `done`. The replies come back
/// with the socket, still open: `serve` may be between looking at `done` and reading from
/// its client, and a socket closed then is a `ClientDisconnected` the test itself made.
/// Hold it until `serve` has returned.
///
/// Gives up quietly if nothing ever answers on `port`, so that an attempt whose port
/// was taken before `serve` could bind it can be joined and run again.
fn client(
    port: u16,
    lines: Vec<String>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<(Vec<String>, Option<TcpStream>)> {
    std::thread::spawn(move || {
        let mut connected = None;
        for _ in 0..500 {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                // The test sets `done` once it knows there is nothing to connect to.
                Err(_) if done.load(Ordering::SeqCst) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let Some(stream) = connected else {
            done.store(true, Ordering::SeqCst);
            return (Vec::new(), None);
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut replies = Vec::new();
        for line in lines {
            writer.write_all(format!("{line}\n").as_bytes()).unwrap();
            let mut reply = String::new();
            if reader.read_line(&mut reply).unwrap_or(0) == 0 {
                break;
            }
            replies.push(reply.trim_end().to_string());
        }
        done.store(true, Ordering::SeqCst);
        (replies, Some(writer))
    })
}

#[test]
fn serves_domains_guards_and_writes() {
    on_a_free_port(|port| {
        let mut ram = vec![0u8; 0x80_0000];
        ram[0x1000..0x1004].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let rom = rom();
        let hash: String = Sha1::digest(&rom[..0x1000])
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        let ram = Rc::new(RefCell::new(RamImage::with_rom(ram, rom)));
        let messages = Arc::new(Mutex::new(Vec::new()));
        let log = {
            let m = messages.clone();
            Arc::new(move |s: String| m.lock().unwrap().push(s))
        };
        let connector = Connector::new(&GENERIC, Box::new(ram.clone()), log).unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let lines = [
            "VERSION",
            // Identity, sizes, and one read from each domain. 0x80001000 on the System
            // Bus is KSEG0 for RDRAM 0x1000, so it must match that read.
            r#"[{"type":"PING"},{"type":"SYSTEM"},{"type":"HASH"},{"type":"MEMORY_SIZE","domain":"RDRAM"},{"type":"MEMORY_SIZE","domain":"ROM"},{"type":"READ","address":32,"size":5,"domain":"ROM"},{"type":"READ","address":4096,"size":4,"domain":"RDRAM"},{"type":"READ","address":2147487744,"size":4,"domain":"System Bus"}]"#,
            // A guard that holds, its write, and a read in the same batch that must see
            // the write, as it would under BizHawk.
            r#"[{"type":"GUARD","address":8192,"expected_data":"AAAAAA==","domain":"RDRAM"},{"type":"WRITE","address":8192,"value":"AAUAAA==","domain":"RDRAM"},{"type":"READ","address":8192,"size":4,"domain":"RDRAM"}]"#,
            // The same guard now fails, so the write after it must not happen.
            r#"[{"type":"GUARD","address":8192,"expected_data":"AAAAAA==","domain":"RDRAM"},{"type":"WRITE","address":12288,"value":"/w==","domain":"RDRAM"}]"#,
            // ROM writes and unknown domains are refused, not invented.
            r#"[{"type":"WRITE","address":0,"value":"AQ==","domain":"ROM"},{"type":"READ","address":0,"size":1,"domain":"EEPROM"}]"#,
            // GoldenEye 007's world also sends LOADOUT_TRACKER, for an overlay its own Lua draws.
            r#"[{"type":"DISPLAY_MESSAGE","message":"Got Roast Chicken"},{"type":"LOCK"},{"type":"UNLOCK"},{"type":"LOADOUT_TRACKER","enabled":true,"missions":[]}]"#,
            // ROM from the cart: the System Bus mirror of 0x20, a read in the same batch as an
            // RDRAM guard, and a read past the window, which fails only that request.
            r#"[{"type":"READ","address":268435488,"size":5,"domain":"System Bus"},{"type":"GUARD","address":4096,"expected_data":"3q2+7w==","domain":"RDRAM"},{"type":"READ","address":6144,"size":1,"domain":"ROM"},{"type":"READ","address":8190,"size":4,"domain":"ROM"},{"type":"HASH"}]"#,
        ];
        let c = client(
            port,
            lines.iter().map(|s| s.to_string()).collect(),
            done.clone(),
        );
        let mut events = Vec::new();
        // A bind failure here is the port having been taken between the pick and this
        // call; stop the client so the attempt can be run again on another port.
        if let Err(e) = serve(&connector, &[port], &done, &mut |e| events.push(e)) {
            done.store(true, Ordering::SeqCst);
            let _ = c.join();
            return Err(e);
        }
        let (replies, _socket) = c.join().unwrap();
        assert_eq!(replies.len(), lines.len(), "{replies:#?}");

        assert_eq!(replies[0], "1", "VERSION");

        let r = &replies[1];
        let hash_value = format!("\"value\":\"{hash}\"");
        for want in [
            "\"type\":\"PONG\"",
            "\"value\":\"N64\"",
            hash_value.as_str(),
            "\"value\":8388608",
            "\"value\":8192",
            "\"value\":\"UEFQRVI=\"",
        ] {
            assert!(r.contains(want), "batch 1 lacks {want}: {r}");
        }
        assert_eq!(
            r.matches("\"value\":\"3q2+7w==\"").count(),
            2,
            "RDRAM and its System Bus mirror must read the same bytes: {r}"
        );

        let r = &replies[2];
        for want in [
            "\"type\":\"GUARD_RESPONSE\"",
            "\"value\":true",
            "\"type\":\"WRITE_RESPONSE\"",
            "\"value\":\"AAUAAA==\"",
        ] {
            assert!(r.contains(want), "batch 2 lacks {want}: {r}");
        }

        let r = &replies[3];
        assert!(
            r.contains("\"value\":false"),
            "batch 3 guard should fail: {r}"
        );
        assert!(
            !r.contains("WRITE_RESPONSE"),
            "a failed guard must skip the write: {r}"
        );

        let r = &replies[4];
        assert_eq!(r.matches("\"type\":\"ERROR\"").count(), 2, "batch 4: {r}");

        let r = &replies[5];
        for want in [
            "DISPLAY_MESSAGE_RESPONSE",
            "LOCKED",
            "UNLOCKED",
            "LOADOUT_TRACKER_RESPONSE",
        ] {
            assert!(r.contains(want), "batch 5 lacks {want}: {r}");
        }
        assert!(messages
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("Got Roast Chicken")));

        let r = &replies[6];
        assert_eq!(
            r.matches("\"value\":\"UEFQRVI=\"").count(),
            1,
            "System Bus 0x10000020 is ROM 0x20: {r}"
        );
        assert!(
            r.contains("\"value\":true"),
            "the RDRAM guard in the same batch: {r}"
        );
        assert!(r.contains("\"value\":\"Qg==\""), "ROM 0x1800: {r}");
        assert_eq!(
            r.matches("\"type\":\"ERROR\"").count(),
            1,
            "past the window: {r}"
        );
        assert!(r.contains(&hash_value), "the hash is stable: {r}");

        let mut ram = ram.borrow_mut();
        assert_eq!(
            ram.read_many(&[(0x2000, 4)]).unwrap()[0],
            [0, 5, 0, 0],
            "guarded write landed"
        );
        assert_eq!(
            ram.read_many(&[(0x3000, 1)]).unwrap()[0],
            [0],
            "skipped write did not"
        );

        assert!(
            matches!(events[0], Event::Listening(p) if p == port),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ClientConnected(_))),
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|e| **e == Event::Handled).count(),
            lines.len()
        );
        Ok(())
    });
}

#[test]
fn a_dead_cart_ends_the_session_even_though_the_script_catches_it() {
    on_a_free_port(|port| {
        let connector = Connector::new(&GENERIC, Box::new(Dead), ap64_cart::quiet()).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let c = client(
            port,
            vec![r#"[{"type":"READ","address":0,"size":4,"domain":"RDRAM"}]"#.to_string()],
            done.clone(),
        );
        let err = serve(&connector, &[port], &done, &mut |_| {}).unwrap_err();
        if port_was_taken(&err) {
            done.store(true, Ordering::SeqCst);
            let _ = c.join();
            return Err(err);
        }
        assert!(err.contains("gave up"), "{err}");
        assert!(
            c.join().unwrap().0.is_empty(),
            "no reply is sent for a dead cart"
        );
        Ok(())
    });
}

#[test]
fn a_taken_port_moves_to_the_next_one() {
    // Bound for as long as the test runs, so the port that must be skipped is in use
    // however many ports the free one takes to land.
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    on_a_free_port(|free| {
        assert_ne!(free, busy);
        let connector = Connector::new(
            &GENERIC,
            Box::new(RamImage::with_rom(vec![0; 16], rom())),
            ap64_cart::quiet(),
        )
        .unwrap();
        let stop = AtomicBool::new(true);
        let mut events = Vec::new();
        // An error can only mean `free` was taken between the pick and this bind:
        // another port is picked and this runs again. Binding `busy`, or reporting
        // any other port, fails on the spot.
        serve(&connector, &[busy, free], &stop, &mut |e| events.push(e))?;
        assert_eq!(events, vec![Event::Listening(free)]);
        Ok(())
    });
}

#[test]
fn an_agent_without_peekrom_is_refused_up_front() {
    let err = Connector::new(
        &GENERIC,
        Box::new(RamImage::new(vec![0; 16])),
        ap64_cart::quiet(),
    )
    .err()
    .unwrap();
    assert!(err.contains("PEEKROM"), "{err}");
}

/// No script may evict a client for being slow to speak.
///
/// Every Archipelago client AP64 serves is request/response: it writes a line, then blocks
/// reading the reply. While it blocks it sends nothing, so `Client::last_heard` ages by
/// exactly however long the cart took to answer -- and server::serve_until checks the
/// timeout AFTER handle() returns. A reply slower than the timeout is therefore delivered
/// and the client dropped for silence the server itself caused.
///
/// `generic` carried Some(5s) for exactly this reason and it showed on a console:
/// Castlevania 64 lost its client every few minutes, each time reconnecting about a second
/// later with no check lost, which is eviction rather than a fault. A cart round trip has
/// no upper bound worth betting a disconnect on -- the game owns the bus and the agent
/// waits its turn.
///
/// Nothing is given up by dropping it: a timeout exists to free the slot for a new client,
/// and accept_newest() already replaces the old one when a new connection arrives.
#[test]
fn no_script_drops_a_client_for_being_slow() {
    for script in ap64_connector::SCRIPTS {
        assert!(
            script.client_timeout.is_none(),
            "script {:?} sets client_timeout = {:?}; a request/response client cannot ping \
             while it waits for the cart, so this evicts healthy clients on a slow round trip",
            script.id,
            script.client_timeout,
        );
    }
}

/// A cart whose link restarts under the next read, as `Multi64`'s does when a console is reset
/// mid-request: the read is answered from the new boot, on a new generation.
struct RestartsUnderRead {
    ram: Rc<RefCell<RamImage>>,
    generation: Rc<std::cell::Cell<u32>>,
    armed: Rc<std::cell::Cell<bool>>,
}

impl Backend for RestartsUnderRead {
    fn rdram_size(&self) -> u32 {
        self.ram.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        if self.armed.replace(false) {
            self.generation.set(self.generation.get() + 1);
        }
        self.ram.borrow_mut().read_many(r)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.ram.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        self.ram.borrow().rom_window()
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.ram.borrow_mut().read_rom_many(r)
    }
    fn generation(&self) -> u32 {
        self.generation.get()
    }
    // No watch: live memory, which is what a connector falls back to.
    fn set_watch(&mut self, _: ap64_cart::watch::Watch) {}
    fn take_watched(&mut self) -> Option<Vec<u8>> {
        None
    }
    fn sample_watch(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
        None
    }
}

/// A GUARD that passed against memory read across a restarted link must not let its WRITE
/// through: the console may have been reset in between, and the write was decided from the
/// boot before. The line fails rather than writing, and the next is answered afresh.
#[test]
fn a_batch_whose_link_restarted_does_not_write() {
    let ram = Rc::new(RefCell::new(RamImage::with_rom(
        vec![0u8; 0x80_0000],
        rom(),
    )));
    let generation = Rc::new(std::cell::Cell::new(0));
    let armed = Rc::new(std::cell::Cell::new(false));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let log = {
        let lines = lines.clone();
        Arc::new(move |s: String| lines.lock().unwrap().push(s))
    };
    let backend = RestartsUnderRead {
        ram: ram.clone(),
        generation: generation.clone(),
        armed: armed.clone(),
    };
    let connector = Connector::new(&GENERIC, Box::new(backend), log).unwrap();
    let batch = r#"[{"type":"GUARD","address":8192,"expected_data":"AAAAAA==","domain":"RDRAM"},{"type":"WRITE","address":8192,"value":"AAUAAA==","domain":"RDRAM"}]"#;

    armed.set(true);
    assert!(
        connector.handle(batch).is_err(),
        "a line whose writes were dropped is not answered as done"
    );
    assert_eq!(
        ram.borrow_mut().read_many(&[(0x2000, 4)]).unwrap()[0],
        vec![0; 4],
        "the write reached the console after the link restarted"
    );

    // The next line is loaded against the link as it is, and writes.
    let reply = connector.handle(batch).unwrap();
    assert!(reply.contains("WRITE_RESPONSE"), "{reply}");
    assert_eq!(
        ram.borrow_mut().read_many(&[(0x2000, 4)]).unwrap()[0],
        vec![0, 5, 0, 0]
    );
    assert!(lines
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("loading the generic connector again")));
}

/// Send `line` on `s` and read one line back, or say why there was none.
fn ask(s: &TcpStream, line: &str) -> Result<String, String> {
    let mut w = s;
    w.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut reply = String::new();
    match BufReader::new(s).read_line(&mut reply) {
        Ok(0) => Err("closed".into()),
        Ok(_) => Ok(reply.trim_end().to_string()),
        Err(e) => Err(e.kind().to_string()),
    }
}

/// Connect to `port`, giving up once `done` is set or after 5 s. By time, not by tries:
/// Windows takes about 2 s to refuse a connection to a localhost port nobody listens on.
fn dial(port: u16, done: &AtomicBool) -> Option<TcpStream> {
    let until = std::time::Instant::now() + Duration::from_secs(5);
    let at = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    while std::time::Instant::now() < until {
        if let Ok(s) = TcpStream::connect_timeout(&at, Duration::from_millis(500)) {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            return Some(s);
        }
        if done.load(Ordering::SeqCst) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A line the script raises on is the client's problem, not the cart's: that client is let
/// go, with a reset it recovers from, and the session goes on serving the one that comes
/// back. It used to end the session as a cart failure, which in AP64 means reconnecting to
/// the cart and losing the client anyway.
#[test]
fn a_line_the_script_cannot_answer_lets_the_client_go_and_the_session_goes_on() {
    on_a_free_port(|port| {
        let ram = Rc::new(RefCell::new(RamImage::with_rom(
            vec![0u8; 0x80_0000],
            rom(),
        )));
        let connector = Connector::new(&GENERIC, Box::new(ram), ap64_cart::quiet()).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let asking = {
            let done = done.clone();
            std::thread::spawn(move || {
                let Some(first) = dial(port, &done) else {
                    done.store(true, Ordering::SeqCst);
                    return None;
                };
                let bad = ask(&first, "this is not json");
                let again = dial(port, &done).map(|s| (ask(&s, "VERSION"), s));
                done.store(true, Ordering::SeqCst);
                Some((bad, again))
            })
        };
        let mut events = Vec::new();
        let served = serve(&connector, &[port], &done, &mut |e| events.push(e));
        if let Err(e) = &served {
            if port_was_taken(e) {
                done.store(true, Ordering::SeqCst);
                let _ = asking.join();
                return Err(e.clone());
            }
        }
        let (bad, again) = asking.join().unwrap().expect("serve listened");
        served.expect("the session goes on");
        assert!(
            bad.is_err(),
            "no reply to a line nobody can answer: {bad:?}"
        );
        let (again, _held) = again.expect("the client could connect again");
        assert_eq!(
            again.as_deref(),
            Ok("1"),
            "the client that came back is served"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ClientDisconnected(why)
                if why.contains("could not be answered"))),
            "{events:?}"
        );
        Ok(())
    });
}
