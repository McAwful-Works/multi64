//! A console reset into another ROM without the link to the cart restarting.
//!
//! A reset no longer costs a reconnect: the agent goes quiet, a request is retried on the same
//! link, and the new ROM's agent answers it. Nothing then said the ROM had changed, and the ROM
//! pages cached from the old one went on being served as the cart's. So once the cart answers
//! after missing a request, its ROM header is read again, and a different one is greeted as a
//! new link.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ap64_cart::backend::{link_restarted, Backend, Multi64};
use ap64_cart::m64p;
use common::{answer, Step};

/// Half the reply timeout (1.2 s): a request sent while the console is quiet goes unanswered,
/// and its retry does not.
const QUIET_FOR: Duration = Duration::from_millis(600);

/// The console: its ROM reads back as `rom` throughout, and it answers nothing while `quiet` is
/// set, as a console between a reset and its new ROM's first frame does not. With `refuse_rom`
/// set it refuses `PEEKROM`, as an agent from before it does.
#[derive(Clone)]
struct Console {
    rom: Arc<AtomicU8>,
    quiet: Arc<AtomicBool>,
    refuse_rom: Arc<AtomicBool>,
}

impl Console {
    fn new() -> Self {
        Self {
            rom: Arc::new(AtomicU8::new(0x11)),
            quiet: Arc::new(AtomicBool::new(false)),
            refuse_rom: Arc::new(AtomicBool::new(false)),
        }
    }

    fn agent(&self, hello: fn() -> Vec<u8>) -> common::Agent {
        let c = self.clone();
        Arc::new(move |req| match req.msg {
            m64p::MSG_HELLO => vec![Step::Send(hello())],
            _ if c.quiet.load(Ordering::SeqCst) => Vec::new(),
            m64p::MSG_PEEKROM if c.refuse_rom.load(Ordering::SeqCst) => {
                vec![Step::Send(common::err(req.rid, 0x02))]
            }
            _ => answer(req, c.rom.load(Ordering::SeqCst)),
        })
    }

    /// The reset: quiet now, holding `rom`, and answering again a moment later.
    fn reset_into(&self, rom: u8) {
        self.quiet.store(true, Ordering::SeqCst);
        self.rom.store(rom, Ordering::SeqCst);
        let quiet = self.quiet.clone();
        std::thread::spawn(move || {
            std::thread::sleep(QUIET_FOR);
            quiet.store(false, Ordering::SeqCst);
        });
    }
}

/// A cart whose header has been read, so there is a ROM to compare.
fn connected(console: &Console, hello: fn() -> Vec<u8>) -> (Multi64, common::Fake) {
    let fake = common::start(console.agent(hello));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    assert_eq!(
        cart.read_rom_many(&[(0, 0x40)]).unwrap()[0],
        vec![0x11; 0x40]
    );
    (cart, fake)
}

/// The cart answers again holding another ROM: a new link, greeted with a new `HELLO`, and the
/// ROM read from the cart rather than from what the old one left behind.
#[test]
fn a_rom_changed_under_a_quiet_spell_is_greeted_as_a_new_link() {
    let console = Console::new();
    let (mut cart, fake) = connected(&console, common::hello_ack);
    console.reset_into(0x22);
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(cart.generation(), 1);
    assert_eq!(fake.seen_of(m64p::MSG_HELLO).len(), 2);
    assert_eq!(
        cart.read_rom_many(&[(0, 0x40)]).unwrap()[0],
        vec![0x22; 0x40]
    );
}

/// The same ROM after a quiet spell, a scene load as often as anything, is the same link: one
/// small read to say so, and the cache kept.
#[test]
fn the_same_rom_after_a_quiet_spell_is_the_same_link() {
    let console = Console::new();
    let (mut cart, fake) = connected(&console, common::hello_ack);
    console.reset_into(0x11);
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(cart.generation(), 0);
    let rom_reads = fake.seen_of(m64p::MSG_PEEKROM).len();
    assert_eq!(
        rom_reads, 2,
        "the header is read once to fill the cache and once to check it"
    );
    cart.read_rom_many(&[(0, 0x40)]).unwrap();
    assert_eq!(
        fake.seen_of(m64p::MSG_PEEKROM).len(),
        rom_reads,
        "the cache was dropped"
    );
}

/// A session with nothing to ask only probes the cart, and a probe that finds the cart answering
/// again after misses checks the ROM the same way.
#[test]
fn a_probe_that_finds_another_rom_counts_as_a_new_link() {
    let console = Console::new();
    let (mut cart, _fake) = connected(&console, common::hello_ack);
    console.quiet.store(true, Ordering::SeqCst);
    assert!(!cart.alive());
    console.rom.store(0x22, Ordering::SeqCst);
    console.quiet.store(false, Ordering::SeqCst);
    assert!(cart.alive());
    assert_eq!(cart.generation(), 1);
    assert_eq!(
        cart.read_rom_many(&[(0, 0x40)]).unwrap()[0],
        vec![0x22; 0x40]
    );
}

/// The agent that served the header read it; one that refuses to is another agent, in another
/// ROM, and is not asked again after every answer for the rest of the session.
#[test]
fn an_agent_that_refuses_the_header_is_another_rom() {
    let console = Console::new();
    let (mut cart, fake) = connected(&console, common::hello_ack);
    console.refuse_rom.store(true, Ordering::SeqCst);
    console.reset_into(0x11);
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(cart.generation(), 1);
    let rom_reads = fake.seen_of(m64p::MSG_PEEKROM).len();
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(fake.seen_of(m64p::MSG_PEEKROM).len(), rom_reads);
}

/// A ROM read the console was reset under fails, rather than stitching one image's pages to
/// another's or reading a page the change threw away.
#[test]
fn a_rom_read_the_console_was_reset_under_fails() {
    let console = Console::new();
    let (mut cart, _fake) = connected(&console, common::hello_ack);
    // Page 0 is cached; the next page is not, and is asked for while the console is reset.
    console.reset_into(0x22);
    let e = cart.read_rom_many(&[(0, 0x800)]).unwrap_err();
    assert!(link_restarted(&e), "{e}");
    assert_eq!(
        cart.read_rom_many(&[(0, 0x800)]).unwrap()[0],
        vec![0x22; 0x800]
    );
}

/// A write that goes unanswered while the console is reset into another ROM is not sent again
/// to that ROM's agent, which would apply it: the write fails as one the link restarted under.
#[test]
fn a_write_is_not_sent_again_to_another_rom() {
    let console = Console::new();
    let (mut cart, fake) = connected(&console, common::hello_ack_poke_once);
    console.reset_into(0x22);
    let e = cart.write_many(&[(0x100, &[1, 2, 3, 4])]).unwrap_err();
    assert!(link_restarted(&e), "{e}");
    assert_eq!(
        fake.seen_of(m64p::MSG_POKEV).len(),
        1,
        "the write was sent again"
    );
    assert_eq!(cart.generation(), 1);
}
