//! The forked Banjo-Tooie connector against a synthetic RDRAM image, driven with lines
//! shaped like Banjo-Tooie Client's, through `Connector::handle`.
//!
//! The image is the randomizer's own data block: a pointer at `0x400000` to a struct of
//! pointers, which is what every BTHACK accessor dereferences twice before it reads
//! anything. That chain is the reason `cartmem` exists, so these check the cost as well as
//! the behaviour.

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use ap64_cart::backend::{Backend, RamImage};
use ap64_connector::{Connector, BT};

/// The anchor the randomizer's boot DMA fills, and the emu_loader checks.
const BASE_INDEX: u32 = 0x40_0000;
/// Where this image puts the struct the anchor points at.
const STRUCT: u32 = 0x41_0000;

// Offsets inside that struct, from the connector's own BTHACK table.
/// The ROM version sits inline at the struct's start rather than behind a pointer
/// (`BTHACK.version = 0x0`), so it is the struct address itself.
const VERSION: u32 = STRUCT;
const F_PC: u32 = 0x4;
const F_MESSAGES: u32 = 0x8;
const F_SIGNPOST_MSG: u32 = 0xC;
const F_SETTINGS: u32 = 0x10;
const F_ITEMS: u32 = 0x14;
const F_TRAPS: u32 = 0x18;
const F_EXIT_MAP: u32 = 0x1C;
const F_N64: u32 = 0x20;
const F_REAL_FLAGS: u32 = 0x24;
const F_FAKE_FLAGS: u32 = 0x28;
const F_NEST_FLAGS: u32 = 0x2C;
const F_SIGNPOST_FLAGS: u32 = 0x30;

// Where each pointed-at block lives in this image.
const PC: u32 = 0x42_0000;
const MESSAGES: u32 = 0x42_1000;
const SIGNPOST_MSG: u32 = 0x42_2000;
const SETTINGS: u32 = 0x42_3000;
const ITEMS: u32 = 0x42_4000;
const TRAPS: u32 = 0x42_5000;
const EXIT_MAP: u32 = 0x42_6000;
const N64: u32 = 0x42_7000;
const REAL_FLAGS: u32 = 0x42_8000;
const FAKE_FLAGS: u32 = 0x42_9000;
const NEST_FLAGS: u32 = 0x42_A000;
const SIGNPOST_FLAGS: u32 = 0x42_B000;

/// `n64 + current_map`, a u16.
const N64_CURRENT_MAP: u32 = N64 + 0x6;
/// `pc + pc_death_us`, the counter the game raises when Banjo dies.
const PC_DEATH_US: u32 = PC;

fn ram() -> Vec<u8> {
    let mut ram = vec![0u8; 0x80_0000];
    let be32 = |ram: &mut Vec<u8>, a: u32, v: u32| {
        ram[a as usize..a as usize + 4].copy_from_slice(&v.to_be_bytes())
    };

    // The anchor, then the struct of pointers. Every value has to look like RDRAM to
    // BTHACK:isPointer, which wants 0x80000000..0x80800000.
    be32(&mut ram, BASE_INDEX, 0x8000_0000 + STRUCT);
    for (field, target) in [
        (F_PC, PC),
        (F_MESSAGES, MESSAGES),
        (F_SIGNPOST_MSG, SIGNPOST_MSG),
        (F_SETTINGS, SETTINGS),
        (F_ITEMS, ITEMS),
        (F_TRAPS, TRAPS),
        (F_EXIT_MAP, EXIT_MAP),
        (F_N64, N64),
        (F_REAL_FLAGS, REAL_FLAGS),
        (F_FAKE_FLAGS, FAKE_FLAGS),
        (F_NEST_FLAGS, NEST_FLAGS),
        (F_SIGNPOST_FLAGS, SIGNPOST_FLAGS),
    ] {
        be32(&mut ram, STRUCT + field, 0x8000_0000 + target);
    }

    // The ROM version, read as u16 major / u8 minor / u8 patch. "0" means the randomizer
    // has not populated its block, which the fork treats as not ready.
    // 4.13.1: the version a real Banjo-Tooie AP ROM reports, not the one the forked Lua
    // happened to carry. These two -- and slot()'s slot_version -- are what force
    // connector.lua's BT_VERSION to stay equal to the apworld it is used with. When they
    // agreed with the fork instead of with reality, every test here passed while a console
    // session latched VERROR on the slot and answered keep-alives forever.
    ram[VERSION as usize..VERSION as usize + 2].copy_from_slice(&4u16.to_be_bytes());
    ram[(VERSION + 2) as usize] = 13;
    ram[(VERSION + 3) as usize] = 1;

    ram[N64_CURRENT_MAP as usize..N64_CURRENT_MAP as usize + 2]
        .copy_from_slice(&0x0142u16.to_be_bytes());
    ram
}

