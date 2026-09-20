//! The forked OoT connector against a synthetic RDRAM image (the layout oot-ap-cart's
//! tools/make-synthetic-dump.py builds): context pointers, normal gameplay, a ROM name.
//! Driven with lines shaped like OoT Client's, through `Connector::handle`.

mod common;

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use ap64_cart::backend::{Backend, RamImage};
use ap64_connector::{Connector, OOT};
use common::on_a_free_port;

const RANDO_CTX: u32 = 0x40_0000;
const COOP_CTX: u32 = 0x40_0100;
const COUNT: u32 = 0x11A5D0 + 0x90;
const MAILBOX_PLAYER: u32 = COOP_CTX + 6;
const MAILBOX_ITEM: u32 = COOP_CTX + 8;
/// The slot OoTR writes the most recent flag-set event to, and the only sign in-scene
/// that a check was collected. connector.lua reads it once per scan.
const TEMP_CONTEXT: u32 = 0x40_002C;

fn ram() -> Vec<u8> {
    let mut ram = vec![0u8; 0x80_0000];
    let be32 = |ram: &mut Vec<u8>, a: u32, v: u32| {
        ram[a as usize..a as usize + 4].copy_from_slice(&v.to_be_bytes())
    };
    be32(&mut ram, 0x1C6E90 + 0x15D4, 0x8000_0000 + RANDO_CTX);
    be32(&mut ram, RANDO_CTX, 0x8000_0000 + COOP_CTX);
    be32(&mut ram, RANDO_CTX + 0x0E9F, 0x0348_0000);
    ram[(RANDO_CTX + 0x0EAD) as usize] = 10;
    ram[(COOP_CTX + 0x0B) as usize] = 1; // death link on
    be32(&mut ram, 0x11F200, 0xDEAD_BEEF); // not the N64 logo
    ram[0x11B92F] = 3; // not title or file select
    ram[0x1D8DD5] = 0; // unpaused
    ram[0x11A600..0x11A602].copy_from_slice(&0x30u16.to_be_bytes()); // alive
    let name = (COOP_CTX + 20 + 0x800 + 5) as usize;
    ram[name..name + 6].copy_from_slice(b"SYNTH\0");
    ram
}

/// A RAM image that counts the calls made on it, and lets the test change memory.
struct Counted {
    ram: Rc<RefCell<RamImage>>,
    reads: Rc<Cell<u32>>,
}

impl Backend for Counted {
    fn rdram_size(&self) -> u32 {
        self.ram.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.reads.set(self.reads.get() + 1);
        self.ram.borrow_mut().read_many(r)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.ram.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        Some(0x0400_0000)
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        Ok(r.iter().map(|&(_, l)| vec![0; l]).collect())
    }
    fn set_watch(&mut self, w: ap64_cart::watch::Watch) {
        self.ram.borrow_mut().set_watch(w);
    }
    fn take_watched(&mut self) -> Option<Vec<u8>> {
        self.ram.borrow_mut().take_watched()
    }
    fn sample_watch(&mut self) -> io::Result<()> {
        self.ram.borrow_mut().sample_watch()
    }
    fn watch_stats(&self) -> Option<ap64_cart::watch::WatchStats> {
        self.ram.borrow().watch_stats()
    }
}

fn block(items: &[u16]) -> String {
    let items: Vec<String> = items.iter().map(u16::to_string).collect();
    format!(
        r#"{{"playerNames":["Alice","Bob"],"triggerDeath":false,"items":[{}],"collectibleOverrides":0,"collectibleOffsets":{{}}}}"#,
        items.join(",")
    )
}

fn u16_at(ram: &Rc<RefCell<RamImage>>, a: u32) -> u16 {
    let b = ram.borrow_mut().read_many(&[(a, 2)]).unwrap();
    u16::from_be_bytes([b[0][0], b[0][1]])
}

fn setup() -> (Connector, Rc<RefCell<RamImage>>, Rc<Cell<u32>>) {
    let ram = Rc::new(RefCell::new(RamImage::new(ram())));
    let reads = Rc::new(Cell::new(0));
    let backend = Counted {
        ram: ram.clone(),
        reads: reads.clone(),
    };
    let c = Connector::new(&OOT, Box::new(backend), ap64_cart::quiet()).unwrap();
    (c, ram, reads)
}

