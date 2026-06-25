//! The peer table — where to send beacons, and which stations we've heard.
//!
//! Two pieces, deliberately separate:
//! - **targets**: the set of socket addresses we beacon to (manual peers + every
//!   address mDNS resolves + every address we've received a frame from). A
//!   `HashSet`, so duplicates collapse.
//! - **by_station**: per-`StationId` bookkeeping (last `seq`, last-seen instant)
//!   for snapshot dedup and, later, time-to-live expiry.
//!
//! We learn a peer's `StationId` only once a frame arrives, but we can send to a
//! manual/discovered address before that — hence the split.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use types::StationId;

/// Shared, cloneable handle to the peer table.
#[derive(Clone, Default)]
pub struct Peers {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    targets: HashSet<SocketAddr>,
    by_station: HashMap<StationId, Seen>,
}

struct Seen {
    addr: SocketAddr,
    last_seq: u64,
    last_seen: Instant,
}

/// What [`Peers::observe`] reports about an incoming snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    /// `seq` is newer than anything seen from this station — the caller should act
    /// on this snapshot (republish it). `false` for a stale/duplicate beacon.
    pub fresh: bool,
    /// This is the **first** snapshot from this station this session (or the first
    /// since it aged out of the live set). Triggers the one-shot bulk-log catch-up
    /// push so a freshly-discovered peer gets our backlog without waiting for the
    /// anti-entropy loop. Always implies `fresh`.
    pub new_station: bool,
}

impl Peers {
    /// Add a beacon target (a manual `DM420_PEERS` entry or an mDNS-resolved
    /// address). Idempotent.
    pub fn add_target(&self, addr: SocketAddr) {
        self.inner.lock().unwrap().targets.insert(addr);
    }

    /// Record a snapshot from `station` at `addr`. Adds `addr` as a target (we
    /// reply to whoever we hear) and updates the station's high-water `seq`.
    /// See [`Observed`] for the two booleans it reports — `fresh` (act on this
    /// snapshot) and `new_station` (first contact this session → send catch-up).
    pub fn observe(
        &self,
        station: &StationId,
        addr: SocketAddr,
        seq: u64,
        now: Instant,
    ) -> Observed {
        let mut inner = self.inner.lock().unwrap();
        inner.targets.insert(addr);
        match inner.by_station.get_mut(station) {
            Some(seen) => {
                seen.addr = addr;
                seen.last_seen = now;
                let fresh = seq > seen.last_seq;
                if fresh {
                    seen.last_seq = seq;
                }
                // A known station — never new, even on a stale/duplicate beacon.
                Observed {
                    fresh,
                    new_station: false,
                }
            }
            None => {
                inner.by_station.insert(
                    station.clone(),
                    Seen {
                        addr,
                        last_seq: seq,
                        last_seen: now,
                    },
                );
                // First time we've heard this station (or the first since it aged
                // out via `expire`): always fresh, and flagged for the one-shot
                // bulk-log catch-up.
                Observed {
                    fresh: true,
                    new_station: true,
                }
            }
        }
    }

    /// Every address we currently beacon to.
    pub fn targets(&self) -> Vec<SocketAddr> {
        self.inner.lock().unwrap().targets.iter().copied().collect()
    }

    /// Drop stations not heard within `ttl` from the live set. Their beacon
    /// targets are *kept* (a quiet peer is still worth pinging); only the
    /// liveness/`seq` record ages out. Returns the dropped station ids.
    pub fn expire(&self, ttl: Duration, now: Instant) -> Vec<StationId> {
        let mut inner = self.inner.lock().unwrap();
        let dropped: Vec<StationId> = inner
            .by_station
            .iter()
            .filter(|(_, s)| now.duration_since(s.last_seen) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dropped {
            inner.by_station.remove(id);
        }
        dropped
    }

    /// Number of live (recently-heard) peer stations.
    pub fn live_count(&self) -> usize {
        self.inner.lock().unwrap().by_station.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    #[test]
    fn observe_dedups_by_seq() {
        let peers = Peers::default();
        let s = StationId("w4ll".into());
        let t0 = Instant::now();
        let first = peers.observe(&s, addr(9000), 1, t0);
        assert!(first.fresh, "first seq is new");
        assert!(first.new_station, "first contact flags new_station");
        assert!(
            peers.observe(&s, addr(9000), 2, t0).fresh,
            "higher seq is new"
        );
        let dup = peers.observe(&s, addr(9000), 2, t0);
        assert!(!dup.fresh, "same seq is stale");
        assert!(!dup.new_station, "a known station is never new again");
        assert!(
            !peers.observe(&s, addr(9000), 1, t0).fresh,
            "lower seq is stale"
        );
        assert_eq!(peers.targets(), vec![addr(9000)]);
        assert_eq!(peers.live_count(), 1);
    }

    #[test]
    fn observe_flags_new_station_again_after_expiry() {
        let peers = Peers::default();
        let s = StationId("w4ll".into());
        let t0 = Instant::now();
        assert!(peers.observe(&s, addr(9000), 1, t0).new_station);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            peers.expire(Duration::from_secs(30), later),
            vec![s.clone()]
        );
        // After aging out, the next snapshot is a fresh "first contact" again, so
        // the peer re-receives our catch-up.
        assert!(
            peers.observe(&s, addr(9000), 5, later).new_station,
            "re-discovered after expiry counts as new"
        );
    }

    #[test]
    fn expire_drops_quiet_stations_keeps_targets() {
        let peers = Peers::default();
        let s = StationId("w4ll".into());
        let t0 = Instant::now();
        peers.observe(&s, addr(9000), 1, t0);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(peers.expire(Duration::from_secs(30), later), vec![s]);
        assert_eq!(peers.live_count(), 0);
        assert_eq!(peers.targets(), vec![addr(9000)], "target survives expiry");
    }
}
