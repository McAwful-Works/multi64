//! A slot the game rewrites faster than a connector can poll it.
//!
//! Some games record "this just happened" in one place that the next event overwrites.
//! Ocarina of Time does: until a scene transition commits its flags, the only sign a
//! check was collected is a 4-byte slot holding the most recent flag-set event, which the
//! Archipelago connector reads once per scan. An emulator reads it every frame and sees
//! them all. A cart round trip is ~67-100 ms, so a poll sees one value and whatever
//! happened between polls is gone before it looks -- which is "I collected a check, but
//! the client only saw it after I left the room".
//!
//! Two parts, both from oot-ap-cart, where they were measured and then run on hardware:
//!
//! - **Sample far more often than the connector reads.** The region rides on requests
//!   already going out ([`backend`](crate::backend)), and on time the session is spending
//!   waiting for the client anyway. Neither costs an exchange.
//! - **Queue what those samples see, and hand it over in order.** A read of the slot gets
//!   the oldest change the connector has not been shown; when the queue is empty it gets
//!   what is in memory now, exactly as before.
//!
//! Handing back something other than live memory is normally the one thing a memory
//! shim must not do. The case for it here is narrow, and worth stating plainly:
//!
//! - every queued value is a change that **really happened**, so a check can arrive late
//!   but never wrongly -- there is no input that makes this invent one;
//! - the connector is given the same *sequence* an emulator observes at 60 Hz, shifted
//!   later by a poll or two: this compensates for sample rate, it does not second-guess
//!   the connector;
//! - `filter` keeps out changes no call site could act on, since replaying one of those
//!   costs a scan and delays one that matters.
//!
//! What it does not fix: a value written and overwritten between two samples was never
//! seen and cannot be replayed. That still falls back to whatever the game commits later,
//! which is the behaviour without any of this. Closing it outright means the cart holding
//! the events rather than the host sampling for them.
//!
//! Nothing here knows which game is running: the address, the length and the filter come
//! from the connector that asked for the watch.

use std::collections::VecDeque;

/// How many unseen changes to hold before dropping the oldest.
///
/// The queue drains one per scan, so it grows only while changes arrive faster than the
/// connector polls. Sixteen is several seconds of backlog; past that the session is so
/// far behind that holding more would replay history rather than catch up.
const QUEUE_CAP: usize = 16;

/// Which changes are worth queueing: byte `at` must be one of `values`.
///
/// A slot usually carries more kinds of event than the connector acts on. Keeping the
/// rest out of the queue is what stops a replay of something unmatched from delaying one
/// that matters. Deliberately coarse -- encoding a connector's full matching rules here
/// would be a second copy of them, and being generous only costs queue slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub at: usize,
    pub values: Vec<u8>,
}

impl Filter {
    fn admits(&self, value: &[u8]) -> bool {
        value.get(self.at).is_some_and(|b| self.values.contains(b))
    }
}

/// What the watch has seen and handed over, for the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchStats {
    /// Times the slot was read, however it was read.
    pub samples: u64,
    /// Changes to a non-zero value.
    pub events: u32,
    /// Of those, the ones the filter admitted and the queue took.
    pub queued: u32,
    /// Queued values handed to the connector.
    pub replayed: u32,
    /// Queued values lost to a full queue.
    pub dropped: u32,
    /// Waiting to be handed over now.
    pub waiting: usize,
}

/// One watched slot: where it is, what counts, and what has been seen but not shown.
#[derive(Debug)]
pub struct Watch {
    addr: u32,
    len: usize,
    filter: Option<Filter>,
    last: Option<Vec<u8>>,
    queue: VecDeque<Vec<u8>>,
    stats: WatchStats,
}

impl Watch {
    pub fn new(addr: u32, len: usize, filter: Option<Filter>) -> Self {
        Self {
            addr,
            len,
            filter,
            last: None,
            queue: VecDeque::new(),
            stats: WatchStats::default(),
        }
    }

    /// The region to read, for whoever is doing the reading.
    pub fn region(&self) -> (u32, usize) {
        (self.addr, self.len)
    }