#[test]
fn a_poll_reports_what_oot_client_expects() {
    let (c, _, _) = setup();
    let reply = c.handle(&block(&[])).unwrap();
    for want in [
        "\"playerName\":\"SYNTH\"",
        "\"scriptVersion\":3",
        "\"deathlinkActive\":true",
        "\"locations\":{",
        "\"DMT Chest\":false",
        "\"isDead\":false",
        "\"gameComplete\":false",
    ] {
        assert!(
            reply.contains(want),
            "reply lacks {want}: {}",
            &reply[..300.min(reply.len())]
        );
    }
}

#[test]
fn an_item_is_queued_once_and_names_are_written() {
    let (c, ram, _) = setup();
    c.handle(&block(&[0x42, 0x43])).unwrap();
    assert_eq!(u16_at(&ram, MAILBOX_ITEM), 0x42, "first item queued");
    assert_eq!(u16_at(&ram, MAILBOX_PLAYER), 0);
    let alice = (COOP_CTX + 20 + 8) as usize;
    let written = ram.borrow_mut().read_many(&[(alice as u32, 5)]).unwrap();
    assert_eq!(
        written[0],
        [
            0x6A + b'A',
            0x64 + b'l',
            0x64 + b'i',
            0x64 + b'c',
            0x64 + b'e'
        ]
    );

    // Not consumed yet: nothing more is queued.
    c.handle(&block(&[0x42, 0x43])).unwrap();
    assert_eq!(u16_at(&ram, MAILBOX_ITEM), 0x42);

    // The game takes it: mailbox cleared, count up. The next item follows.
    ram.borrow_mut()
        .write_many(&[(MAILBOX_ITEM, &[0, 0]), (COUNT, &[0, 1])])
        .unwrap();
    c.handle(&block(&[0x42, 0x43])).unwrap();
    assert_eq!(u16_at(&ram, MAILBOX_ITEM), 0x43);
}

/// A batched read split across two requests, with the game taking the mailbox between
/// them: the region holding the count comes back before the item is consumed, and the
/// region holding the mailbox after. That torn pair (count 0, mailbox empty) is how an
/// item used to be delivered twice.
struct TearOnce {
    ram: Rc<RefCell<RamImage>>,
    armed: Rc<Cell<bool>>,
}

impl Backend for TearOnce {
    fn rdram_size(&self) -> u32 {
        self.ram.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for &(addr, len) in r {
            out.push(self.ram.borrow_mut().read_many(&[(addr, len)])?.remove(0));
            if self.armed.get() && addr <= COUNT && COUNT < addr + len as u32 {
                self.armed.set(false);
                self.ram
                    .borrow_mut()
                    .write_many(&[(MAILBOX_ITEM, &[0, 0]), (COUNT, &[0, 1])])?;
            }
        }
        Ok(out)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.ram.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        Some(0x0400_0000)
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        Ok(r.iter().map(|&(_, l)| vec![0; l]).collect())
    }
}

#[test]
fn a_torn_count_and_mailbox_never_deliver_an_item_twice() {
    let ram = Rc::new(RefCell::new(RamImage::new(ram())));
    let armed = Rc::new(Cell::new(false));
    let backend = TearOnce {
        ram: ram.clone(),
        armed: armed.clone(),
    };
    let c = Connector::new(&OOT, Box::new(backend), ap64_cart::quiet()).unwrap();
    c.handle(&block(&[0x42, 0x43])).unwrap(); // learns the pages; queues 0x42
    assert_eq!(u16_at(&ram, MAILBOX_ITEM), 0x42);
    armed.set(true); // the game takes 0x42 in the middle of the next prefetch
    c.handle(&block(&[0x42, 0x43])).unwrap();
    assert!(!armed.get(), "the prefetch read the count's page");
    assert_eq!(
        u16_at(&ram, MAILBOX_ITEM),
        0x43,
        "0x42 must not be queued again"
    );
}

