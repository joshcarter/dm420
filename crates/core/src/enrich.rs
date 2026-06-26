//! The decode enricher — the single owner of a decode's RF **band** and
//! **worked-status**.
//!
//! The raw [`Decode`] is audio-domain: its `offset` is a tone *within* the receiver
//! passband (the decoder never sees the dial), so "what band was this heard on" and
//! "have I worked this station" are *derived* facts. This producer derives them once
//! and republishes [`EnrichedDecode`] on `radio/{id}/decodes_enriched`, so consumers
//! (the band-status aggregate today; the map / waterslide / scanner tally as they
//! migrate) read one owned fact instead of each re-deriving band-from-VFO and the
//! dupe rule. It is purely additive — nothing changes on the raw `decodes` stream.
//!
//! ## Band attribution (the subtle part)
//!
//! A decode lands a slot-or-two *after* its audio (the decoder runs at slot end), so
//! by the time it arrives the dial may have moved — during a band-scan sweep it
//! usually has. Reading "the current dial" would mis-attribute every post-hop decode
//! (exactly the bug `core::scan` already avoids). Instead we keep a per-slot band
//! timeline: on every clock / rig / scanner update we stamp
//! `slot_band[current_slot] = resolved_band`, where the resolved band is the
//! scanner's *commanded* band while sweeping (authoritative — published right at the
//! hop) or [`Band::from_hz`] of the dial otherwise. The same per-slot stamping also
//! records the *actual* dial (`slot_dial[current_slot] = resolved_dial`, the live
//! VFO — deliberately not the calling frequency, since operation may sit off it), so
//! a consumer can reconstruct each heard station's absolute frequency correctly even
//! across a hop. Because a decode for slot `S`
//! can't arrive until `S` has ended, `slot_band[S]` has had the whole slot to settle
//! on the right band — so even the first slot after a hop is attributed correctly,
//! despite the CAT echo landing a beat late. (Mirrors `core::scan`'s private
//! `slot_band`, generalized to always-on; that copy converges here in a later step.)
//!
//! Like the other `core::spawn` producers, this is a detached tokio task that
//! subscribes its inputs and republishes a derived topic.

use std::collections::HashMap;

use bus::types::{
    AbsHz, Band, Callsign, ClockStatus, Decode, DecodeContent, EnrichedDecode, ExchangePayload,
    GridSquare, ParsedMessage, RadioId, RigState, ScannerState, SlotId, WorkedSet, WorkedStatus,
};
use bus::{BusError, BusHandle, Topic, TopicSelector};

/// How many recent slot→band entries to keep. Decodes lag their slot by only a slot
/// or two, so a small window is plenty (mirrors `core::scan`'s `SLOT_BAND_KEEP`).
const SLOT_BAND_KEEP: u64 = 16;

/// Launch the decode enricher onto `bus`, publishing `radio/{id}/decodes_enriched`.
/// Spawns a detached tokio task, so it must be called from within a runtime context
/// (like [`crate::spawn`]).
pub fn spawn(bus: &BusHandle, radio: RadioId) {
    tokio::spawn(run(bus.clone(), radio));
}

