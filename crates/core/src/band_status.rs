//! The band-status producer — the always-on aggregate of who's active on each
//! configured band/mode.
//!
//! The single owner of the read-only **Band Status** panel's data. It subscribes to
//! the enriched decode stream (band + mode + call + worked, owned by [`crate::enrich`]),
//! folds distinct heard stations into a per-`(band, mode)` set with a rolling
//! retention window, and publishes [`BandStatus`] on `band/status` (State). Every
//! operator runs its own copy, merging its local decodes with every peer's gossiped
//! `heard` list, so all operators converge on approximately the same view (small
//! differences from each operator's own retention window are expected and fine).
//!
//! Per-`(band, mode)` it reports, over the window:
//! - **heard** — distinct stations (mine ∪ peers),
//! - **cq** — those seen calling CQ (a subset; local-only, peers carry no CQ flag),
//! - **unworked** — those not worked on the band, recomputed against the live worked
//!   set at publish time (so working a station mid-window updates it).
//!
//! Like the other `core::spawn` producers, this is a detached tokio task that
//! subscribes its inputs and republishes a derived State topic — only when the rows
//! actually change, so the 2 s tick + window decay don't churn the topic.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bus::types::{
    Band, BandStatus, BandStatusRow, Callsign, Decode, DecodeContent, EnrichedDecode, OverAirMode,
    ParsedMessage, RadioId, StationId, StationSnapshot, Timestamp, WorkedSet, now_ms,
};
use bus::{BusError, BusHandle, Topic, TopicKind, TopicSelector};

/// How often the producer prunes the window and republishes (if changed). Band
/// status is situational, not real-time, so a couple seconds keeps it fresh without
/// churning the State topic.
const TICK: Duration = Duration::from_secs(2);

/// Launch the band-status producer onto `bus`, publishing `band/status`. `stops` is
/// the configured `(band, mode)` set it tracks and `window` the heard-retention
/// span; `me` filters our own beacon echo out of the peer stream. Spawns a detached
/// tokio task, so it must be called from within a runtime context (like [`crate::spawn`]).
pub fn spawn(
    bus: &BusHandle,
    radio: RadioId,
    me: StationId,
    stops: Vec<(Band, OverAirMode)>,
    window: Duration,
) {
    tokio::spawn(run(bus.clone(), radio, me, stops, window));
}

