//! The band scanner — the pure sweep engine.
//!
//! A *strategy for single-receiver hardware*: on demand it time-slices the
//! receiver across selected band/mode stops, dwelling a couple of slots on each and
//! reporting per-(band, mode) heard / calling-CQ / unworked counts. This crate is
//! the **pure state machine** only — no clock, no bus, no radio. The `core::scan`
//! shell drives it: it feeds slot boundaries ([`Scanner::on_slot`]), decodes
//! ([`Scanner::on_decode`]) and the worked set ([`Scanner::set_worked`], mirrored
//! from the `core::worked` producer), and acts
//! on the [`Step`] / [`Scanner::current`] / [`Scanner::tallies`] it reports
//! (retuning the rig, switching mode, publishing state). Keeping the logic pure
//! makes the sweep testable without hardware (mirrors `qso::engine`).
//!
//! Behaviour (per `docs/band_scanner.md`, with the agreed tweaks):
//! - Dwell **≥2 slots** per stop so both even/odd TX parities are covered.
//! - Scan the selected **(band, mode) stops** (the panel's per-band FT8/FT4
//!   toggles). The plan is **mode-major** (all FT8 stops, then FT4) because a mode
//!   change restarts audio capture while a band change is a cheap retune.
//! - **Loop until cancelled**, accumulating distinct callsigns across passes so
//!   later passes pick up traffic an earlier one missed.
//! - Counts are split into **heard** (every station decoded transmitting) and **cq**
//!   (those calling CQ, a subset), each per band *and* mode, plus **unworked**.
//!
//! Specs: `docs/band_scanner.md`, `docs/message-catalog.md` §8. Owner: Josh (N0JDC).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use types::{Band, Callsign, OverAirMode, ParsedMessage, calling_freq};

/// Mode-major plan order: FT8 stops before FT4 (a mode change restarts capture).
const MODE_ORDER: [OverAirMode; 2] = [OverAirMode::Ft8, OverAirMode::Ft4];

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

/// Per-(band, mode) tally for the panel. `cq` ⊆ `heard`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandTally {
    pub band: Band,
    pub mode: OverAirMode,
    /// Distinct stations heard transmitting.
    pub heard: u32,
    /// Distinct stations heard calling CQ (subset of `heard`).
    pub cq: u32,
    /// Distinct heard stations not logged on this band + mode.
    pub unworked: u32,
}

/// The pure band-scan sweep state machine. See the module docs for how the shell
/// drives it.
pub struct Scanner {
    phase: Phase,
    /// Distinct bands in display order (first-seen across the stops).
    bands: Vec<Band>,
    /// The sweep order: `(mode, band)` stops, **mode-major**.
    plan: Vec<(OverAirMode, Band)>,
    /// Slots to dwell per stop (clamped to ≥2 for even/odd parity coverage).
    dwell_slots: u8,
    cursor: usize,
    slots_here: u8,
    /// Distinct stations heard per `(band, mode)`, accumulated across the scan.
    heard: HashMap<(Band, OverAirMode), HashSet<Callsign>>,
    /// Distinct CQ callers per `(band, mode)` (a subset of `heard`).
    cq: HashMap<(Band, OverAirMode), HashSet<Callsign>>,
    /// `(call, band)` already worked — set wholesale via [`Scanner::set_worked`] from
    /// the `core::worked` producer (the single owner). Keyed per band, not per mode:
    /// under the ARRL Field Day rule all digital modes count as one, so a station
    /// worked on a band is a dupe there regardless of FT8/FT4.
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
            cq: HashMap::new(),
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

