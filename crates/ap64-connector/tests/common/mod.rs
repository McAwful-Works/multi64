//! What the tests that put the connector on a real socket share: getting a port that
//! [`serve`](ap64_connector::server::serve) can actually bind.

use std::net::TcpListener;

/// How many ports an attempt is given before the test gives up on the machine.
const TRIES: u32 = 32;

/// A port nothing was listening on at the moment it was picked.
///
/// The only way to ask the OS for a free port is to bind one and let it go, so the
/// answer is already stale by the time it is returned.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Whether `serve` gave up because every port it was handed was in use.
///
/// Matches the message `serve` builds for an exhausted port list, not the OS's wording
/// for one refused bind.
pub fn port_was_taken(err: &str) -> bool {
    err.contains("every port in")
}

/// Run `attempt` on picked ports until it gets one that was still free when `serve`
/// bound it, and panic if that never happens.
///
/// A port can only be picked by binding it and dropping the listener, so there is
/// always a window between the pick and `serve`'s own bind. In a `cargo test
/// --workspace` run other test binaries are picking ports out of the same ephemeral
/// range at the same time, and one of them can take the port inside that window: an
/// observed, if rare, failure. That is this harness losing a race rather than anything
/// about the code under test, so the attempt is run again on another port.
///
/// `attempt` returns `Err` only for that bind failure, and only after leaving nothing
/// of itself running; everything it means to check belongs inside it, and it is run
/// from the start each time, since a previous attempt may have left state behind.
pub fn on_a_free_port(mut attempt: impl FnMut(u16) -> Result<(), String>) {
    for _ in 0..TRIES {
        match attempt(free_port()) {
            Ok(()) => return,
            Err(e) => assert!(
                port_was_taken(&e),
                "serve failed for some reason other than the port being taken: {e}"
            ),
        }
    }
    panic!("{TRIES} ports in a row were taken between the pick and serve's bind, or serve is reporting a port list it has not worked through");
}