async fn run(
    bus: BusHandle,
    radio: RadioId,
    me: StationId,
    stops: Vec<(Band, OverAirMode)>,
    window: Duration,
) {
    macro_rules! sub {
        ($ty:ty, $sel:expr, $what:literal) => {
            match bus.subscribe::<$ty>($sel) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("band_status: cannot subscribe {}: {e:?}", $what);
                    return;
                }
            }
        };
    }

    let mut enriched = sub!(
        EnrichedDecode,
        TopicSelector::Exact(Topic::DecodesEnriched(radio.clone())),
        "enriched decodes"
    );
    let mut worked_sub = sub!(WorkedSet, TopicSelector::Exact(Topic::Worked), "worked-status");
    let mut peers = sub!(
        StationSnapshot,
        TopicSelector::Wildcard(TopicKind::StationSnapshot),
        "peer snapshots"
    );

    let allowed: HashSet<(Band, OverAirMode)> = stops.iter().copied().collect();
    let mut agg = Agg::default();
    let mut worked = WorkedSet::default();
    let mut last: Option<Vec<BandStatusRow>> = None;
    let mut tick = tokio::time::interval(TICK);

    tracing::info!(stops = stops.len(), "band_status: producer ready");
    loop {
        tokio::select! {
            r = enriched.recv() => match r {
                Ok(ed) => agg.note_mine(&allowed, &ed),
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = worked_sub.recv() => match r {
                Ok(w) => worked = w,
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = peers.recv() => match r {
                // Skip our own beacon echo: our heard stations already arrive as local
                // enriched decodes. (Inert until the beacon populates `heard`.)
                Ok(snap) => {
                    if snap.station != me {
                        agg.note_peer(&allowed, &snap);
                    }
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            _ = tick.tick() => {
                agg.prune(window);
                let rows = agg.rows(&stops, &worked);
                // Anti-churn: republish only when the rows changed (the timestamp is
                // not part of the comparison).
                if last.as_ref() != Some(&rows) {
                    last = Some(rows.clone());
                    let _ = bus.publish(&Topic::BandStatus, BandStatus { rows, t: Timestamp(now_ms()) });
                }
            }
        }
    }
}

/// One heard station's recency + whether it's been seen calling CQ.
struct Seen {
    last_ms: i64,
    cq: bool,
}

/// The rolling per-`(band, mode)` set of distinct heard stations. Mine and peers
/// merge into one set keyed by normalized callsign, so a station heard by both
/// counts once. (Origin — mine vs peer — isn't surfaced in [`BandStatusRow`] yet;
/// add it here when the panel shows a peer split, or the beacon sources its `heard`
/// list from this set.)
#[derive(Default)]
struct Agg {
    seen: HashMap<(Band, OverAirMode), HashMap<Callsign, Seen>>,
}

impl Agg {
    /// Credit a locally-decoded station to its `(band, mode)` bucket.
    fn note_mine(&mut self, allowed: &HashSet<(Band, OverAirMode)>, ed: &EnrichedDecode) {
        let key = (ed.band, ed.decode.mode);
        if !allowed.contains(&key) {
            return;
        }
        let Some(call) = &ed.callsign else {
            return;
        };
        let cq = is_cq(&ed.decode);
        let e = self
            .seen
            .entry(key)
            .or_default()
            .entry(call.normalized())
            .or_insert(Seen { last_ms: 0, cq: false });
        e.last_ms = now_ms();
        e.cq |= cq;
    }

    /// Merge a peer's heard list. Peers carry no CQ flag, so they only contribute to
    /// the heard set, not the CQ count.
    fn note_peer(&mut self, allowed: &HashSet<(Band, OverAirMode)>, snap: &StationSnapshot) {
        for h in &snap.heard {
            let key = (h.band, h.mode);
            if !allowed.contains(&key) {
                continue;
            }
            let e = self
                .seen
                .entry(key)
                .or_default()
                .entry(h.call.normalized())
                .or_insert(Seen { last_ms: 0, cq: false });
            // Re-stamp on the receiver's clock, so peer recency is immune to operator
            // clock skew (matching the `HeardStation` contract).
            e.last_ms = now_ms();
        }
    }

    /// Drop stations not heard within `window`.
    fn prune(&mut self, window: Duration) {
        let cutoff = now_ms() - window.as_millis() as i64;
        for bucket in self.seen.values_mut() {
            bucket.retain(|_, s| s.last_ms >= cutoff);
        }
        self.seen.retain(|_, b| !b.is_empty());
    }

    /// One [`BandStatusRow`] per configured stop (zero-filled when nothing's heard),
    /// in `stops` order so the panel sees a stable set. `unworked` is recomputed
    /// against the live worked set rather than stored, so working a station updates it.
    fn rows(&self, stops: &[(Band, OverAirMode)], worked: &WorkedSet) -> Vec<BandStatusRow> {
        stops
            .iter()
            .map(|&(band, mode)| {
                let (heard, cq, unworked) = match self.seen.get(&(band, mode)) {
                    Some(b) => (
                        b.len() as u32,
                        b.values().filter(|s| s.cq).count() as u32,
                        b.keys().filter(|c| !worked.is_worked(c, band)).count() as u32,
                    ),
                    None => (0, 0, 0),
                };
                BandStatusRow {
                    band,
                    mode,
                    heard,
                    cq,
                    unworked,
                }
            })
            .collect()
    }
}

/// Whether a decode is a CQ call.
fn is_cq(d: &Decode) -> bool {
    matches!(
        &d.content,
        DecodeContent::Slotted {
            message: ParsedMessage::Cq { .. },
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bus::types::{
        GridSquare, HeardStation, OffsetHz, SignalSource, Signoff, SlotId, WorkedEntry,
        WorkedStatus,
    };

    const FD: Band = Band::B20m;

    fn stops() -> Vec<(Band, OverAirMode)> {
        vec![
            (Band::B20m, OverAirMode::Ft8),
            (Band::B40m, OverAirMode::Ft8),
        ]
    }

    fn allowed() -> HashSet<(Band, OverAirMode)> {
        stops().into_iter().collect()
    }

    fn enriched(band: Band, mode: OverAirMode, call: &str, cq: bool) -> EnrichedDecode {
        let message = if cq {
            ParsedMessage::Cq {
                caller: Callsign(call.into()),
                contest: None,
                grid: None,
            }
        } else {
            ParsedMessage::Signoff {
                to: Callsign("CQER".into()),
                from: Callsign(call.into()),
                kind: Signoff::Rr73,
            }
        };
        EnrichedDecode {
            decode: Decode {
                radio: RadioId("rig0".into()),
                mode,
                t: Timestamp(0),
                offset: OffsetHz(0.0),
                snr_db: None,
                source: SignalSource::Received,
                content: DecodeContent::Slotted {
                    slot: SlotId(0),
                    dt: 0.0,
                    message,
                    raw: String::new(),
                },
            },
            callsign: Some(Callsign(call.into())),
            grid: None,
            worked: WorkedStatus::New,
            band,
            dial: None,
        }
    }

    fn row(rows: &[BandStatusRow], band: Band, mode: OverAirMode) -> BandStatusRow {
        *rows
            .iter()
            .find(|r| r.band == band && r.mode == mode)
            .expect("row present")
    }

    #[test]
    fn counts_distinct_stations_with_cq_as_a_subset() {
        let mut agg = Agg::default();
        let a = allowed();
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "W1ABC", true)); // a CQ
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "W1ABC", false)); // same call again
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "K2DEF", false)); // a second call
        let rows = agg.rows(&stops(), &WorkedSet::default());
        let r = row(&rows, FD, OverAirMode::Ft8);
        assert_eq!(r.heard, 2, "two distinct calls");
        assert_eq!(r.cq, 1, "one of them called CQ; cq ⊆ heard");
        // An untouched stop is still present, zeroed.
        assert_eq!(row(&rows, Band::B40m, OverAirMode::Ft8).heard, 0);
    }

    #[test]
    fn merges_mine_and_peer_into_one_distinct_station() {
        let mut agg = Agg::default();
        let a = allowed();
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "W1ABC", false));
        let snap = StationSnapshot {
            station: StationId("peer".into()),
            seq: 1,
            working: None,
            band_activity: vec![],
            heard: vec![HeardStation {
                call: Callsign("w1abc".into()), // same call, lowercase, from a peer
                grid: Some(GridSquare("FN42".into())),
                band: FD,
                mode: OverAirMode::Ft8,
                snr: -3,
                last_heard: Timestamp(0),
            }],
        };
        agg.note_peer(&a, &snap);
        let rows = agg.rows(&stops(), &WorkedSet::default());
        assert_eq!(
            row(&rows, FD, OverAirMode::Ft8).heard,
            1,
            "mine ∪ peer of the same call is one station"
        );
    }

    #[test]
    fn ignores_stops_outside_the_configured_set() {
        let mut agg = Agg::default();
        let a = allowed();
        // 15 m isn't configured, so nothing is recorded for it.
        agg.note_mine(&a, &enriched(Band::B15m, OverAirMode::Ft8, "W1ABC", false));
        assert!(agg.seen.is_empty(), "an unconfigured stop is dropped");
    }

    #[test]
    fn unworked_excludes_worked_on_the_band_and_recomputes_live() {
        let mut agg = Agg::default();
        let a = allowed();
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "W1ABC", false));
        agg.note_mine(&a, &enriched(FD, OverAirMode::Ft8, "K2DEF", false));
        // Nothing worked yet → both unworked.
        let rows = agg.rows(&stops(), &WorkedSet::default());
        assert_eq!(row(&rows, FD, OverAirMode::Ft8).unworked, 2);
        // Work one of them on the band → unworked drops to 1 (recomputed, not stored).
        let worked = WorkedSet {
            entries: vec![WorkedEntry {
                call: Callsign("W1ABC".into()),
                band: FD,
                status: WorkedStatus::WorkedByMe,
            }],
        };
        let rows = agg.rows(&stops(), &worked);
        assert_eq!(row(&rows, FD, OverAirMode::Ft8).unworked, 1);
    }

    #[test]
    fn prunes_entries_past_the_window() {
        let mut agg = Agg::default();
        let now = now_ms();
        let bucket = agg.seen.entry((FD, OverAirMode::Ft8)).or_default();
        bucket.insert(Callsign("FRESH".into()), Seen { last_ms: now, cq: false });
        bucket.insert(
            Callsign("STALE".into()),
            Seen {
                last_ms: now - 600_000, // 10 minutes ago
                cq: false,
            },
        );
        agg.prune(Duration::from_secs(300)); // 5-minute window
        let rows = agg.rows(&stops(), &WorkedSet::default());
        assert_eq!(
            row(&rows, FD, OverAirMode::Ft8).heard,
            1,
            "the 10-minute-old station aged out"
        );
    }
}
