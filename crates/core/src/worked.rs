//! The worked-status producer — the single owner of "which `(callsign, band)` have I
//! worked".
//!
//! Worked-status used to be re-derived in the band scanner, the GUI Contacts map, the
//! waterslide, and the `core::scan` tally, each with a subtly different key (some
//! upper-cased the call, some dropped the band, one dropped the mode) — so changing
//! the dupe rule in one place left the others silently disagreeing. This producer
//! makes it a single owned fact: it subscribes to `logbook/entries` (the logbook's
//! replayed history + live contacts), folds every entry through the canonical
//! [`worked_key`] `(call, band)` rule, and publishes the authoritative [`WorkedSet`]
//! on `logbook/worked` (State, latest-wins). Every consumer subscribes and reads
//! it; none re-derive the dupe rule.
//!
//! Worked-status is a station-level fact derived from the global (unscoped) logbook,
//! so its topic is global too — `logbook/worked`, mirroring `logbook/entries` — not
//! scoped per radio.
//!
//! `worked_key` collapses every digital mode per band (ARRL Field Day / this
//! all-digital app), so a station worked on 20 m FT8 is a dupe on 20 m FT4, while the
//! same call on another band is a new contact.
//!
//! **Origin.** Every entry is `WorkedStatus::WorkedByMe` today — all logged contacts
//! originate locally. The published type already carries the origin dimension
//! (`WorkedByNetwork(StationId)`), so once peer logs merge in over `logbook/entries`
//! (networking; not built here) the producer classifies peer-origin entries by
//! `entry.origin`. This is the multi-op substrate, populated `Mine`-only for now.
//!
//! This producer mirrors the other `core::spawn` producers: a detached tokio task
//! that subscribes to one topic and republishes a derived State topic.

use std::collections::HashMap;

use bus::types::{
    Band, Callsign, ContestProfile, LogEntry, WorkedEntry, WorkedSet, WorkedStatus, worked_key,
};
use bus::{BusError, BusHandle, Topic, TopicSelector};

/// Launch the worked-status producer onto `bus`, publishing `logbook/worked`.
/// `contest` selects the dupe rule applied by [`worked_key`] (both shipping profiles
/// collapse digital modes per band today). Spawns a detached tokio task, so it must
/// be called from within a runtime context (like [`crate::spawn`]).
pub fn spawn(bus: &BusHandle, contest: ContestProfile) {
    tokio::spawn(run(bus.clone(), contest));
}

async fn run(bus: BusHandle, contest: ContestProfile) {
    // Subscribe to the logbook stream. It is `StreamLossless` with a generous replay
    // ring, so the logbook's startup history reaches us even if we subscribe after it
    // replayed — no ordering dependency on `logbook::spawn`.
    let mut sub = match bus.subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("worked: cannot subscribe logbook entries: {e:?}");
            return;
        }
    };

    let topic = Topic::Worked;
    // The canonical worked set, keyed once by `worked_key`. A `HashMap` because
    // `(Callsign, Band)` is `Hash + Eq` (not `Ord`); the published `Vec` order is
    // unspecified — consumers treat it as a set.
    let mut worked: HashMap<(Callsign, Band), WorkedStatus> = HashMap::new();

    tracing::info!("worked: producer ready");
    loop {
        match sub.recv().await {
            Ok(entry) => {
                // Republish only when the set actually changed: the logbook replays
                // its history (and may later gossip peer dupes), so an unchanged
                // re-insert must not churn the State topic.
                if apply(&mut worked, &entry, contest)
                    && let Err(e) = bus.publish(&topic, to_worked_set(&worked))
                {
                    tracing::warn!("worked: publish failed: {e:?}");
                }
            }
            // Lossless stream: a lag only means we missed some entries — the next one
            // still republishes the full set. A closed channel ends the producer.
            Err(BusError::Lagged { .. }) => continue,
            Err(_) => break,
        }
    }
}