/// A check collected and gone again before the next poll still reaches the client.
///
/// OoT commits scene flags on a scene transition; until then the only sign a check
/// happened is one slot holding the most recent event, which the next one overwrites. The
/// connector reads it once per scan, so at cart poll rates an event between scans used to
/// be lost and the check waited for the scene change -- "I opened the chest, but it only
/// showed up after I left the grotto". The slot is watched and its changes queued, so the
/// scan is shown each one.
#[test]
fn a_check_seen_only_between_polls_still_reaches_the_client() {
    let (c, ram, _) = setup();
    let poll = |c: &Connector| c.handle(&block(&[])).unwrap();
    assert!(
        poll(&c).contains("\"DMT Chest\":false"),
        "the check has not been collected yet"
    );

    // DMT Chest collected: scene 0x60, type 0x01 (chest), id 0x01.
    ram.borrow_mut()
        .write_many(&[(TEMP_CONTEXT, &[0x60, 0x01, 0x00, 0x01])])
        .unwrap();
    c.sample_watch().unwrap();
    // The game clears the slot before the connector's next scan reads it.
    ram.borrow_mut()
        .write_many(&[(TEMP_CONTEXT, &[0, 0, 0, 0])])
        .unwrap();

    let reply = poll(&c);
    assert!(
        reply.contains("\"DMT Chest\":true"),
        "the collected check never reached the client: {}",
        &reply[..300.min(reply.len())]
    );
    let stats = c.watch_stats().expect("the script watches the slot");
    assert_eq!((stats.events, stats.replayed), (1, 1));
}

#[test]
fn a_steady_poll_costs_few_exchanges() {
    let (c, _, reads) = setup();
    c.handle(&block(&[])).unwrap(); // learns which pages a poll touches
    let before = reads.get();
    c.handle(&block(&[])).unwrap();
    let per_poll = reads.get() - before;
    // One prefetch of the learned pages and one refresh of the item pair.
    assert!(per_poll <= 2, "{per_poll} reads for a steady poll");
}

/// OoT Client does not survive being dropped (its N64 task dies on the closed socket and it
/// goes on showing itself connected), and upstream's script never drops it. A client that
/// pauses, as OoT Client does while it logs in to the server, must still be answered.
#[test]
fn a_quiet_client_is_never_dropped() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use ap64_connector::server::{serve, Event};

    on_a_free_port(|port| {
        let (c, _, _) = setup();
        let done = Arc::new(AtomicBool::new(false));
        let client = {
            let done = done.clone();
            std::thread::spawn(move || {
                let mut connected = None;
                for _ in 0..500 {
                    match TcpStream::connect(("127.0.0.1", port)) {
                        Ok(s) => {
                            connected = Some(s);
                            break;
                        }
                        // The test sets `done` once it knows there is nothing to
                        // connect to, because `serve` could not bind the port.
                        Err(_) if done.load(Ordering::Relaxed) => break,
                        Err(_) => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
                let Some(mut s) = connected else {
                    done.store(true, Ordering::Relaxed);
                    return Vec::new();
                };
                // Whatever holds a port a lost race gave away is itself a listener, so
                // this may have connected to it rather than to the connector: never
                // wait on it for good.
                s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut replies = Vec::new();
                for pause in [0, 6] {
                    std::thread::sleep(Duration::from_secs(pause));
                    writeln!(s, "{}", block(&[])).unwrap();
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    replies.push(line);
                }
                done.store(true, Ordering::Relaxed);
                replies
            })
        };
        let mut events = Vec::new();
        // A bind failure here is the port having been taken between the pick and this
        // call; stop the client so the attempt can be run again on another port.
        if let Err(e) = serve(&c, &[port], &done, &mut |e| events.push(e)) {
            done.store(true, Ordering::Relaxed);
            let _ = client.join();
            return Err(e);
        }
        let replies = client.join().unwrap();
        assert_eq!(replies.len(), 2, "both polls were answered: {replies:?}");
        assert!(
            replies.iter().all(|r| r.contains("\"playerName\"")),
            "{replies:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::ClientDisconnected(_))),
            "{events:?}"
        );
        Ok(())
    });
}
