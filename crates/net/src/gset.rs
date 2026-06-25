//! The log **G-set** — the grow-only union of every [`LogEntry`] this instance
//! holds, keyed by its [`QsoId`] (`{origin, seq}`).
//!
//! Two background tasks touch it, so it's an `Arc<Mutex<…>>` handle (cloneable,
//! like [`crate::peers::Peers`]):
//! - the **outbound** log loop folds in entries seen on `logbook/entries` and
//!   proactively pushes the ones we authored (`origin == me`);
//! - [`crate::recv_loop`] folds in entries arriving from peers (`LogPush` /
//!   `LogReply`) and re-publishes the *new* ones onto `logbook/entries`.
//!
//! It stores **full bodies**, not just ids, because the new-peer catch-up
//! ([`Gset::mine`]) must re-send the entries themselves. Merge is set union: a
//! `QsoId` already held is dropped, which is what makes both the inbound merge and
//! the startup replay idempotent. See `docs/networking.md` "Merge semantics".

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex};

use types::{LogEntry, QsoId, StationId};

/// Shared, cloneable handle to the log G-set.
#[derive(Clone, Default)]
pub struct Gset {
    inner: Arc<Mutex<HashMap<QsoId, LogEntry>>>,
}

impl Gset {
    /// Merge one entry into the set. Returns `true` if it was **newly** added
    /// (the caller should act on it — push or re-publish), `false` if its `QsoId`
    /// was already held (idempotent no-op).
    pub fn insert(&self, entry: LogEntry) -> bool {
        match self.inner.lock().unwrap().entry(entry.id.clone()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(entry);
                true
            }
        }
    }

    /// Every entry we **authored** (`origin == me`). Snapshotted into an owned
    /// `Vec` so the lock is released before the caller does any I/O with it — this
    /// feeds the one-shot bulk catch-up to a newly-seen peer, which is `origin ==
    /// me` only (the same asymmetry as the proactive push guard).
    pub fn mine(&self, me: &StationId) -> Vec<LogEntry> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|e| &e.id.origin == me)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{AbsHz, Band, Callsign, OverAirMode, Timestamp};

    fn entry(origin: &str, seq: u64) -> LogEntry {
        LogEntry {
            id: QsoId {
                origin: StationId(origin.into()),
                seq,
            },
            radio: None,
            call: Callsign(format!("K{seq}ABC")),
            mode: OverAirMode::Ft8,
            band: Band::B20m,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000 + seq as i64),
            exchange_sent: "-10".into(),
            exchange_rcvd: "-12".into(),
            grid: None,
            section: None,
        }
    }

    #[test]
    fn insert_is_idempotent_by_qsoid() {
        let g = Gset::default();
        assert!(g.insert(entry("me", 1)), "first insert is new");
        assert!(!g.insert(entry("me", 1)), "same QsoId is a no-op");
        assert!(g.insert(entry("me", 2)), "different seq is new");
        // Only the two distinct ids survive the dedup.
        assert_eq!(g.mine(&StationId("me".into())).len(), 2);
    }

    #[test]
    fn mine_filters_by_origin() {
        let g = Gset::default();
        g.insert(entry("me", 1));
        g.insert(entry("me", 2));
        g.insert(entry("peer", 5));
        let me = StationId("me".into());
        let mut mine: Vec<u64> = g.mine(&me).iter().map(|e| e.id.seq).collect();
        mine.sort_unstable();
        assert_eq!(mine, vec![1, 2], "only my own entries, not the peer's");
    }
}