    /// Record the slot as it was just read.
    pub fn observe(&mut self, bytes: &[u8]) {
        if bytes.len() != self.len {
            return;
        }
        self.stats.samples += 1;
        let was = match self.last.replace(bytes.to_vec()) {
            Some(was) => was,
            // The first sample is a baseline, not a change: what was there before this
            // session started is not an event it should report.
            None => return,
        };
        if was == bytes {
            return;
        }
        // Going to zero is the game clearing the slot, not something happening in it.
        if bytes.iter().all(|&b| b == 0) {
            return;
        }
        self.stats.events += 1;
        if let Some(f) = &self.filter {
            if !f.admits(bytes) {
                return;
            }
        }
        if self.queue.len() == QUEUE_CAP {
            self.queue.pop_front();
            self.stats.dropped += 1;
        }
        self.queue.push_back(bytes.to_vec());
        self.stats.queued += 1;
    }

    /// The oldest change not yet handed over.
    ///
    /// `None` means read the slot as it is now, which is right once the queue has drained.
    pub fn take(&mut self) -> Option<Vec<u8>> {
        let v = self.queue.pop_front()?;
        self.stats.replayed += 1;
        Some(v)
    }

    pub fn stats(&self) -> WatchStats {
        WatchStats {
            waiting: self.queue.len(),
            ..self.stats
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chest(id: u8) -> Vec<u8> {
        vec![0x60, 0x01, 0x00, id]
    }

    fn oot() -> Watch {
        Watch::new(
            0x40_002C,
            4,
            Some(Filter {
                at: 1,
                values: vec![0x00, 0x01, 0x02, 0x05],
            }),
        )
    }

    #[test]
    fn the_first_sample_is_a_baseline_and_not_an_event() {
        let mut w = oot();
        w.observe(&chest(1));
        assert_eq!(w.take(), None);
        assert_eq!(w.stats().events, 0);
    }

    #[test]
    fn a_change_is_queued_and_handed_over_oldest_first() {
        let mut w = oot();
        w.observe(&[0, 0, 0, 0]);
        w.observe(&chest(1));
        w.observe(&chest(2));
        assert_eq!(w.take(), Some(chest(1)));
        assert_eq!(w.take(), Some(chest(2)));
        assert_eq!(w.take(), None);
        let s = w.stats();
        assert_eq!((s.events, s.queued, s.replayed), (2, 2, 2));
    }

    #[test]
    fn the_same_value_read_again_is_not_a_second_event() {
        let mut w = oot();
        w.observe(&[0, 0, 0, 0]);
        w.observe(&chest(1));
        w.observe(&chest(1));
        assert_eq!(w.take(), Some(chest(1)));
        assert_eq!(w.take(), None);
    }

    /// The game clearing the slot would otherwise double every count, and replaying a
    /// cleared slot tells the connector nothing.
    #[test]
    fn the_slot_going_back_to_zero_is_not_an_event() {
        let mut w = oot();
        w.observe(&chest(1));
        w.observe(&[0, 0, 0, 0]);
        assert_eq!(w.take(), None);
        assert_eq!(w.stats().events, 0);
    }

    #[test]
    fn a_change_the_filter_rejects_counts_but_is_not_queued() {
        let mut w = oot();
        w.observe(&[0, 0, 0, 0]);
        w.observe(&[0x0B, 0x03, 0x00, 0x08]); // a type no call site matches
        let s = w.stats();
        assert_eq!((s.events, s.queued), (1, 0));
        assert_eq!(w.take(), None);
    }

    #[test]
    fn without_a_filter_every_change_is_queued() {
        let mut w = Watch::new(0x40_002C, 4, None);
        w.observe(&[0, 0, 0, 0]);
        w.observe(&[0x0B, 0x03, 0x00, 0x08]);
        assert_eq!(w.take(), Some(vec![0x0B, 0x03, 0x00, 0x08]));
    }

    /// Past the cap the session is far enough behind that older events are history; the
    /// game's own flags still commit them later.
    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut w = oot();
        w.observe(&[0, 0, 0, 0]);
        for i in 0..(QUEUE_CAP + 2) {
            w.observe(&chest(i as u8));
        }
        let s = w.stats();
        assert_eq!((s.dropped, s.waiting), (2, QUEUE_CAP));
        assert_eq!(w.take(), Some(chest(2)));
    }

    #[test]
    fn a_read_of_the_wrong_length_is_ignored() {
        let mut w = oot();
        w.observe(&[0, 0, 0, 0]);
        w.observe(&[0x60, 0x01]);
        assert_eq!(w.stats().samples, 1);
        assert_eq!(w.take(), None);
    }
}
