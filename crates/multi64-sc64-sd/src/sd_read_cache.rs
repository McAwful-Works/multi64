//! A read cache for SD sectors, for the life of one PC-side SD session (#196).
//!
//! Every exFAT operation resolves its path through the root directory, and a single command reads
//! that root several times: a nested mkdir once per path segment, a folder delete at least three
//! times. On a card whose root spans 18 clusters that is 576 KiB and about two seconds a time over
//! the SC64 link, so repeat reads — not the link, and not the shape of the requests — are the cost.
//!
//! **Why this is safe, and where it stops being safe.** While the PC holds the SD session, the card
//! is locked away from the console, so the only writer is this process, and every SD write passes
//! through one method, `Sc64Link::write_sd_sectors`. That method goes through
//! [`SdReadCache::write_through`], which drops every cached range the write overlaps **before**
//! writing, so a write that fails part-way leaves nothing stale either. Outside a session the card
//! can change under us — the console may write a save — so the cache is cleared on `SD_CARD_OP`
//! init and deinit and never outlives a session. Xfer64 opens a session per command, so in practice
//! nothing survives from one click to the next.
//!
//! A cached request is kept whole. Entries never overlap: inserting one removes any it touches. A
//! later read is served only when it lies **entirely** inside one entry; anything else goes to the
//! card. Memory is bounded by [`SdReadCache::BUDGET_BYTES`], oldest entries first.

use std::collections::{BTreeMap, VecDeque};
use std::io;

const SECTOR_BYTES: u64 = 512;

#[derive(Default)]
pub(crate) struct SdReadCache {
    /// Cached requests by first LBA. Never overlapping.
    entries: BTreeMap<u64, Vec<u8>>,
    /// First LBAs in insertion order, for eviction.
    order: VecDeque<u64>,
    bytes: usize,
    #[cfg(test)]
    pub(crate) fetches: u32,
}

impl SdReadCache {
    /// Enough for any directory an operation plausibly walks, plus a volume's allocation bitmap,
    /// and small enough not to matter next to a file transfer's own buffers.
    pub(crate) const BUDGET_BYTES: usize = 8 * 1024 * 1024;

    /// Fill `buf` with the sectors from `lba`, from the cache when one entry covers the whole range,
    /// otherwise through `fetch`, whose result is then cached. A failed fetch caches nothing.
    pub(crate) fn read_through(
        &mut self,
        lba: u64,
        buf: &mut [u8],
        fetch: impl FnOnce(u64, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if buf.is_empty() {
            return fetch(lba, buf);
        }
        if let Some(bytes) = self.covering(lba, buf.len()) {
            buf.copy_from_slice(bytes);
            return Ok(());
        }
        #[cfg(test)]
        {
            self.fetches += 1;
        }
        fetch(lba, buf)?;
        self.insert(lba, buf);
        Ok(())
    }

    /// Write through `store`, having first dropped every cached range the write overlaps.
    ///
    /// Invalidation comes first on purpose. If `store` fails part-way, some of those sectors may
    /// already hold the new bytes; a cache entry from before the write would then disagree with the
    /// card.
    pub(crate) fn write_through(
        &mut self,
        lba: u64,
        buf: &[u8],
        store: impl FnOnce(u64, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.invalidate(lba, buf.len());
        store(lba, buf)
    }

    /// Forget everything: the card may change once the session is not ours.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    fn end(lba: u64, len: usize) -> u64 {
        lba + (len as u64).div_ceil(SECTOR_BYTES)
    }

    /// The bytes for `[lba, lba + len)` if one entry covers all of it.
    fn covering(&self, lba: u64, len: usize) -> Option<&[u8]> {
        let (&start, bytes) = self.entries.range(..=lba).next_back()?;
        let from = ((lba - start) * SECTOR_BYTES) as usize;
        bytes.get(from..from + len)
    }

    /// Remove every entry that shares a sector with `[lba, lba + len)`.
    fn invalidate(&mut self, lba: u64, len: usize) {
        let end = Self::end(lba, len);
        let doomed: Vec<u64> = self
            .entries
            .range(..end)
            .filter(|(&start, bytes)| Self::end(start, bytes.len()) > lba)
            .map(|(&start, _)| start)
            .collect();
        for start in doomed {
            self.remove(start);
        }
    }

    fn remove(&mut self, start: u64) {
        if let Some(bytes) = self.entries.remove(&start) {
            self.bytes -= bytes.len();
            self.order.retain(|&s| s != start);
        }
    }

    fn insert(&mut self, lba: u64, bytes: &[u8]) {
        if bytes.len() > Self::BUDGET_BYTES {
            return;
        }
        self.invalidate(lba, bytes.len());
        while self.bytes + bytes.len() > Self::BUDGET_BYTES {
            match self.order.front().copied() {
                Some(oldest) => self.remove(oldest),
                None => break,
            }
        }
        self.bytes += bytes.len();
        self.entries.insert(lba, bytes.to_vec());
        self.order.push_back(lba);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card of `sectors` sectors, each filled with its own LBA's low byte, counting fetches.
    struct Card {
        data: Vec<u8>,
        fetches: u32,
    }

    impl Card {
        fn new(sectors: u64) -> Self {
            let mut data = vec![0u8; (sectors * SECTOR_BYTES) as usize];
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i as u64 / SECTOR_BYTES) as u8;
            }
            Self { data, fetches: 0 }
        }

        fn read(&mut self, cache: &mut SdReadCache, lba: u64, sectors: usize) -> Vec<u8> {
            let mut buf = vec![0u8; sectors * SECTOR_BYTES as usize];
            let data = &self.data;
            let fetches = &mut self.fetches;
            cache
                .read_through(lba, &mut buf, |lba, out| {
                    *fetches += 1;
                    let at = (lba * SECTOR_BYTES) as usize;
                    out.copy_from_slice(&data[at..at + out.len()]);
                    Ok(())
                })
                .unwrap();
            buf
        }

        fn write(&mut self, cache: &mut SdReadCache, lba: u64, bytes: &[u8]) {
            let data = &mut self.data;
            cache
                .write_through(lba, bytes, |lba, b| {
                    let at = (lba * SECTOR_BYTES) as usize;
                    data[at..at + b.len()].copy_from_slice(b);
                    Ok(())
                })
                .unwrap();
        }

        fn truth(&self, lba: u64, sectors: usize) -> Vec<u8> {
            let at = (lba * SECTOR_BYTES) as usize;
            self.data[at..at + sectors * SECTOR_BYTES as usize].to_vec()
        }
    }

