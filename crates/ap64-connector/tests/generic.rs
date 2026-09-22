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

/// A RAM image the test can still look at after handing it to the connector.
struct Shared(Rc<RefCell<RamImage>>);

impl Backend for Shared {
    fn rdram_size(&self) -> u32 {
        self.0.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.0.borrow_mut().read_many(r)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.0.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        self.0.borrow().rom_window()
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.0.borrow_mut().read_rom_many(r)
    }
}

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
}

/// The cart's ROM, as PEEKROM serves it: an 8 KiB window here.
fn rom() -> Vec<u8> {
    let mut image = vec![0u8; 0x2000];
    image[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
    image[0x20..0x25].copy_from_slice(b"PAPER");
    image[0x1800] = 0x42; // past what the hash covers
    image
}

/// Connect, send each line, collect each reply, then set `done`.
///
/// Gives up quietly if nothing ever answers on `port`, so that an attempt whose port
/// was taken before `serve` could bind it can be joined and run again.
fn client(
    port: u16,
    lines: Vec<String>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<String>> {
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
            return Vec::new();
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
        replies
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
        let connector = Connector::new(&GENERIC, Box::new(Shared(ram.clone())), log).unwrap();

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
            r#"[{"type":"DISPLAY_MESSAGE","message":"Got Roast Chicken"},{"type":"LOCK"},{"type":"UNLOCK"}]"#,
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
        let replies = c.join().unwrap();
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
        for want in ["DISPLAY_MESSAGE_RESPONSE", "LOCKED", "UNLOCKED"] {
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
            c.join().unwrap().is_empty(),
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
