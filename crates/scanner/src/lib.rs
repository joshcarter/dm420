//! The band scanner — the pure sweep engine.
//!
//! A *strategy for single-receiver hardware*: on demand it time-slices the
//! receiver across selected bands and modes, dwelling a couple of slots on each and
//! reporting per-band heard/unworked counts. This crate is the **pure state
//! machine** only — no clock, no bus, no radio. The `core::scanner` shell drives
//! it: it feeds slot boundaries ([`Scanner::on_slot`]), decodes
//! ([`Scanner::on_decode`]) and logged contacts ([`Scanner::on_logged`]), and acts
//! on the [`Step`] / [`Scanner::current`] / [`Scanner::tallies`] it reports
//! (retuning the rig, switching mode, publishing state). Keeping the logic pure
//! makes the sweep testable without hardware (mirrors `qso::engine`).
//!
//! Behaviour (per `docs/band_scanner.md`, with the agreed tweaks):
//! - Dwell **≥2 slots** per stop so both even/odd TX parities are covered — a
//!   one-slot dwell would miss every station whose transmit turn is the other slot.
//! - Scan **both FT8 and FT4**. The plan is **mode-major** (all bands in FT8, then
//!   all bands in FT4) because a band change is a cheap retune but a mode change
//!   restarts audio capture — so we change mode only twice per full cycle.
//! - **Loop until cancelled**, accumulating heard stations across passes so later
//!   passes pick up traffic an earlier one missed.
//!
//! Specs: `docs/band_scanner.md`, `docs/message-catalog.md` §8. Owner: Josh (N0JDC).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use types::{Band, Callsign, OverAirMode, ParsedMessage, calling_freq};

/// Where the sweep is in its life cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Scanning,
}

/// What the shell should do after accounting one slot boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Stay on the current stop; keep counting this dwell.
    Dwell,
    /// The dwell finished — retune to this `(mode, band)` stop next.
    Hop { mode: OverAirMode, band: Band },
}

/// Per-band tally for the panel: distinct stations heard, and how many of those are
/// unworked (heard on this band but not in the logbook for this band).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandTally {
    pub band: Band,
    pub seen: u32,
    pub unworked: u32,
}

/// The pure band-scan sweep state machine. See the module docs for how the shell
/// drives it.
pub struct Scanner {
    phase: Phase,
    /// Bands in display order (e.g. 40, 20, 15, 10) — also the tally order.
    bands: Vec<Band>,
    /// The sweep order, **mode-major** (FT8 across all bands, then FT4) so a full
    /// cycle changes mode only twice — mode changes restart capture, retunes don't.
    plan: Vec<(OverAirMode, Band)>,
    /// Slots to dwell per stop (clamped to ≥2 for even/odd parity coverage).
    dwell_slots: u8,
    cursor: usize,
    slots_here: u8,
    /// Distinct stations heard per band, **accumulated across the whole scan** so
    /// looping picks up traffic an earlier pass missed.
    heard: HashMap<Band, HashSet<Callsign>>,
    /// `(call, band)` pairs already in the logbook — fed from `logbook/entries`.
    worked: HashSet<(Callsign, Band)>,
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            bands: Vec::new(),
            plan: Vec::new(),
            dwell_slots: 2,
            cursor: 0,
            slots_here: 0,
            heard: HashMap::new(),
            worked: HashSet::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The stop currently being monitored, if scanning.
    pub fn current(&self) -> Option<(OverAirMode, Band)> {
        match self.phase {
            Phase::Scanning => self.plan.get(self.cursor).copied(),
            Phase::Idle => None,
        }
    }