    const S: usize = SECTOR_BYTES as usize;

    #[test]
    fn a_repeated_read_is_served_from_the_cache() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        let first = card.read(&mut cache, 10, 16);
        let again = card.read(&mut cache, 10, 16);
        assert_eq!(first, again);
        assert_eq!(card.fetches, 1);
    }

    #[test]
    fn a_read_inside_a_cached_range_is_served_but_one_straddling_its_end_is_not() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        card.read(&mut cache, 10, 16);
        assert_eq!(card.read(&mut cache, 12, 1), card.truth(12, 1));
        assert_eq!(card.read(&mut cache, 25, 1), card.truth(25, 1));
        assert_eq!(card.fetches, 1, "both lie inside 10..26");
        assert_eq!(card.read(&mut cache, 24, 4), card.truth(24, 4));
        assert_eq!(card.fetches, 2, "24..28 runs past the entry");
    }

    /// The property the whole cache rests on: after a write, no read returns the old bytes,
    /// wherever the write lands relative to what is cached.
    #[test]
    fn a_write_anywhere_in_a_cached_range_is_never_read_back_stale() {
        for (at, sectors) in [
            (10, 16),
            (10, 1),
            (25, 1),
            (18, 2),
            (8, 3),
            (25, 4),
            (0, 64),
        ] {
            let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
            card.read(&mut cache, 10, 16);
            card.write(&mut cache, at, &vec![0xEE; sectors * S]);
            assert_eq!(
                card.read(&mut cache, 10, 16),
                card.truth(10, 16),
                "stale after a write of {sectors} sector(s) at {at}"
            );
        }
    }

    #[test]
    fn a_write_that_misses_every_entry_evicts_nothing() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        card.read(&mut cache, 10, 16);
        card.write(&mut cache, 26, &[0xEE; S]);
        card.write(&mut cache, 9, &[0xEE; S]);
        card.read(&mut cache, 10, 16);
        assert_eq!(card.fetches, 1, "26 and 9 are both just outside 10..26");
    }

    #[test]
    fn one_write_invalidates_every_entry_it_touches() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        card.read(&mut cache, 0, 4);
        card.read(&mut cache, 4, 4);
        card.read(&mut cache, 8, 4);
        card.write(&mut cache, 3, &vec![0xEE; 6 * S]);
        assert_eq!(card.read(&mut cache, 0, 4), card.truth(0, 4));
        assert_eq!(card.read(&mut cache, 4, 4), card.truth(4, 4));
        assert_eq!(card.read(&mut cache, 8, 4), card.truth(8, 4));
        assert_eq!(
            card.fetches, 6,
            "all three were touched and had to be read again"
        );
    }

    #[test]
    fn a_failed_write_still_invalidates() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        card.read(&mut cache, 10, 4);
        let data = &mut card.data;
        let err = cache.write_through(10, &[0xEE; S], |lba, b| {
            // The card took the bytes, then the acknowledgement was lost.
            let at = (lba * SECTOR_BYTES) as usize;
            data[at..at + b.len()].copy_from_slice(b);
            Err(io::Error::other("lost CMP"))
        });
        assert!(err.is_err());
        assert_eq!(card.read(&mut cache, 10, 4), card.truth(10, 4));
    }

    #[test]
    fn a_failed_read_is_not_cached() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        let mut buf = vec![0u8; S];
        assert!(cache
            .read_through(5, &mut buf, |_, _| Err(io::Error::other("timeout")))
            .is_err());
        assert_eq!(card.read(&mut cache, 5, 1), card.truth(5, 1));
        assert_eq!(card.fetches, 1);
    }

    #[test]
    fn clear_forgets_everything() {
        let (mut card, mut cache) = (Card::new(64), SdReadCache::default());
        card.read(&mut cache, 10, 4);
        cache.clear();
        card.read(&mut cache, 10, 4);
        assert_eq!(card.fetches, 2);
    }

    #[test]
    fn memory_stays_within_budget_by_dropping_the_oldest() {
        let per = 128 * 1024;
        let fits = SdReadCache::BUDGET_BYTES / per;
        let sectors = per / S;
        let mut card = Card::new(((fits + 2) * sectors) as u64);
        let mut cache = SdReadCache::default();
        for i in 0..=fits {
            card.read(&mut cache, (i * sectors) as u64, sectors);
        }
        assert!(cache.bytes <= SdReadCache::BUDGET_BYTES);
        let before = card.fetches;
        card.read(&mut cache, (fits * sectors) as u64, sectors);
        assert_eq!(card.fetches, before, "the newest entry is kept");
        card.read(&mut cache, 0, sectors);
        assert_eq!(card.fetches, before + 1, "the oldest was evicted");
    }
}
