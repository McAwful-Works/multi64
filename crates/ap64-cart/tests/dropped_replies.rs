//! A reply the agent never sent, asked for again without waiting out the whole reply timeout.
//!
//! Measured on a SummerCart64 in Ocarina of Time (2026-10-18): the agent handles every request,
//! but now and then the reply never reaches the serial port, between 0.1% and 2% of requests
//! depending on where the game is. Each cost a whole [`REPLY_TIMEOUT`] (1.2 s) before the retry, and
//! three in a row cost a reconnect. An agent that has been answering every frame and has not
//! answered within a few round trips has almost certainly dropped the reply, so the request goes
//! again under the same rid, and whichever copy is answered first is the answer.
//!
//! [`REPLY_TIMEOUT`]: ap64_cart::transport

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ap64_cart::backend::{Backend, Multi64};
use ap64_cart::m64p;
use common::{answer, poke_ack, Step, Writes};

/// Reads answered promptly, enough of them to say how long a round trip takes.
fn warm_up(cart: &mut Multi64) {
    for _ in 0..12 {
        cart.read_many(&[(0x100, 4)]).unwrap();
    }
}

/// An agent that drops the reply to the `n`th read it is sent, counting from 1, and answers every
/// other request, copies included.
fn dropping_read(n: u32) -> common::Agent {
    let reads = Arc::new(AtomicU32::new(0));
    Arc::new(move |req| match req.msg {
        m64p::MSG_PEEKV if reads.fetch_add(1, Ordering::SeqCst) + 1 == n => Vec::new(),
        _ => answer(req, 0x5A),
    })
}

/// The agent drops one read's reply: the read is asked for again long before the reply timeout,
/// under the same rid, and answered.
#[test]
fn a_reply_the_agent_never_sent_is_asked_for_again_well_inside_the_timeout() {
    let fake = common::start(dropping_read(13));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    warm_up(&mut cart);
    let asked = Instant::now();
    assert_eq!(cart.read_many(&[(0x100, 4)]).unwrap(), vec![vec![0x5A; 4]]);
    let took = asked.elapsed();
    assert!(took < Duration::from_millis(700), "the read took {took:?}");
    let reads = fake.seen_of(m64p::MSG_PEEKV);
    let last = &reads[reads.len() - 1];
    let before = &reads[reads.len() - 2];
    assert_eq!(reads.len(), 14, "{reads:?}");
    assert_eq!(before.rid, last.rid, "the copy went under another rid");
    assert_eq!(cart.stats().stalls, 0);
    assert_eq!(cart.stats().quick_resends, 1);
}

/// The same for a write, to an agent that recognizes a copy: sent again early under its rid, and
/// applied once.
#[test]
fn a_write_whose_reply_was_never_sent_is_sent_again_early_and_applied_once() {
    let writes = Arc::new(Writes::default());
    let dropped = Arc::new(AtomicBool::new(false));
    let fake = {
        let writes = writes.clone();
        let dropped = dropped.clone();
        common::start(Arc::new(move |req| {
            let steps = writes.serve(req, 0x5A);
            if req.msg == m64p::MSG_POKEV && !dropped.swap(true, Ordering::SeqCst) {
                // Applied, and the acknowledgement never sent.
                Vec::new()
            } else {
                steps
            }
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    warm_up(&mut cart);
    let asked = Instant::now();
    cart.write_many(&[(0x200, &[1, 2, 3, 4])]).unwrap();
    let took = asked.elapsed();
    assert!(took < Duration::from_millis(700), "the write took {took:?}");
    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert_eq!(pokes.len(), 2, "{pokes:?}");
    assert_eq!(pokes[0].rid, pokes[1].rid);
    assert_eq!(writes.applied().len(), 1, "the write landed twice");
}

/// A write to an agent that cannot tell a copy from a new write is never sent early, however
/// long its reply takes: it is waited on, as before.
#[test]
fn a_write_to_an_agent_without_copies_is_not_sent_early() {
    let late = Arc::new(AtomicBool::new(false));
    let fake = {
        let late = late.clone();
        common::start(Arc::new(move |req| match req.msg {
            m64p::MSG_POKEV if !late.swap(true, Ordering::SeqCst) => vec![
                Step::Sleep(Duration::from_millis(500)),
                Step::Send(poke_ack(req.rid, req.writes.len() as u8)),
            ],
            _ => answer(req, 0),
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    warm_up(&mut cart);
    cart.write_many(&[(0x200, &[1, 2, 3, 4])]).unwrap();
    assert_eq!(fake.seen_of(m64p::MSG_POKEV).len(), 1);
    assert_eq!(cart.stats().quick_resends, 0);
}

/// Over an EverDrive a copy could be joined to a request a lost piece cut short, so nothing is
/// sent early there: the read waits out the timeout, as before, and is asked again under a new
/// rid.
#[test]
fn over_an_everdrive_nothing_is_sent_early() {
    let fake = common::start_with_cart(dropping_read(13), "ed64");
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    warm_up(&mut cart);
    let asked = Instant::now();
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert!(asked.elapsed() >= Duration::from_millis(1100));
    let reads = fake.seen_of(m64p::MSG_PEEKV);
    assert_ne!(reads[reads.len() - 1].rid, reads[reads.len() - 2].rid);
    assert_eq!(cart.stats().quick_resends, 0);
}

/// An agent that goes quiet, as at a scene load, gets one early copy of the request it was
/// sent, and none of the retries after it: by then it has not answered for longer than an
/// agent that is merely dropping a reply would.
#[test]
fn an_agent_gone_quiet_gets_one_early_copy_not_one_per_retry() {
    let quiet = Arc::new(AtomicBool::new(false));
    let fake = {
        let quiet = quiet.clone();
        common::start(Arc::new(move |req| match req.msg {
            m64p::MSG_PEEKV if quiet.load(Ordering::SeqCst) => Vec::new(),
            _ => answer(req, 0),
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::ZERO);
    warm_up(&mut cart);
    quiet.store(true, Ordering::SeqCst);
    cart.read_many(&[(0x100, 4)]).unwrap_err();
    let reads: Vec<_> = fake.seen_of(m64p::MSG_PEEKV).into_iter().skip(12).collect();
    // The first request and its early copy, then one request for each retry.
    assert_eq!(reads.len(), 4, "{reads:?}");
    assert_eq!(reads[0].rid, reads[1].rid);
    assert_ne!(reads[2].rid, reads[3].rid);
    assert_eq!(cart.stats().quick_resends, 1);
}
