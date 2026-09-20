//! The console's RDRAM, read and written through the M64P cart agent.
//!
//! [`m64p`] is the wire codec, [`transport`] carries it over multi64d's WebSocket, and
//! [`backend`] is what a connector uses: batched reads and writes of any size, retried
//! and reconnected, or a RAM image standing in for a console in tests.
//!
//! [`watch`] is the one thing a backend does that a plain read does not: a slot the game
//! overwrites between polls is sampled on requests already going out, and the changes are
//! queued so the connector is shown each one.
//!
//! Ported from oot-ap-cart's host (the plain path, which has run Paper Mario and
//! Castlevania 64 sessions on hardware).

pub mod backend;
pub mod m64p;
pub mod transport;
pub mod watch;

use std::sync::Arc;

/// Where diagnostic lines go. The app sends them to its log pane; tests drop them.
pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// A [`Log`] that discards everything.
pub fn quiet() -> Log {
    Arc::new(|_| {})
}
