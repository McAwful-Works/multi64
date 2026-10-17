//! A write lands on the console once, and only on the console it was decided against.
//!
//! The agent serves requests in order, one per game frame: a `POKEV` the host gave up on is
//! still served when the game reaches its hook, and a copy sent after it lands on the next frame.
//! To an agent that cannot tell the copy from a new write, a mailbox the game emptied between the
//! two is filled again, so such an agent is never sent a write twice. One that can (`HELLO_ACK`
//! bit 3, spec 4.4) is sent it again on the same link, under the same rid, and a write it never
//! received still lands.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ap64_cart::backend::{link_restarted, Backend, Multi64};
use ap64_cart::m64p;
use common::{answer, poke_ack, Step, Writes};

/// An agent from before bit 3 goes quiet past one reply timeout, as at a scene load, and then
/// serves the write it was sent. The write is sent once and its late reply is the answer.
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

/// The write never reached an agent that recognizes a copy: a frame lost on the way, say. The
/// copy is what lands, once, and the link never restarts for it.
#[test]
fn a_write_the_agent_never_received_is_sent_again_and_lands() {
    let writes = Arc::new(Writes::default());
    let lost = Arc::new(AtomicBool::new(true));
    let fake = {
        let writes = writes.clone();
        common::start(Arc::new(move |req| match req.msg {
            m64p::MSG_POKEV if lost.swap(false, Ordering::SeqCst) => Vec::new(),
            _ => writes.serve(req, 0),
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.write_many(&[(0x100, &[0x42, 0x00])]).unwrap();

    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert_eq!(pokes.len(), 2, "sent, and sent again: {pokes:?}");
    assert_eq!(
        pokes[0], pokes[1],
        "the copy is the same write under the same rid"
    );
    assert_eq!(writes.applied().len(), 1);
    assert_eq!(writes.applied()[0].writes, vec![(0x100, vec![0x42, 0x00])]);
    assert_eq!(cart.stats().reconnects, 0);

    // The next write is a new one: it goes under a rid of its own and lands too.
    cart.write_many(&[(0x200, &[0x07])]).unwrap();
    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert_eq!(pokes.len(), 3, "{pokes:?}");
    assert_ne!(pokes[2].rid, pokes[1].rid);
    assert_eq!(writes.applied().len(), 2);
}

/// The write did land, but its reply came after the host stopped waiting, as at a scene load.
/// The copy sent meanwhile is acknowledged by the agent and applied by nobody.
#[test]
fn a_write_answered_late_is_sent_again_and_applied_once() {
    let writes = Arc::new(Writes::default());
    let late = Arc::new(AtomicBool::new(true));
    let fake = {
        let writes = writes.clone();
        common::start(Arc::new(move |req| {
            let mut steps = writes.serve(req, 0);
            // Past two reply timeouts: one wait can run a little over its timeout, and the
            // copy must go out before this reply does.
            if req.msg == m64p::MSG_POKEV && late.swap(false, Ordering::SeqCst) {
                steps.insert(0, Step::Sleep(Duration::from_millis(2600)));
            }
            steps
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.write_many(&[(0x100, &[0x42, 0x00])]).unwrap();
    // Answered behind the copy, which the agent serves in order: by the time this returns the
    // copy has been served too, and its acknowledgment, a reply to a rid already answered, has
    // been passed over.
    assert_eq!(cart.read_many(&[(0x100, 2)]).unwrap(), vec![vec![0; 2]]);

    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert!(pokes.len() >= 2, "sent again while it waited: {pokes:?}");
    assert!(pokes.iter().all(|r| *r == pokes[0]), "{pokes:?}");
    assert_eq!(
        writes.applied().len(),
        1,
        "the write landed twice: {:?}",
        writes.applied()
    );
    assert_eq!(cart.stats().reconnects, 0);
}

/// Sending a write again is for the same link only. An agent that recognizes copies forgets the
/// write at HELLO, and the console may have been reset besides: once the link restarts, the
/// write is not sent at all.
#[test]
fn a_write_sent_again_is_not_carried_over_to_a_restarted_link() {
    let writes = Arc::new(Writes::default());
    let fake = {
        let writes = writes.clone();
        common::start(Arc::new(move |req| match req.msg {
            m64p::MSG_POKEV if req.conn == 0 => Vec::new(),
            _ => writes.serve(req, 0x11),
        }))
    };
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_secs(5));
    let e = cart.write_many(&[(0x100, &[0x42, 0x00])]).unwrap_err();
    assert!(link_restarted(&e), "{e}");

    let pokes = fake.seen_of(m64p::MSG_POKEV);
    assert!(pokes.len() > 1, "sent again on the same link: {pokes:?}");
    assert!(
        pokes.iter().all(|r| r.conn == 0 && r.rid == pokes[0].rid),
        "every copy on the first link, under one rid: {pokes:?}"
    );
    assert!(writes.applied().is_empty());
    assert_eq!(cart.generation(), 1);
    assert_eq!(cart.read_many(&[(0x100, 2)]).unwrap(), vec![vec![0x11; 2]]);
}