    /// Begin a survey of `bands` in each of `modes`, dwelling `dwell_slots` slots
    /// (clamped to ≥2 for parity) per stop. Builds the mode-major plan, skipping any
    /// `(band, mode)` with no established calling frequency (e.g. FT4 on 160 m).
    /// Resets the heard tallies (a fresh scan) but keeps the worked-set. Returns the
    /// first stop to tune, or `None` if no stop is valid.
    pub fn start(
        &mut self,
        bands: &[Band],
        modes: &[OverAirMode],
        dwell_slots: u8,
    ) -> Option<(OverAirMode, Band)> {
        self.bands = bands.to_vec();
        self.plan = modes
            .iter()
            .flat_map(|&m| bands.iter().map(move |&b| (m, b)))
            .filter(|&(m, b)| calling_freq(b, m).is_some())
            .collect();
        self.dwell_slots = dwell_slots.max(2);
        self.cursor = 0;
        self.slots_here = 0;
        self.heard.clear();
        if self.plan.is_empty() {
            self.phase = Phase::Idle;
            return None;
        }
        self.phase = Phase::Scanning;
        self.current()
    }

    /// Stop scanning. Tallies are kept so the panel still shows the last results.
    pub fn cancel(&mut self) {
        self.phase = Phase::Idle;
        self.slots_here = 0;
    }

    /// Account one elapsed slot at the current stop. After `dwell_slots` slots,
    /// advance to the next stop — **wrapping**, so the scan loops until cancelled —
    /// and report the [`Step::Hop`] to retune to. Otherwise [`Step::Dwell`].
    pub fn on_slot(&mut self) -> Step {
        if self.phase != Phase::Scanning || self.plan.is_empty() {
            return Step::Dwell;
        }
        self.slots_here += 1;
        if self.slots_here >= self.dwell_slots {
            self.slots_here = 0;
            self.cursor = (self.cursor + 1) % self.plan.len();
            let (mode, band) = self.plan[self.cursor];
            Step::Hop { mode, band }
        } else {
            Step::Dwell
        }
    }

    /// Note a decoded message heard on `band`. The transmitting station (CQ caller,
    /// or the sender of an exchange / sign-off) is added to that band's heard set;
    /// unclassifiable text is ignored. The caller supplies `band` rather than the
    /// engine assuming the *current* stop, because a decode arrives a slot or two
    /// after its audio (the decoder runs once the slot ends) — by which point the
    /// sweep may have hopped on — so it must be credited to the band that was tuned
    /// when it was on the air.
    pub fn on_decode(&mut self, band: Band, msg: &ParsedMessage) {
        if self.phase != Phase::Scanning {
            return;
        }
        if let Some(call) = station_of(msg) {
            self.heard.entry(band).or_default().insert(call.clone());
        }
    }

    /// Record a logged contact so it counts as worked on its band (drives the
    /// unworked tally). Fed from the logbook's startup replay + live
    /// `logbook/entries`.
    pub fn on_logged(&mut self, call: Callsign, band: Band) {
        self.worked.insert((call, band));
    }