async fn run(bus: BusHandle, radio: RadioId) {
    macro_rules! sub {
        ($ty:ty, $topic:expr, $what:literal) => {
            match bus.subscribe::<$ty>(TopicSelector::Exact($topic)) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("enrich: cannot subscribe {}: {e:?}", $what);
                    return;
                }
            }
        };
    }

    let mut decodes = sub!(Decode, Topic::Decodes(radio.clone()), "decodes");
    let mut rig = sub!(RigState, Topic::RigState(radio.clone()), "rig_state");
    let mut scanner = sub!(ScannerState, Topic::ScannerState, "scanner state");
    let mut clock = sub!(ClockStatus, Topic::ClockStatus, "clock");
    let mut worked = sub!(WorkedSet, Topic::Worked, "worked-status");

    let out = Topic::DecodesEnriched(radio.clone());
    let mut tl = Timeline::default();

    tracing::info!("enrich: producer ready");
    loop {
        tokio::select! {
            r = decodes.recv() => match r {
                Ok(d) => {
                    if let Some(ed) = tl.enrich(d) {
                        let _ = bus.publish(&out, ed);
                    }
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = rig.recv() => match r {
                Ok(s) => { tl.latest_vfo = Some(s.vfo); tl.stamp(); }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = scanner.recv() => match r {
                // `current` is `Some` only while sweeping, so it is exactly the
                // "commanded band" override, falling back to the dial when idle.
                Ok(s) => { tl.scan_band = s.current; tl.stamp(); }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = clock.recv() => match r {
                Ok(c) => { tl.cur_slot = Some(c.slot); tl.stamp(); }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = worked.recv() => match r {
                Ok(w) => tl.worked = w,
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            else => break,
        }
    }
}

/// The per-slot band + dial timeline plus the latest worked set — everything needed
/// to enrich a decode, kept pure so it tests without the bus (like `core::worked`).
#[derive(Default)]
struct Timeline {
    /// The slot the clock is currently in (`clock/status`).
    cur_slot: Option<SlotId>,
    /// Latest dial frequency (`rig_state`), for the non-sweeping band.
    latest_vfo: Option<AbsHz>,
    /// The scanner's currently-dwelled band — `Some` only while sweeping, so it acts
    /// as the authoritative override over the dial during a scan.
    scan_band: Option<Band>,
    /// Which band each recent slot was received on (bounded to [`SLOT_BAND_KEEP`]).
    slot_band: HashMap<SlotId, Band>,
    /// Which dial each recent slot was received on (bounded to [`SLOT_BAND_KEEP`]).
    slot_dial: HashMap<SlotId, AbsHz>,
    /// Latest authoritative worked set (`logbook/worked`).
    worked: WorkedSet,
}

impl Timeline {
    /// The band the radio is on *now*: the scanner's commanded band while sweeping,
    /// else the band the dial sits in (`None` if the dial is outside the ham bands).
    fn resolved_band(&self) -> Option<Band> {
        self.scan_band
            .or_else(|| self.latest_vfo.and_then(Band::from_hz))
    }

    /// The dial the radio is actually on now — the live VFO. Deliberately NOT
    /// `calling_freq`: operation (and a future scanner) may sit off the calling
    /// frequency, so we record where the dial truly is.
    fn resolved_dial(&self) -> Option<AbsHz> {
        self.latest_vfo
    }

    /// Record the current slot's band + dial. Called on every clock / rig / scanner
    /// update, so each `slot_*[S]` keeps settling for the whole ~slot that `S` is live
    /// — and a decode for `S` (which can't arrive until `S` ends) reads the settled
    /// value.
    fn stamp(&mut self) {
        if let (Some(slot), Some(band)) = (self.cur_slot, self.resolved_band()) {
            self.slot_band.insert(slot, band);
            self.slot_band
                .retain(|s, _| s.0 + SLOT_BAND_KEEP >= slot.0);
        }
        if let (Some(slot), Some(dial)) = (self.cur_slot, self.resolved_dial()) {
            self.slot_dial.insert(slot, dial);
            self.slot_dial
                .retain(|s, _| s.0 + SLOT_BAND_KEEP >= slot.0);
        }
    }

    /// Enrich a raw decode with its band + worked-status, or `None` when we can't yet
    /// attribute its slot to a band (startup, before any slot was stamped — dropped,
    /// not mis-credited) or it isn't a slotted (FT8/FT4) decode.
    fn enrich(&self, d: Decode) -> Option<EnrichedDecode> {
        let DecodeContent::Slotted { slot, message, .. } = &d.content else {
            return None;
        };
        let band = *self.slot_band.get(slot)?;
        // Best-effort, unlike `band`: a decode whose slot has a known band but no
        // recorded dial must still be emitted, so don't `?` here. (Computed before the
        // literal below moves `d`, since `slot` borrows `d.content`.)
        let dial = self.slot_dial.get(slot).copied();
        let (callsign, grid) = station_and_grid(message);
        let worked = callsign
            .as_ref()
            .map_or(WorkedStatus::New, |c| self.worked_status(c, band));
        Some(EnrichedDecode {
            decode: d,
            callsign,
            grid,
            worked,
            band,
            dial,
        })
    }

    /// The worked-status of `(call, band)` from the latest set, preserving the entry's
    /// origin (`WorkedByMe` / future `WorkedByNetwork`). Keyed through
    /// [`Callsign::normalized`] so it matches the producer's `(call, band)` keys.
    fn worked_status(&self, call: &Callsign, band: Band) -> WorkedStatus {
        let key = call.normalized();
        self.worked
            .entries
            .iter()
            .find(|e| e.band == band && e.call == key)
            .map_or(WorkedStatus::New, |e| e.status.clone())
    }
}

/// The transmitting station and the location it advertises (if any) in a decoded
/// message — the `(callsign, grid)` the panels read off [`EnrichedDecode`]. A Field
/// Day exchange carries an ARRL/RAC section rather than a grid, so it yields a call
/// but no grid. (A local copy of the `station_of` logic the scanner/map/waterslide
/// each carry — converging these on one shared extractor is a tracked follow-up.)
fn station_and_grid(msg: &ParsedMessage) -> (Option<Callsign>, Option<GridSquare>) {
    match msg {
        ParsedMessage::Cq { caller, grid, .. } => (Some(caller.clone()), grid.clone()),
        ParsedMessage::Exchange { from, payload, .. } => {
            let grid = match payload {
                ExchangePayload::Grid(g) => Some(g.clone()),
                _ => None,
            };
            (Some(from.clone()), grid)
        }
        ParsedMessage::Signoff { from, .. } => (Some(from.clone()), None),
        ParsedMessage::Free(_) | ParsedMessage::Raw(_) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bus::types::{OffsetHz, OverAirMode, SignalSource, Timestamp, WorkedEntry};

    fn decode(slot: u64, msg: ParsedMessage) -> Decode {
        Decode {
            radio: RadioId("rig0".into()),
            mode: OverAirMode::Ft8,
            t: Timestamp(0),
            offset: OffsetHz(1500.0),
            snr_db: Some(-5),
            source: SignalSource::Received,
            content: DecodeContent::Slotted {
                slot: SlotId(slot),
                dt: 0.1,
                message: msg,
                raw: String::new(),
            },
        }
    }

    fn cq(call: &str, grid: &str) -> ParsedMessage {
        ParsedMessage::Cq {
            caller: Callsign(call.into()),
            contest: None,
            grid: Some(GridSquare(grid.into())),
        }
    }

    #[test]
    fn attributes_a_lagged_decode_to_its_slots_band_not_the_current_dial() {
        // Slot 100 received on 20 m.
        let mut tl = Timeline {
            cur_slot: Some(SlotId(100)),
            latest_vfo: Some(AbsHz(14_074_000)),
            ..Default::default()
        };
        tl.stamp();
        // The sweep hops to 40 m for slot 101 — the scanner's commanded band wins over
        // the dial, and settles within the slot before any of its decodes arrive.
        tl.scan_band = Some(Band::B40m);
        tl.cur_slot = Some(SlotId(101));
        tl.stamp();
        // A decode for slot 100 arrives now, a slot late: it must read 20 m (the band
        // its audio was on), not 40 m (where the dial has since hopped).
        let ed = tl.enrich(decode(100, cq("W1ABC", "FN42"))).expect("attributed");
        assert_eq!(ed.band, Band::B20m);
        // The new slot's decode reads 40 m.
        let ed = tl.enrich(decode(101, cq("K2DEF", "EM73"))).expect("attributed");
        assert_eq!(ed.band, Band::B40m);
    }

    #[test]
    fn fills_worked_status_against_the_resolved_band_case_insensitively() {
        let mut tl = Timeline {
            cur_slot: Some(SlotId(1)),
            latest_vfo: Some(AbsHz(14_074_000)), // 20 m
            worked: WorkedSet {
                entries: vec![WorkedEntry {
                    call: Callsign("W1ABC".into()),
                    band: Band::B20m,
                    status: WorkedStatus::WorkedByMe,
                }],
            },
            ..Default::default()
        };
        tl.stamp();
        // Worked on 20 m, matched through the normalized key despite the lowercase call.
        let ed = tl.enrich(decode(1, cq("w1abc", "FN42"))).unwrap();
        assert_eq!(ed.worked, WorkedStatus::WorkedByMe);
        assert_eq!(ed.callsign, Some(Callsign("w1abc".into())), "raw call preserved");
        // A different call on the same band is unworked.
        let ed = tl.enrich(decode(1, cq("K2DEF", "EM73"))).unwrap();
        assert_eq!(ed.worked, WorkedStatus::New);
    }

    #[test]
    fn extracts_call_and_grid_per_message_kind() {
        let mut tl = Timeline {
            cur_slot: Some(SlotId(1)),
            latest_vfo: Some(AbsHz(14_074_000)),
            ..Default::default()
        };
        tl.stamp();
        let ex = ParsedMessage::Exchange {
            to: Callsign("N0JDC".into()),
            from: Callsign("W4LL".into()),
            payload: ExchangePayload::Grid(GridSquare("EM73".into())),
        };
        let ed = tl.enrich(decode(1, ex)).unwrap();
        assert_eq!(ed.callsign, Some(Callsign("W4LL".into())));
        assert_eq!(ed.grid, Some(GridSquare("EM73".into())));
        // A bare Free message names no station and no grid.
        let ed = tl.enrich(decode(1, ParsedMessage::Free("TNX 73".into()))).unwrap();
        assert_eq!(ed.callsign, None);
        assert_eq!(ed.grid, None);
    }

    #[test]
    fn skips_decodes_for_an_unrecorded_slot() {
        // No slot stamped yet → a decode can't be attributed and is dropped.
        let tl = Timeline::default();
        assert!(tl.enrich(decode(7, cq("W1ABC", "FN42"))).is_none());
    }
}
