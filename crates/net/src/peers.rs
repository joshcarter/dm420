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

impl Peers {
    /// Add a beacon target (a manual `DM420_PEERS` entry or an mDNS-resolved
    /// address). Idempotent.
    pub fn add_target(&self, addr: SocketAddr) {
        self.inner.lock().unwrap().targets.insert(addr);
    }

    /// Record a snapshot from `station` at `addr`. Adds `addr` as a target (we
    /// reply to whoever we hear) and updates the station's high-water `seq`.
    /// Returns `true` if `seq` is newer than anything seen from this station
    /// (i.e. the caller should act on it), `false` if it's stale/duplicate.
    pub fn observe(&self, station: &StationId, addr: SocketAddr, seq: u64, now: Instant) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.targets.insert(addr);
        match inner.by_station.get_mut(station) {
            Some(seen) => {
                seen.addr = addr;
                seen.last_seen = now;
                if seq > seen.last_seq {
                    seen.last_seq = seq;
                    true
                } else {
                    false // stale beacon — a reordered/duplicate datagram
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
                true
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
        assert!(peers.observe(&s, addr(9000), 1, t0), "first seq is new");
        assert!(peers.observe(&s, addr(9000), 2, t0), "higher seq is new");
        assert!(!peers.observe(&s, addr(9000), 2, t0), "same seq is stale");
        assert!(!peers.observe(&s, addr(9000), 1, t0), "lower seq is stale");
        assert_eq!(peers.targets(), vec![addr(9000)]);
        assert_eq!(peers.live_count(), 1);
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