/// Fold one logbook entry into the worked map, returning whether the set changed.
/// The single place the dupe rule is applied — [`worked_key`] normalizes the call and
/// collapses the mode per the contest profile.
fn apply(
    worked: &mut HashMap<(Callsign, Band), WorkedStatus>,
    entry: &LogEntry,
    contest: ContestProfile,
) -> bool {
    let key = worked_key(entry, contest);
    // Only `WorkedByMe` today — see the module docs on origin. When peer entries land
    // on `logbook/entries`, classify by `entry.origin` here.
    let status = WorkedStatus::WorkedByMe;
    worked.insert(key, status.clone()) != Some(status)
}

/// Snapshot the worked map into the published [`WorkedSet`].
fn to_worked_set(worked: &HashMap<(Callsign, Band), WorkedStatus>) -> WorkedSet {
    WorkedSet {
        entries: worked
            .iter()
            .map(|((call, band), status)| WorkedEntry {
                call: call.clone(),
                band: *band,
                status: status.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bus::types::{AbsHz, GridSquare, OverAirMode, QsoId, RadioId, StationId, Timestamp};

    /// A logged contact; only `call`, `mode`, and `band` matter to the worked key.
    fn log_entry(call: &str, mode: OverAirMode, band: Band) -> LogEntry {
        LogEntry {
            id: QsoId {
                origin: StationId("station-a".into()),
                seq: 1,
            },
            origin: StationId("station-a".into()),
            radio: Some(RadioId("rig0".into())),
            call: Callsign(call.into()),
            mode,
            band,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000),
            exchange_sent: "3A CO".into(),
            exchange_rcvd: "3A WCF".into(),
            grid: Some(GridSquare("DN70".into())),
            section: None,
        }
    }

    /// Drive the producer's fold over a sequence of entries and return the snapshot a
    /// subscriber would see — the same path `run` publishes.
    fn fold(entries: &[LogEntry], contest: ContestProfile) -> WorkedSet {
        let mut worked = HashMap::new();
        for e in entries {
            apply(&mut worked, e, contest);
        }
        to_worked_set(&worked)
    }

    #[test]
    fn same_call_same_band_different_mode_is_worked() {
        // Field Day collapses digital modes: working W1ABC on 20 m FT8 marks it worked
        // on 20 m regardless of mode — and FT4 on the same band adds no new key.
        let mut worked = HashMap::new();
        let contest = ContestProfile::ArrlFieldDay;
        assert!(apply(
            &mut worked,
            &log_entry("W1ABC", OverAirMode::Ft8, Band::B20m),
            contest,
        ));
        let changed = apply(
            &mut worked,
            &log_entry("W1ABC", OverAirMode::Ft4, Band::B20m),
            contest,
        );
        assert!(!changed, "FT4 on a band already worked on FT8 is a dupe");
        assert!(to_worked_set(&worked).is_worked(&Callsign("W1ABC".into()), Band::B20m));
    }

    #[test]
    fn same_call_different_band_is_not_worked() {
        let set = fold(
            &[log_entry("W1ABC", OverAirMode::Ft8, Band::B20m)],
            ContestProfile::ArrlFieldDay,
        );
        assert!(set.is_worked(&Callsign("W1ABC".into()), Band::B20m));
        // Worked-ness is per band: the same call on 40 m is a new contact.
        assert!(!set.is_worked(&Callsign("W1ABC".into()), Band::B40m));
    }

    #[test]
    fn reflects_logbook_including_replayed_entries() {
        // The logbook replays its history on startup, so the producer sees each entry
        // again. A replay must be idempotent (no change), and the snapshot reflects
        // every logged contact, matched case-insensitively against normalized keys.
        let mut worked = HashMap::new();
        let contest = ContestProfile::Standard;
        let e = log_entry("n0jdc", OverAirMode::Ft8, Band::B40m);
        assert!(apply(&mut worked, &e, contest), "first sighting → new key");
        assert!(!apply(&mut worked, &e, contest), "replay → no change");
        let set = to_worked_set(&worked);
        assert!(set.is_worked(&Callsign("N0JDC".into()), Band::B40m));
        assert_eq!(set.entries.len(), 1);
    }
}