/// A RAM image that counts the calls made on it, and lets the test change memory.
struct Counted {
    ram: Rc<RefCell<RamImage>>,
    reads: Rc<Cell<u32>>,
    writes: Rc<Cell<u32>>,
    /// Regions and bytes asked for by the most recent read.
    shape: Rc<Cell<(usize, usize)>>,
}

impl Backend for Counted {
    fn rdram_size(&self) -> u32 {
        self.ram.borrow().rdram_size()
    }
    fn read_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        self.reads.set(self.reads.get() + 1);
        self.shape
            .set((r.len(), r.iter().map(|&(_, l)| l).sum::<usize>()));
        self.ram.borrow_mut().read_many(r)
    }
    fn write_many(&mut self, w: &[(u32, &[u8])]) -> io::Result<()> {
        self.writes.set(self.writes.get() + 1);
        self.ram.borrow_mut().write_many(w)
    }
    fn rom_window(&self) -> Option<u32> {
        Some(0x0400_0000)
    }
    fn read_rom_many(&mut self, r: &[(u32, usize)]) -> io::Result<Vec<Vec<u8>>> {
        Ok(r.iter().map(|&(_, l)| vec![0; l]).collect())
    }
}

struct Harness {
    connector: Connector,
    ram: Rc<RefCell<RamImage>>,
    reads: Rc<Cell<u32>>,
    writes: Rc<Cell<u32>>,
    shape: Rc<Cell<(usize, usize)>>,
}

impl Harness {
    fn new() -> Self {
        Self::from_ram(ram())
    }

    fn from_ram(image: Vec<u8>) -> Self {
        let ram = Rc::new(RefCell::new(RamImage::new(image)));
        let reads = Rc::new(Cell::new(0));
        let writes = Rc::new(Cell::new(0));
        let shape = Rc::new(Cell::new((0, 0)));
        let backend = Counted {
            ram: ram.clone(),
            reads: reads.clone(),
            writes: writes.clone(),
            shape: shape.clone(),
        };
        let connector = Connector::new(&BT, Box::new(backend), ap64_cart::quiet()).unwrap();
        Harness {
            connector,
            ram,
            reads,
            writes,
            shape,
        }
    }

    fn handle(&self, line: &str) -> String {
        self.connector.handle(line).unwrap()
    }

    fn byte(&self, a: u32) -> u8 {
        self.ram.borrow_mut().read_many(&[(a, 1)]).unwrap()[0][0]
    }

    fn set(&self, a: u32, v: u8) {
        self.ram.borrow_mut().write_many(&[(a, &[v])]).unwrap();
    }

    /// Get past the handshake: one payload to be asked for the slot, then the slot.
    fn connect(&self) -> String {
        let first = self.handle(&payload(&[]));
        assert!(
            first.contains(r#""getSlot":true"#),
            "the first line should be answered with a request for the slot, got {first}"
        );
        self.handle(slot())
    }
}

/// A slot payload, as BTClient's `get_slot_payload` builds one. Only the fields the
/// connector reads are here; it tolerates the rest being absent.
fn slot() -> &'static str {
    concat!(
        r#"{"slot_player":"Banjo","slot_seed":12345,"slot_version":"4.13.1","#,
        r#""slot_deathlink":0,"slot_taglink":0,"slot_worlds":{},"#,
        r#""slot_open_hag1":0,"slot_skip_puzzles":0,"slot_dialog_character":110,"#,
        r#""slot_victory_condition":0,"slot_minigame_hunt":0,"slot_boss_hunt":0,"#,
        r#""slot_jinjo_family_rescue":0,"slot_token_hunt":0,"slot_honeycomb":0,"#,
        r#""slot_pages":0,"slot_jiggy_chunks":0,"slot_silo_costs":{}}"#,
    )
}

