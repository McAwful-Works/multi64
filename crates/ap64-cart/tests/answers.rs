//! What [`Multi64`] does with an agent that is there and answering, but not with the answer
//! asked for: busy, a refusal, a reply too late for its request, a reply meant for a link that has
//! since been rebuilt. None of these is a cart that has gone, so none of them is worth a
//! reconnect: that redoes HELLO, drops the cached ROM and moves the link generation, which reloads
//! a script and lets a relay's client go.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ap64_cart::backend::{Backend, Multi64};
use ap64_cart::m64p;
use ap64_cart::watch::Watch;
use common::{
    answer, err, hello_ack, hello_ack_watch, peek_resp, peek_resp_with_events, watch_ack, whole,
    Step,
};

/// The game holds the PI bus, so the agent answers a ROM read `E_BUSY` a few times: asked again
/// on the same link, it is answered once the bus is free.
#[test]
fn a_busy_rom_read_is_asked_again_on_the_same_link() {
    let busy = Arc::new(AtomicU32::new(3));
    let fake = common::start(Arc::new(move |req| match req.msg {
        m64p::MSG_PEEKROM if busy.load(Ordering::SeqCst) > 0 => {
            busy.fetch_sub(1, Ordering::SeqCst);
            vec![Step::Send(err(req.rid, m64p::E_BUSY))]
        }
        _ => answer(req, 0x5A),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(5));
    assert_eq!(cart.read_rom_many(&[(0, 4)]).unwrap(), vec![vec![0x5A; 4]]);
    assert_eq!(cart.stats().reconnects, 0, "a busy bus cost a reconnect");
    assert_eq!(cart.generation(), 0);
    assert_eq!(fake.seen_of(m64p::MSG_HELLO).len(), 1);
}

/// An agent that refuses a read has answered it, and would answer a copy the same way, on this
/// link or any other: the read fails at once, and the link stays as it is.
#[test]
fn a_refused_read_fails_at_once_and_the_link_is_kept() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_PEEKV => vec![Step::Send(err(req.rid, 0x04))],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(3));
    let asked = Instant::now();
    let e = cart.read_many(&[(0x100, 4)]).unwrap_err();
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "a refusal was retried for {:?}: {e}",
        asked.elapsed()
    );
    assert!(e.to_string().contains("E_RANGE"), "{e}");
    assert_eq!(cart.stats().reconnects, 0);
    assert_eq!(fake.seen_of(m64p::MSG_HELLO).len(), 1);
}

/// An agent that will not watch the slot is asked once. The host samples it from then on, and
/// every read after does not ask again.
#[test]
fn a_refused_watch_is_asked_once_and_sampled_instead() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_HELLO => vec![Step::Send(hello_ack_watch())],
        m64p::MSG_WATCH => vec![Step::Send(err(req.rid, 0x06))],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(2));
    cart.set_watch(Watch::new(0x200, 2, None));
    cart.read_many(&[(0x100, 4)]).unwrap();
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(fake.seen_of(m64p::MSG_WATCH).len(), 1);
    assert_eq!(cart.stats().reconnects, 0);
    // Sampled from here: the probe rides each read.
    let reads = fake.seen_of(m64p::MSG_PEEKV);
    assert!(
        reads.iter().all(|r| r.regions.contains(&(0x200, 2))),
        "{reads:?}"
    );
}

/// The agent hands each watch event out once, on whichever response goes next. A read that
/// stalls past one reply timeout is asked again, and the late answer to the first copy carries
/// the event: it is kept, though the answer itself is not the one waited for.
#[test]
fn an_event_on_a_reply_given_up_on_is_kept() {
    let late = Arc::new(AtomicBool::new(true));
    let fake = common::start(Arc::new(move |req| match req.msg {
        m64p::MSG_HELLO => vec![Step::Send(hello_ack_watch())],
        m64p::MSG_WATCH => vec![Step::Send(watch_ack(req.rid, 1))],
        m64p::MSG_PEEKV if late.swap(false, Ordering::SeqCst) => vec![
            Step::Sleep(Duration::from_millis(1500)),
            Step::Send(peek_resp_with_events(
                req.rid,
                &whole(req, 0),
                &[vec![7, 7]],
                0,
            )),
        ],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_watch(Watch::new(0x200, 2, None));
    cart.read_many(&[(0x100, 4)]).unwrap();
    assert_eq!(
        fake.seen_of(m64p::MSG_PEEKV).len(),
        2,
        "the read was not asked again"
    );
    assert_eq!(cart.take_watched(), Some(vec![7, 7]));
}

/// multi64d hands every client the whole stream from the cart, so a link rebuilt after a stall
/// hears the old link's late replies before its own `HELLO_ACK`. They are not an answer to HELLO.
#[test]
fn a_hello_is_answered_past_a_late_reply_meant_for_another_link() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_HELLO => vec![
            Step::Send(peek_resp(7, &[vec![0; 4]], false)),
            Step::Send(hello_ack()),
        ],
        _ => answer(req, 0),
    }));
    Multi64::connect(&fake.url, ap64_cart::quiet())
        .expect("a late reply was taken for the answer to HELLO");
}

/// The probe a quiet session sends says when the console stops answering and when it starts
/// again, through the same health callback a request does. One miss is a scene load, not a
/// console gone, so it takes as many misses in a row as a request's own retries.
#[test]
fn the_idle_probe_says_when_the_console_stops_and_starts_answering() {
    let silent = Arc::new(AtomicBool::new(false));
    let quiet = silent.clone();
    let fake = common::start(Arc::new(move |req| match req.msg {
        m64p::MSG_PEEKV if quiet.load(Ordering::SeqCst) => Vec::new(),
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    let said = Rc::new(RefCell::new(Vec::new()));
    let told = said.clone();
    cart.set_health(Rc::new(move |ok| told.borrow_mut().push(ok)));

    assert!(cart.alive());
    assert!(said.borrow().is_empty(), "a console answering is not news");

    silent.store(true, Ordering::SeqCst);
    let mut misses = 0;
    while said.borrow().is_empty() && misses < 5 {
        assert!(!cart.alive());
        misses += 1;
    }
    assert_eq!(*said.borrow(), vec![false], "never said it stopped");
    assert_eq!(misses, 3, "said so after {misses} misses");

    silent.store(false, Ordering::SeqCst);
    assert!(cart.alive());
    assert_eq!(*said.borrow(), vec![false, true]);
}

/// A silent agent costs one reply timeout an attempt, and a request its retries: what the retry
/// budget is reckoned in (`REPLY_TIMEOUT`, 1.2 s, three attempts inside the client's 5 s). A
/// socket read that came back a moment early used to wait the whole timeout again, so one
/// attempt could take twice as long.
#[test]
fn a_silent_agent_costs_one_reply_timeout_an_attempt() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_PEEKV => Vec::new(),
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    // No reconnect: the error comes back as soon as the retries on this link are spent.
    cart.set_reconnect_deadline(Duration::ZERO);
    let asked = Instant::now();
    cart.read_many(&[(0x100, 4)]).unwrap_err();
    let took = asked.elapsed();
    assert_eq!(fake.seen_of(m64p::MSG_PEEKV).len(), 3);
    assert!(
        took < Duration::from_millis(3 * 1200 + 400),
        "three attempts took {took:?}"
    );
}
