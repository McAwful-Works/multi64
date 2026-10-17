//! What [`Multi64`] makes of a reply that parses but does not answer what was asked.

mod common;

use std::sync::Arc;
use std::time::Duration;

use ap64_cart::backend::{Backend, Multi64};
use ap64_cart::m64p;
use common::{answer, peek_resp, whole, Step};

/// Each block one byte short of what was asked for.
fn short(req: &common::Request) -> Vec<Vec<u8>> {
    whole(req, 0xAA)
        .into_iter()
        .map(|mut b| {
            b.pop();
            b
        })
        .collect()
}

/// A cart that answers every read one byte short: the read fails, it is not handed back short.
/// Every caller slices what comes back by the length it asked for, so a short block taken as
/// an answer panicked the session thread.
#[test]
fn a_short_region_in_a_read_is_an_error() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_PEEKV => vec![Step::Send(peek_resp(req.rid, &short(req), false))],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_millis(600));
    let got = cart.read_many(&[(0x100, 8)]);
    assert!(
        got.is_err(),
        "a short block was taken as the answer: {got:?}"
    );
}

/// The same for the cart ROM, where the short block is a cached page: a read that runs to the
/// end of the page used to slice past what the cart sent, and panic.
#[test]
fn a_short_region_in_a_rom_read_is_an_error_not_a_panic() {
    let fake = common::start(Arc::new(|req| match req.msg {
        m64p::MSG_PEEKROM => vec![Step::Send(peek_resp(req.rid, &short(req), true))],
        _ => answer(req, 0),
    }));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    cart.set_reconnect_deadline(Duration::from_millis(600));
    let got = cart.read_rom_many(&[(1000, 24)]);
    assert!(got.is_err(), "{got:?}");
}

/// A cart that answers in full is answered in full, which is what the two above lean on.
#[test]
fn a_whole_reply_is_the_answer() {
    let fake = common::start(Arc::new(|req| answer(req, 0x5A)));
    let mut cart = Multi64::connect(&fake.url, ap64_cart::quiet()).unwrap();
    assert_eq!(cart.read_many(&[(0x100, 8)]).unwrap(), vec![vec![0x5A; 8]]);
    assert_eq!(
        cart.read_rom_many(&[(1000, 24)]).unwrap(),
        vec![vec![0x5A; 24]]
    );
}