    /// Begin a survey of the given `(band, mode)` `stops`, dwelling `dwell_slots`
    /// slots (clamped to ≥2 for parity) per stop. Reorders the stops **mode-major**
    /// (all FT8, then FT4), drops any with no calling frequency (e.g. FT4 on 160 m),
    /// and dedups. Resets the heard/cq tallies (a fresh scan) but keeps the
    /// worked-set. Returns the first stop to tune, or `None` if no stop is valid.
    pub fn start(&mut self, stops: &[(Band, OverAirMode)], dwell_slots: u8) -> Option<(OverAirMode, Band)> {
        let mut bands = Vec::new();
        for &(b, _) in stops {
            if !bands.contains(&b) {
                bands.push(b);
            }
        }
        self.bands = bands;
        let mut plan = Vec::new();
        for &m in &MODE_ORDER {
            for &(b, sm) in stops {
                if sm == m && calling_freq(b, m).is_some() && !plan.contains(&(m, b)) {
                    plan.push((m, b));
                }
            }
        }
        self.plan = plan;
        self.dwell_slots = dwell_slots.max(2);
        self.cursor = 0;
        self.slots_here = 0;
        self.heard.clear();
        self.cq.clear();
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

    /// Replace the sweep's stops mid-scan **without** resetting the accumulated
    /// counts (the panel's band/mode toggles). Rebuilds the mode-major plan, keeps
    /// the heard/cq and worked sets, and keeps dwelling the current stop if it
    /// survived. Returns `true` if the current stop was removed (toggled off) — the
    /// shell should retune to the new [`current`](Self::current). No-op while idle;
    /// if every stop is toggled off the sweep goes [`Phase::Idle`].
    pub fn update_stops(&mut self, stops: &[(Band, OverAirMode)]) -> bool {
        if self.phase != Phase::Scanning {
            return false;
        }
        let prev = self.current();
        let mut bands = Vec::new();
        for &(b, _) in stops {
            if !bands.contains(&b) {
                bands.push(b);
            }
        }
        self.bands = bands;
        let mut plan = Vec::new();
        for &m in &MODE_ORDER {
            for &(b, sm) in stops {
                if sm == m && calling_freq(b, m).is_some() && !plan.contains(&(m, b)) {
                    plan.push((m, b));
                }
            }
        }
        self.plan = plan;
        if self.plan.is_empty() {
            self.phase = Phase::Idle;
            return false;
        }
        match prev.and_then(|c| self.plan.iter().position(|&p| p == c)) {
            // The stop we were dwelling survived — keep dwelling it.
            Some(i) => {
                self.cursor = i;
                false
            }
            // It was removed — clamp the cursor and tell the shell to retune.
            None => {
                self.cursor %= self.plan.len();
                self.slots_here = 0;
                true
            }
        }
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

    /// Note a decoded message heard on `band` in `mode`. The transmitting station
    /// (CQ caller, or the sender of an exchange / sign-off) is added to that
    /// `(band, mode)`'s heard set, and to its CQ set when the message is a CQ;
    /// unclassifiable text is ignored. The caller supplies `band`/`mode` rather than
    /// the engine assuming the *current* stop, because a decode arrives a slot or two
    /// after its audio — by which point the sweep may have hopped — so it must be
    /// credited to where it was actually heard.
    pub fn on_decode(&mut self, band: Band, mode: OverAirMode, msg: &ParsedMessage) {
        if self.phase != Phase::Scanning {
            return;
        }
        if let Some(call) = station_of(msg) {
            self.heard.entry((band, mode)).or_default().insert(call.clone());
            if matches!(msg, ParsedMessage::Cq { .. }) {
                self.cq.entry((band, mode)).or_default().insert(call.clone());
            }
        }
    }

    /// Replace the worked set with the authoritative snapshot from the worked-status
    /// producer (`logbook/worked`). This is how the scanner *reads* worked-status
    /// instead of deriving it: the `core::scan` shell subscribes to the single owner
    /// and feeds the canonical `(call, band)` set here, rather than the scanner
    /// folding raw `logbook/entries` itself with its own key. Keys are normalized
    /// (trimmed + ASCII upper-cased) by `worked_key` upstream, so [`Scanner::tallies`]
    /// upper-cases the heard call to match.
    pub fn set_worked(&mut self, worked: HashSet<(Callsign, Band)>) {
        self.worked = worked;
    }

    /// Per-(band, mode) tallies, one per planned stop in sweep order.
    pub fn tallies(&self) -> Vec<BandTally> {
        self.plan
            .iter()
            .map(|&(mode, band)| {
                let heard_set = self.heard.get(&(band, mode));
                let heard = heard_set.map_or(0, |h| h.len()) as u32;
                let cq = self.cq.get(&(band, mode)).map_or(0, |c| c.len()) as u32;
                // Upper-case the heard call to match the worked set's normalized keys
                // (`worked_key` trims + upper-cases), so case-folding can't make a
                // worked station read unworked.
                let unworked = heard_set.map_or(0, |h| {
                    h.iter()
                        .filter(|&c| {
                            !self
                                .worked
                                .contains(&(Callsign(c.0.to_ascii_uppercase()), band))
                        })
                        .count()
                }) as u32;
                BandTally {
                    band,
                    mode,
                    heard,
                    cq,
                    unworked,
                }
            })
            .collect()
    }

    /// The distinct bands being scanned, in display order.
    pub fn bands(&self) -> &[Band] {
        &self.bands
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
    use types::{ExchangePayload, GridSquare};

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

    fn exch(from: &str) -> ParsedMessage {
        ParsedMessage::Exchange {
            to: call("N0JDC"),
            from: call(from),
            payload: ExchangePayload::Report(-7),
        }
    }

    fn tally(s: &Scanner, band: Band, mode: OverAirMode) -> BandTally {
        s.tallies()
            .into_iter()
            .find(|t| t.band == band && t.mode == mode)
            .expect("tally for band/mode")
    }

    #[test]
    fn plan_is_mode_major_and_skips_missing_calling_freq() {
        let mut s = Scanner::new();
        // Stops arrive band-major from the panel; the engine reorders mode-major and
        // drops (FT4, 160m) which has no calling frequency.
        let first = s.start(
            &[
                (Band::B160m, OverAirMode::Ft8),
                (Band::B160m, OverAirMode::Ft4),
                (Band::B20m, OverAirMode::Ft8),
                (Band::B20m, OverAirMode::Ft4),
            ],
            2,
        );
        assert_eq!(first, Some((OverAirMode::Ft8, Band::B160m)));
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
        s.start(&[(Band::B40m, OverAirMode::Ft8), (Band::B20m, OverAirMode::Ft8)], 2);
        assert_eq!(s.current(), Some((OverAirMode::Ft8, Band::B40m)));
        assert_eq!(s.on_slot(), Step::Dwell);
        assert_eq!(s.on_slot(), Step::Hop { mode: OverAirMode::Ft8, band: Band::B20m });
        assert_eq!(s.on_slot(), Step::Dwell);
        // Wraps back to the first stop — the scan loops.
        assert_eq!(s.on_slot(), Step::Hop { mode: OverAirMode::Ft8, band: Band::B40m });
    }

    #[test]
    fn dwell_is_clamped_to_two_for_parity() {
        let mut s = Scanner::new();
        s.start(&[(Band::B40m, OverAirMode::Ft8), (Band::B20m, OverAirMode::Ft8)], 1);
        assert_eq!(s.on_slot(), Step::Dwell);
        assert!(matches!(s.on_slot(), Step::Hop { .. }));
    }

    #[test]
    fn cq_is_a_subset_of_heard() {
        let mut s = Scanner::new();
        s.start(&[(Band::B20m, OverAirMode::Ft8)], 2);
        s.on_decode(Band::B20m, OverAirMode::Ft8, &cq("W1ABC")); // CQ → heard + cq
        s.on_decode(Band::B20m, OverAirMode::Ft8, &exch("K2DEF")); // exchange → heard only
        s.on_decode(Band::B20m, OverAirMode::Ft8, &exch("W1ABC")); // dup of W1ABC
        let t = tally(&s, Band::B20m, OverAirMode::Ft8);
        assert_eq!(t.heard, 2); // W1ABC, K2DEF
        assert_eq!(t.cq, 1); // only W1ABC called CQ
    }

    #[test]
    fn heard_cq_split_by_mode_but_worked_is_per_band() {
        let mut s = Scanner::new();
        s.start(
            &[(Band::B20m, OverAirMode::Ft8), (Band::B20m, OverAirMode::Ft4)],
            2,
        );
        // W1ABC is heard on both modes; K2DEF only on FT4.
        s.on_decode(Band::B20m, OverAirMode::Ft8, &cq("W1ABC"));
        s.on_decode(Band::B20m, OverAirMode::Ft4, &cq("W1ABC"));
        s.on_decode(Band::B20m, OverAirMode::Ft4, &cq("K2DEF"));
        // Work W1ABC on 20m. The worked set arrives wholesale from the worked-status
        // producer (keyed `(call, band)`, mode dropped — Field Day counts all digital
        // modes as one), so W1ABC is worked on 20m regardless of mode and therefore not
        // unworked on FT4 either. K2DEF stays unworked. heard/CQ remain split per
        // (band, mode).
        s.set_worked(HashSet::from([(call("W1ABC"), Band::B20m)]));
        let ft8 = tally(&s, Band::B20m, OverAirMode::Ft8);
        let ft4 = tally(&s, Band::B20m, OverAirMode::Ft4);
        assert_eq!((ft8.heard, ft8.cq, ft8.unworked), (1, 1, 0));
        // Under the old per-(band,mode) rule this would be (2, 2, 2): W1ABC worked only
        // on FT8 would still read unworked on FT4. Per-band, only K2DEF is unworked.
        assert_eq!((ft4.heard, ft4.cq, ft4.unworked), (2, 2, 1));
    }

    #[test]
    fn idle_ignores_input() {
        let mut s = Scanner::new();
        assert_eq!(s.current(), None);
        assert_eq!(s.on_slot(), Step::Dwell);
        s.on_decode(Band::B20m, OverAirMode::Ft8, &cq("W1ABC")); // no-op while idle
        assert!(s.tallies().is_empty());
    }

    #[test]
    fn cancel_stops_but_keeps_tallies() {
        let mut s = Scanner::new();
        s.start(&[(Band::B20m, OverAirMode::Ft8)], 2);
        s.on_decode(Band::B20m, OverAirMode::Ft8, &cq("W1ABC"));
        s.cancel();
        assert_eq!(s.phase(), Phase::Idle);
        assert_eq!(s.current(), None);
        assert_eq!(tally(&s, Band::B20m, OverAirMode::Ft8).heard, 1);
    }

    #[test]
    fn update_stops_replans_and_preserves_counts() {
        let mut s = Scanner::new();
        s.start(
            &[(Band::B40m, OverAirMode::Ft8), (Band::B20m, OverAirMode::Ft8)],
            2,
        );
        s.on_decode(Band::B40m, OverAirMode::Ft8, &cq("W1ABC"));
        // Drop 40m (the current stop), add 15m → current changed, so retune.
        assert!(s.update_stops(&[
            (Band::B20m, OverAirMode::Ft8),
            (Band::B15m, OverAirMode::Ft8),
        ]));
        assert!(s.tallies().iter().all(|t| t.band != Band::B40m)); // 40m no longer reported
        // Bring 40m back — its accumulated count survived (counts are not reset).
        s.update_stops(&[(Band::B40m, OverAirMode::Ft8), (Band::B20m, OverAirMode::Ft8)]);
        assert_eq!(tally(&s, Band::B40m, OverAirMode::Ft8).heard, 1);
    }
}