/// A poll payload, as BTClient's `get_payload` builds one.
fn payload(items: &[u32]) -> String {
    let items: Vec<String> = items.iter().map(u32::to_string).collect();
    format!(
        r#"{{"items":[{}],"messages":[],"playerNames":["Banjo"],"triggerDeath":false,"triggerTag":false}}"#,
        items.join(",")
    )
}

#[test]
fn the_first_line_is_answered_with_a_request_for_the_slot() {
    let h = Harness::new();
    // The client writes before it is asked for anything, so this is the line that arrives
    // first, and the reply is what makes it send its slot data next.
    let reply = h.handle(&payload(&[]));
    assert!(reply.contains(r#""getSlot":true"#), "{reply}");
}

#[test]
fn an_unpopulated_data_block_is_answered_with_a_keep_alive() {
    // The anchor reads as zero until the randomizer's boot DMA has run and the game has
    // booted far enough. Upstream spun on emu.frameadvance() waiting for it; the fork has
    // no frames to spin on, so it must answer and wait for the next poll instead.
    let mut image = ram();
    image[BASE_INDEX as usize..BASE_INDEX as usize + 4].copy_from_slice(&0u32.to_be_bytes());
    let h = Harness::from_ram(image);

    let reply = h.handle(&payload(&[]));
    assert!(
        reply.contains("scriptVersion"),
        "a keep-alive still reports the script version: {reply}"
    );
    assert!(
        !reply.contains("jiggies"),
        "a reply without jiggies is what the client treats as a keep-alive: {reply}"
    );
    assert!(
        !reply.contains("getSlot"),
        "the slot cannot be asked for before the block exists: {reply}"
    );
}

#[test]
fn the_slot_is_answered_with_the_first_real_request() {
    let h = Harness::new();
    let reply = h.connect();
    // Everything BTClient.parse_payload reads out of a poll.
    for key in [
        r#""scriptVersion":5"#,
        r#""playerName":"Banjo""#,
        r#""jiggies""#,
        r#""notes""#,
        r#""banjo_map""#,
        r#""sync_ready":"true""#,
    ] {
        assert!(reply.contains(key), "{key} missing from {reply}");
    }
}

#[test]
fn the_script_version_matches_what_the_client_requires() {
    // BTClient refuses the connection outright when scriptVersion is below its own
    // script_version, so this is the one field that stops everything if it drifts.
    let h = Harness::new();
    let reply = h.connect();
    assert!(
        reply.contains(r#""scriptVersion":5"#),
        "the client requires 5: {reply}"
    );
}

#[test]
fn a_collected_flag_is_reported_as_a_check() {
    let h = Harness::new();
    let before = h.connect();
    assert!(before.contains(r#""jiggies""#));

    // Set every bit of the real-flag window and the reply must change: the 562 locations
    // share 90 offsets inside 0x03..0x9E, all read through the real_flags pointer.
    for off in 0x00..0xA0u32 {
        h.set(REAL_FLAGS + off, 0xFF);
    }
    let after = h.handle(&payload(&[]));
    assert_ne!(
        before, after,
        "setting every location flag should change what is reported"
    );
    assert!(
        after.contains("true"),
        "something should now read as collected: {after}"
    );
}

#[test]
fn the_current_map_is_reported() {
    let h = Harness::new();
    let reply = h.connect();
    assert!(
        reply.contains(r#""banjo_map":322"#),
        "0x142 is the map the image is set to: {reply}"
    );
}

#[test]
fn a_settled_poll_is_a_handful_of_requests() {
    // The point of cartmem. Every BTHACK accessor dereferences twice before it reads, so
    // 562 locations uncached is about 1,700 round trips. Cached, the derefs all land on
    // the anchor and the struct, and the flags are one window.
    let h = Harness::new();
    h.connect();
    h.handle(&payload(&[]));

    h.reads.set(0);
    h.writes.set(0);
    h.handle(&payload(&[]));
    let reads = h.reads.get();
    assert!(
        (1..=4).contains(&reads),
        "a settled poll took {reads} read requests; the learned set should collapse it"
    );

    let (regions, bytes) = h.shape.get();
    assert!(
        regions <= 32,
        "{regions} regions exceeds what one M64P request can carry"
    );
    assert!(
        bytes < 8192,
        "a settled poll read {bytes} bytes; coalescing is meant to bridge gaps, not swallow pages"
    );
}

#[test]
fn writes_are_visible_to_reads_later_in_the_same_poll() {
    // setPCDeath reads the counter, writes counter + 1, and SendToBTClient reads it again
    // in the same pass. Read straight through to the cart, the second read would miss the
    // queued write and the counter would never advance.
    let h = Harness::new();
    h.connect();

    // Make the game's death counter differ from the client's, which is what drives
    // setPCDeath in SendToBTClient.
    // pc_death_us is the first byte of the pc block; n64_death_us is n64 + 1.
    h.set(PC_DEATH_US, 3);
    h.set(N64 + 0x1, 0);
    h.handle(&payload(&[]));
    assert_eq!(
        h.byte(PC_DEATH_US),
        4,
        "the death counter should have been incremented once"
    );
}

#[test]
fn an_idle_poll_still_costs_one_write_request() {
    // Not what you would guess, and worth pinning because it sets the steady-state cost.
    // Upstream's messageQueue() re-asserts the dialog character every pass the queue is
    // empty, so there is no such thing as a read-only Banjo-Tooie poll -- unlike DKR's,
    // which writes nothing when nothing happened. One write request, not forty: the
    // writes are queued and flushed together.
    let h = Harness::new();
    h.connect();
    h.handle(&payload(&[]));

    h.writes.set(0);
    h.handle(&payload(&[]));
    assert_eq!(
        h.writes.get(),
        1,
        "an idle poll should be exactly one batched write request"
    );
}

/// A session has to keep working after the slot, not just answer the slot.
///
/// This is the shape of the bug that cost an evening on a console. connector.lua's
/// BT_VERSION was the fork's "4.11.6" while the client and the ROM both said "4.13.1", so
/// process_slot() latched VERROR and every poll from the third onward returned a bare
/// keep-alive. The client showed nothing but "will be sent when Banjo-Tooie is loaded",
/// the link stayed up, and nothing anywhere said why.
///
/// Every other test here stops at the first real reply, which is exactly one poll too
/// early to see it. This one keeps polling.
#[test]
fn polls_after_the_slot_keep_carrying_game_data() {
    let h = Harness::new();
    h.connect();

    for poll in 1..=4 {
        let reply = h.handle(&payload(&[]));
        assert!(
            reply.contains("jiggies"),
            "poll {poll} after the slot returned a keep-alive, not game data: {reply}.\n\
             A latched VERROR does this -- check connector.lua's BT_VERSION against the \
             version the ROM and the client report."
        );
    }
}

/// The version the fork claims must be the version the tests model, which is the version a
/// real ROM reports. Pinned on its own so a drift names itself instead of surfacing as four
/// unrelated assertion failures.
#[test]
fn the_connector_claims_the_version_the_rom_reports() {
    let lua = include_str!("../connectors/bt/connector.lua");
    let claimed = lua
        .lines()
        .find_map(|l| l.strip_prefix("local BT_VERSION = "))
        .map(|v| v.trim().trim_matches('"'))
        .expect("connector.lua declares BT_VERSION");
    assert_eq!(
        claimed, "4.13.1",
        "connector.lua's BT_VERSION drifted from the apworld the profile targets; \
         a mismatch is silent at runtime -- it latches VERROR and answers keep-alives"
    );
}
