//! `run_bringup` against a stand-in for `multi64d` and the bring-up ROM: a WebSocket server that
//! answers M64P from a fake RDRAM holding a report, and acts on the report's control words the way
//! the ROM does. It checks the host half end to end, not that any cart or ROM behaves like this.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use multi64_l3::{Channel, Frame, FrameFlags, FrameType, StreamDecoder};
use multi64_test_connector::bringup::{
    run_bringup, BringupOptions, Outcome, CART_SC64, CART_X_SERIES, LOAD_HEAVY, LOAD_MODERATE,
    REPORT_MAGIC, VARIANT_X7_DMA,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const REPORT: usize = 0x3EBA0;
const RAM: usize = 0x10_0000;
const CTL_LOAD: usize = 0x20;
const CTL_VARIANT: usize = 0x24;
/// The value the host writes to re-arm a control word.
const CTL_IGNORED: u32 = u32::MAX;

/// Which half of a request went missing.
#[derive(Clone, Copy, PartialEq)]
enum Lost {
    /// The agent never saw the write, so the ROM did not act on it.
    Request,
    /// The write landed and the ROM acts on it, but its `POKE_ACK` never reached the host.
    Reply,
}

struct Rom {
    ram: Vec<u8>,
    seen_load: u32,
    seen_variant: u32,
    /// Requests that go unanswered under heavy load, as an agent losing the PI would.
    drop_under_heavy: bool,
    /// Control writes that go unanswered, once per entry: the word's offset in the report, the
    /// value written, and which half is lost. As an agent that found the PI busy for one request.
    lose_control: Vec<(usize, u32, Lost)>,
    requests: u32,
}

impl Rom {
    fn new() -> Self {
        let mut ram = vec![0u8; RAM];
        let mut put = |off: usize, v: u32| ram[off..off + 4].copy_from_slice(&v.to_be_bytes());
        // A decoy first: the magic, with the wrong self address, where a scan meets it earlier.
        put(0x2000 + 0x10, REPORT as u32);
        // The report.
        put(REPORT + 0x0C, 0x1388);
        put(REPORT + 0x10, REPORT as u32);
        put(REPORT + 0x14, 0x0001_0000);
        put(REPORT + 0x1C, 46_875_000);
        put(REPORT + 0x30, CART_SC64);
        put(REPORT + 0x34, 1);
        put(REPORT + 0x6C, (1 << 4) | (1 << 2) | (1 << 1));
        put(REPORT + 0x70, 1 << 1);
        put(REPORT + 0x7C, 0x5343_7632);
        put(REPORT + 0x80, 0x8037_1240);
        put(REPORT + 0xC0, 1); // timing[0][0]: PI_STATUS
        put(REPORT + 0xC4, 100_000);
        put(REPORT + 0xC8, 4800);
        for c in 0..4 {
            put(REPORT + 0x148 + c * 20, 1); // buffer[0][c]: match
            put(REPORT + 0x148 + c * 20 + 4, u32::MAX);
            put(REPORT + 0x148 + 80 + c * 20, 1); // buffer[1][c]
            put(REPORT + 0x148 + 80 + c * 20 + 4, u32::MAX);
        }
        ram[0x2000..0x2008].copy_from_slice(&REPORT_MAGIC);
        ram[REPORT..REPORT + 8].copy_from_slice(&REPORT_MAGIC);
        Rom {
            ram,
            seen_load: 0,
            seen_variant: 0,
            drop_under_heavy: false,
            lose_control: Vec::new(),
            requests: 0,
        }
    }

    /// The same report as an X7's, its CPU-word build driving the link.
    fn x7() -> Self {
        let mut rom = Rom::new();
        rom.set(REPORT + 0x30, CART_X_SERIES);
        rom.set(REPORT + 0x34, 2);
        rom.set(REPORT + 0x70, 1 << 2);
        rom
    }

    fn word(&self, off: usize) -> u32 {
        u32::from_be_bytes(self.ram[off..off + 4].try_into().unwrap())
    }

    fn set(&mut self, off: usize, v: u32) {
        self.ram[off..off + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// What the ROM's main loop does once a frame: act on a changed control word, count.
    fn frame(&mut self) {
        let load = self.word(REPORT + 0x20);
        // As the ROM: a changed value is seen, and acted on only if it is a level.
        if load != self.seen_load {
            self.seen_load = load;
            if load <= LOAD_HEAVY {
                self.set(REPORT + 0x38, load);
            }
        }
        let variant = self.word(REPORT + 0x24);
        if variant != self.seen_variant {
            self.seen_variant = variant;
            // The ROM ignores a build that cannot drive the cart it found.
            let fits = match self.word(REPORT + 0x30) {
                CART_X_SERIES => variant == 2 || variant == 3,
                _ => variant == 1,
            };
            if fits {
                self.set(REPORT + 0x34, variant);
            }
        }
        // link[0].m64p_requests
        self.set(REPORT + 0x208 + 0x50, self.requests);
    }

    /// The entry of `lose_control` this request matches, taken out of the list: a `POKEV` of one
    /// word to a control word, carrying the value the entry names.
    fn lost_control(&mut self, p: &[u8]) -> Option<Lost> {
        if p[4] != 0x03 || p.len() != 18 || p[7] != 1 || p[12..14] != [0, 4] {
            return None;
        }
        let off = (u32::from_be_bytes(p[8..12].try_into().unwrap()) as usize).wrapping_sub(REPORT);
        let value = u32::from_be_bytes(p[14..18].try_into().unwrap());
        let at = self
            .lose_control
            .iter()
            .position(|&(o, v, _)| o == off && v == value)?;
        Some(self.lose_control.remove(at).2)
    }

    fn handle(&mut self, p: &[u8]) -> Option<Vec<u8>> {
        if p.len() < 5 || &p[0..4] != b"M64P" {
            return None;
        }
        self.frame();
        if self.drop_under_heavy && self.word(REPORT + 0x38) == LOAD_HEAVY {
            return None;
        }
        let lost = self.lost_control(p);
        if lost == Some(Lost::Request) {
            return None;
        }
        self.requests += 1;
        let mut out = b"M64P".to_vec();
        match p[4] {
            0x01 => {
                out.push(0x81);
                out.extend_from_slice(&[0, 0, 1]);
                out.extend_from_slice(&0x0080_0000u32.to_be_bytes());
                out.push(0x03);
                out.extend_from_slice(&0x0400_0000u32.to_be_bytes());
            }
            0x02 | 0x04 => {
                let rom = p[4] == 0x04;
                out.push(if rom { 0x84 } else { 0x82 });
                out.extend_from_slice(&p[5..8]);
                let n = p[7] as usize;
                for i in 0..n {
                    let at = 8 + i * 6;
                    let addr = u32::from_be_bytes(p[at..at + 4].try_into().unwrap()) as usize;
                    let len = u16::from_be_bytes([p[at + 4], p[at + 5]]) as usize;
                    out.extend_from_slice(&(len as u16).to_be_bytes());
                    if rom {
                        // The ROM header the report recorded, then zeros.
                        let mut b = vec![0u8; len];
                        let h = &self.ram[REPORT + 0x80..REPORT + 0xC0];
                        let k = len.min(64);
                        b[..k].copy_from_slice(
                            &h[addr.min(64)..addr.min(64) + k.min(64 - addr.min(64))],
                        );
                        out.extend_from_slice(&b);
                    } else {
                        out.extend_from_slice(&self.ram[addr..addr + len]);
                    }
                }
            }
            0x03 => {
                out.push(0x83);
                out.extend_from_slice(&p[5..7]);
                let n = p[7] as usize;
                let mut at = 8;
                for _ in 0..n {
                    let addr = u32::from_be_bytes(p[at..at + 4].try_into().unwrap()) as usize;
                    let len = u16::from_be_bytes([p[at + 4], p[at + 5]]) as usize;
                    self.ram[addr..addr + len].copy_from_slice(&p[at + 6..at + 6 + len]);
                    at += 6 + len;
                }
                out.push(n as u8);
            }
            _ => return None,
        }
        if lost == Some(Lost::Reply) {
            return None;
        }
        Some(out)
    }
}

async fn serve(rom: Arc<Mutex<Rom>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let rom = rom.clone();
            tokio::spawn(async move {
                let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let (mut tx, mut rx) = ws.split();
                tx.send(Message::text(r#"{"type":"hello"}"#)).await.unwrap();
                let mut decoder = StreamDecoder::new();
                while let Some(Ok(msg)) = rx.next().await {
                    match msg {
                        Message::Text(t) if t.contains("ping") => {
                            tx.send(Message::text(r#"{"type":"pong"}"#)).await.unwrap();
                        }
                        Message::Binary(b) => {
                            let mut replies = Vec::new();
                            decoder.push_bytes(&b, |f| {
                                if let Some(r) = rom.lock().unwrap().handle(&f.payload) {
                                    replies.push(r);
                                }
                            });
                            for payload in replies {
                                let frame = Frame {
                                    ty: FrameType::Data,
                                    channel: Channel::Application,
                                    flags: FrameFlags::FINAL,
                                    request_id: 0,
                                    payload,
                                };
                                tx.send(Message::binary(frame.encode().unwrap()))
                                    .await
                                    .unwrap();
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    url
}

fn options(url: String) -> BringupOptions {
    BringupOptions {
        ws_url: url,
        // Nothing listens here: the run notes that the daemon did not answer and carries on.
        base_url: "127.0.0.1:9".into(),
        recv_timeout_secs: 0.5,
        rounds: 1,
        default_variant_only: false,
    }
}

#[tokio::test]
async fn finds_the_report_past_a_decoy_and_runs_every_load() {
    let rom = Arc::new(Mutex::new(Rom::new()));
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();

    let get = |n: &str| {
        run.checks
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("no check {n}: {:#?}", run.checks))
    };
    assert_eq!(get("link.hello").outcome, Outcome::Pass);
    assert_eq!(run.report_addr, Some(REPORT as u32));
    assert_eq!(get("identify.cart").outcome, Outcome::Pass);
    assert_eq!(get("identify.driver_init").outcome, Outcome::Pass);
    assert_eq!(
        get("blocks.buffer.DMA>DMA.under_load").outcome,
        Outcome::Pass
    );
    for load in ["off", "moderate", "heavy"] {
        let c = get(&format!("link.default.load_{load}.traffic"));
        assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    }
    assert_eq!(run.phases.len(), 3);
    assert!(run.phases.iter().all(|p| p.wrong == 0 && p.timeouts == 0));
    // The load each phase asked for is the one the report showed during it.
    assert_eq!(
        run.phases
            .iter()
            .map(|p| p.after.as_ref().unwrap().load)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    // And the run leaves the ROM as it found it.
    assert_eq!(run.final_report.as_ref().unwrap().load, 0);
    assert!(run.failed() == 0, "{:#?}", run.checks);
}

#[tokio::test]
async fn heavy_load_timeouts_are_information_not_failure() {
    let rom = Arc::new(Mutex::new(Rom::new()));
    rom.lock().unwrap().drop_under_heavy = true;
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();

    let heavy = run
        .checks
        .iter()
        .find(|c| c.name == "link.default.load_heavy.traffic")
        .unwrap();
    assert_eq!(heavy.outcome, Outcome::Info, "{}", heavy.detail);
    // Three timeouts in a row stop the phase, rather than every request waiting out its timeout.
    assert_eq!(run.phases[2].timeouts, 3);
    assert!(run.notes.iter().any(|n| n.contains("stopped answering")));
}

#[tokio::test]
async fn a_silent_agent_is_a_failed_check_not_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let (mut tx, mut rx) = ws.split();
                tx.send(Message::text(r#"{"type":"hello"}"#)).await.unwrap();
                while let Some(Ok(msg)) = rx.next().await {
                    if let Message::Text(t) = msg {
                        if t.contains("ping") {
                            tx.send(Message::text(r#"{"type":"pong"}"#)).await.unwrap();
                        }
                    }
                }
            });
        }
    });
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();
    assert_eq!(run.checks.len(), 1);
    assert_eq!(run.checks[0].name, "link.hello");
    assert_eq!(run.checks[0].outcome, Outcome::Fail);
}

#[tokio::test]
async fn an_x7_runs_both_builds_and_is_left_on_the_one_it_booted_with() {
    let rom = Arc::new(Mutex::new(Rom::x7()));
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();

    assert!(run.failed() == 0, "{:#?}", run.checks);
    assert_eq!(
        run.phases.iter().map(|p| p.variant).collect::<Vec<_>>(),
        vec![2, 2, 2, 3, 3, 3]
    );
    assert!(run
        .checks
        .iter()
        .any(|c| c.name == "link.x7_dma.hello" && c.outcome == Outcome::Pass));
    assert!(run
        .checks
        .iter()
        .any(|c| c.name == "link.x7_dma.load_moderate.traffic"));
    assert_eq!(run.final_report.as_ref().unwrap().variant, 2);
}

#[tokio::test]
async fn no_report_is_a_failed_check_after_one_pass_over_the_range() {
    let rom = Arc::new(Mutex::new(Rom::new()));
    {
        let mut r = rom.lock().unwrap();
        r.ram[REPORT..REPORT + 8].fill(0);
    }
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();
    let c = run
        .checks
        .iter()
        .find(|c| c.name == "link.report_found")
        .unwrap();
    assert_eq!(c.outcome, Outcome::Fail);
    assert!(run.report_addr.is_none());
}

#[tokio::test]
async fn a_control_word_left_by_an_earlier_run_is_still_obeyed() {
    // ctl_load says off, but a controller press (Z) since has put the load on moderate. Writing
    // off again changes nothing the ROM can see, so the host must re-arm the word first.
    let rom = Arc::new(Mutex::new(Rom::new()));
    rom.lock().unwrap().set(REPORT + 0x38, 1);
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();
    assert!(run.failed() == 0, "{:#?}", run.checks);
    assert_eq!(
        run.phases
            .iter()
            .map(|p| p.after.as_ref().unwrap().load)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn a_control_write_that_goes_unanswered_is_sent_again_and_the_run_carries_on() {
    // Three control writes lost once each, as a cart under load leaves a request unanswered now
    // and then: the re-arm before the first load level, the moderate level itself, and the
    // acknowledgment of the switch to the DMA build, which the ROM has acted on by the time the
    // host writes it again.
    let rom = Arc::new(Mutex::new(Rom::x7()));
    rom.lock().unwrap().lose_control = vec![
        (CTL_LOAD, CTL_IGNORED, Lost::Request),
        (CTL_LOAD, LOAD_MODERATE, Lost::Request),
        (CTL_VARIANT, VARIANT_X7_DMA, Lost::Reply),
    ];
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();

    assert!(rom.lock().unwrap().lose_control.is_empty());
    assert!(run.failed() == 0, "{:#?}", run.checks);
    assert!(!run.checks.iter().any(|c| c.name.ends_with(".aborted")));
    // Every phase of both builds ran, each under the load it asked for.
    assert_eq!(
        run.phases
            .iter()
            .map(|p| (p.variant, p.after.as_ref().unwrap().load))
            .collect::<Vec<_>>(),
        vec![(2, 0), (2, 1), (2, 2), (3, 0), (3, 1), (3, 2)]
    );
    // And the run says which writes it had to repeat.
    let retried: Vec<&String> = run.notes.iter().filter(|n| n.contains("try 1")).collect();
    assert_eq!(retried.len(), 3, "{:#?}", run.notes);
    assert!(retried[0].starts_with("ctl_load: writing 0xffffffff got timeout"));
    assert!(retried[1].starts_with("ctl_load: writing 0x1 got timeout"));
    assert!(retried[2].starts_with("ctl_variant: writing 0x3 got timeout"));
}

#[tokio::test]
async fn a_control_write_never_answered_stops_the_run_after_three_tries() {
    let rom = Arc::new(Mutex::new(Rom::new()));
    rom.lock().unwrap().lose_control = vec![(CTL_LOAD, LOAD_MODERATE, Lost::Request); 4];
    let url = serve(rom.clone()).await;
    let mut log = |_: String| {};
    let run = run_bringup(&options(url), &mut log).await.unwrap();

    let c = run
        .checks
        .iter()
        .find(|c| c.name == "link.default.aborted")
        .unwrap_or_else(|| panic!("the run was not stopped: {:#?}", run.checks));
    assert_eq!(c.outcome, Outcome::Fail);
    assert!(
        c.detail.contains("ctl_load") && c.detail.contains("3 tries"),
        "{}",
        c.detail
    );
    // Three tries, not four: one entry is left.
    assert_eq!(rom.lock().unwrap().lose_control.len(), 1);
    assert_eq!(run.phases.len(), 1);
}
