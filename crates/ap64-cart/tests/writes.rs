//! A write reaches the console once, and only the console it was decided against.
//!
//! The agent serves requests in order, one per game frame, and keeps no record of rids: a
//! `POKEV` the host gave up on is still served when the game reaches its hook, and a copy sent
//! after it lands on the next frame. A mailbox the game emptied between the two is filled again.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ap64_cart::backend::{link_restarted, Backend, Multi64};
use ap64_cart::m64p;
use common::{answer, poke_ack, Step};

/// An agent that goes quiet past one reply timeout, as at a scene load, and then serves the
/// write it was sent. The write is sent once and its late reply is the answer.
#[test]
fn a_write_answered_late_is_sent_once() {
    let late = Arc::new(AtomicBool::new(true));
    let fake = common::start(Arc::new(move |req| match req.msg {
        m64p::MSG_POKEV if late.swap(false, Ordering::SeqCst) => vec![
            Step::Sleep(Duration::from_millis(1600)),
            Step::Send(poke_ack(req.rid, req.writes.len() as u8)),
        ],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.write_many(&[(0x100, &[0x42, 0x00])]).unwrap();
    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert_eq!(
        pokes.len(),
        1,
        "the write reached the cart more than once: {pokes:?}"
    );
    assert_eq!(cart.stats().reconnects, 0);
}

/// The link drops with a write unanswered. The cart is reached again, but the write is not
/// sent to it: the console may have been reset, and the write belonged to the one before.
#[test]
fn a_write_is_not_sent_again_once_the_link_restarts() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_POKEV if req.conn == 0 => vec![Step::Close],
        _ => answer(req, 0x11),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(5));
    let e = cart
        .write_many(&[(0x100, &[0x42, 0x00])])
        .expect_err("a write the link restarted under must not report success");
    assert!(link_restarted(&e), "{e}");
    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert!(
        pokes.iter().all(|r| r.conn == 0),
        "the write was sent again after the restart: {pokes:?}"
    );
    // The link itself is back, on a new generation.
    assert_eq!(cart.generation(), 1);
    assert_eq!(cart.read_many(&[(0x100, 2)]).unwrap(), vec![vec![0x11; 2]]);
}

/// The same when the agent never answers at all: it is waited on, never sent again, and not
/// carried over to the link that replaces this one.
#[test]
fn a_write_never_answered_is_sent_once_and_not_carried_over() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_POKEV if req.conn == 0 => Vec::new(),
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(5));
    let e = cart.write_many(&[(0x100, &[0x42, 0x00])]).unwrap_err();
    assert!(link_restarted(&e), "{e}");
    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert_eq!(pokes.len(), 1, "{pokes:?}");
    assert_eq!(cart.generation(), 1);
}