    /// Per-band tallies in display order: distinct stations heard and how many are
    /// unworked on that band.
    pub fn tallies(&self) -> Vec<BandTally> {
        self.bands
            .iter()
            .map(|&band| {
                let heard = self.heard.get(&band);
                let seen = heard.map_or(0, |h| h.len()) as u32;
                let unworked = heard.map_or(0, |h| {
                    h.iter()
                        .filter(|&c| !self.worked.contains(&(c.clone(), band)))
                        .count()
                }) as u32;
                BandTally {
                    band,
                    seen,
                    unworked,
                }
            })
            .collect()
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

/// The transmitting station in a decoded message, if it names one.
fn station_of(msg: &ParsedMessage) -> Option<&Callsign> {
    match msg {
        ParsedMessage::Cq { caller, .. } => Some(caller),
        ParsedMessage::Exchange { from, .. } => Some(from),
        ParsedMessage::Signoff { from, .. } => Some(from),
        ParsedMessage::Free(_) | ParsedMessage::Raw(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{ExchangePayload, GridSquare, Signoff};

    fn call(s: &str) -> Callsign {
        Callsign(s.into())
    }

    fn cq(c: &str) -> ParsedMessage {
        ParsedMessage::Cq {
            caller: call(c),
            contest: None,
            grid: Some(GridSquare("FN42".into())),
        }
    }

    #[test]
    fn plan_is_mode_major_and_skips_missing_calling_freq() {
        let mut s = Scanner::new();
        // 160 m has an FT8 calling freq but no FT4 one — so (FT4, 160m) is skipped.
        let first = s.start(&[Band::B160m, Band::B20m], &[OverAirMode::Ft8, OverAirMode::Ft4], 2);
        assert_eq!(first, Some((OverAirMode::Ft8, Band::B160m)));
        // Mode-major: all FT8 stops, then the surviving FT4 stop.
        assert_eq!(
            s.plan,
            vec![
                (OverAirMode::Ft8, Band::B160m),
                (OverAirMode::Ft8, Band::B20m),
                (OverAirMode::Ft4, Band::B20m),
            ]
        );
    }

    #[test]
    fn dwells_two_slots_then_hops_and_loops() {
        let mut s = Scanner::new();
        s.start(&[Band::B40m, Band::B20m], &[OverAirMode::Ft8], 2);
        assert_eq!(s.current(), Some((OverAirMode::Ft8, Band::B40m)));
        // First slot: keep dwelling.
        assert_eq!(s.on_slot(), Step::Dwell);
        // Second slot: hop to the next band.
        assert_eq!(
            s.on_slot(),
            Step::Hop {
                mode: OverAirMode::Ft8,
                band: Band::B20m
            }
        );
        assert_eq!(s.current(), Some((OverAirMode::Ft8, Band::B20m)));
        // Two more slots wrap back to the first band — the scan loops.
        assert_eq!(s.on_slot(), Step::Dwell);
        assert_eq!(
            s.on_slot(),
            Step::Hop {
                mode: OverAirMode::Ft8,
                band: Band::B40m
            }
        );
    }

    #[test]
    fn dwell_is_clamped_to_two_for_parity() {
        let mut s = Scanner::new();
        s.start(&[Band::B40m, Band::B20m], &[OverAirMode::Ft8], 1); // ask for 1…
        assert_eq!(s.on_slot(), Step::Dwell); // …but we still dwell 2.
        assert!(matches!(s.on_slot(), Step::Hop { .. }));
    }

    #[test]
    fn tallies_count_distinct_heard_and_unworked() {
        let mut s = Scanner::new();
        s.start(&[Band::B20m], &[OverAirMode::Ft8], 2);
        s.on_decode(Band::B20m, &cq("W1ABC"));
        s.on_decode(Band::B20m, &cq("K2DEF"));
        // A second decode from W1ABC (as an exchange sender) must not double-count.
        s.on_decode(Band::B20m, &ParsedMessage::Exchange {
            to: call("N0JDC"),
            from: call("W1ABC"),
            payload: ExchangePayload::Report(-7),
        });
        // Worked W1ABC on 20 m; K2DEF stays unworked. A contact on another band
        // must not mark them worked here.
        s.on_logged(call("W1ABC"), Band::B20m);
        s.on_logged(call("K2DEF"), Band::B40m);
        let t = s.tallies();
        assert_eq!(t, vec![BandTally { band: Band::B20m, seen: 2, unworked: 1 }]);
    }

    #[test]
    fn signoff_sender_counts_as_heard() {
        let mut s = Scanner::new();
        s.start(&[Band::B20m], &[OverAirMode::Ft8], 2);
        s.on_decode(Band::B20m, &ParsedMessage::Signoff {
            to: call("N0JDC"),
            from: call("W1ABC"),
            kind: Signoff::Rr73,
        });
        // Free text names no station, so it's ignored.
        s.on_decode(Band::B20m, &ParsedMessage::Free("hello world".into()));
        assert_eq!(s.tallies()[0].seen, 1);
    }

    #[test]
    fn idle_ignores_input() {
        let mut s = Scanner::new();
        assert_eq!(s.current(), None);
        assert_eq!(s.on_slot(), Step::Dwell);
        s.on_decode(Band::B20m, &cq("W1ABC")); // no-op while idle
        assert!(s.tallies().is_empty());
    }

    #[test]
    fn cancel_stops_but_keeps_tallies() {
        let mut s = Scanner::new();
        s.start(&[Band::B20m], &[OverAirMode::Ft8], 2);
        s.on_decode(Band::B20m, &cq("W1ABC"));
        s.cancel();
        assert_eq!(s.phase(), Phase::Idle);
        assert_eq!(s.current(), None);
        // Last results survive for the panel.
        assert_eq!(s.tallies()[0].seen, 1);
    }
}
