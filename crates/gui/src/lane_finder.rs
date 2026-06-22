//! Clear-lane finder: pick the best audio offset to call CQ on.
//!
//! Opinionated, not advisory (`JOELS_ROADMAP.md` Now-#10): given the recent RX
//! spectrum and decodes, it returns the single best *base* audio offset (Hz) for
//! the next CQ — the operator just jumps there, no menu of choices. Scoped to the
//! current band/mode; it only moves the outgoing TX offset, never the dial.
//!
//! The scoring is a pure, deterministic function so it's unit-testable and so a
//! future bus-published occupancy map can feed it unchanged (see the occupancy-map
//! note under Now-#10). A signal occupies `[base, base + bandwidth_hz]`, matching
//! how the waterslide draws the TX lane and how `call_cq` interprets the offset.
//!
//! Scoring terms (lower cost wins):
//! - **Clearness** — recency-weighted energy in the lane + a guard band on each
//!   side, measured as *excess* over the band's quiet floor so a truly clear lane
//!   scores ~0. Peak-dominated (one strong signal disqualifies a lane) with mean
//!   as a secondary term. Persistent carriers/birdies are avoided for free: they
//!   read as occupied energy.
//! - **Decode proximity** — a penalty near the offset of any recently-decoded
//!   station, even one momentarily idle between overs (it still owns its lane).
//! - **Center bias** — pull toward the passband center, which does triple duty:
//!   the flat part of the SSB filter (best TX/RX), the window listeners actually
//!   watch, and staying inside the TX-offset limits.

use types::{Decode, SignalSource, SpectrumRow};

/// Full audio span we analyze (Hz). FT8/FT4 traffic lands in ~0..3000 Hz.
const MAX_HZ: f32 = 3000.0;
/// Analysis grid resolution (Hz). Finer than a signal's width so the lane and its
/// guard land on several cells.
const STEP_HZ: f32 = 5.0;

/// Usable window for the *whole* signal `[base, base + bw]` — kept inside a ~3 kHz
/// SSB filter and clear of the skirts (below ~`LO`, above ~`HI`) where TX and RX
/// roll off. The picker never returns a lane outside this.
const USABLE_LO_HZ: f32 = 250.0;
const USABLE_HI_HZ: f32 = 2750.0;

/// Bias target for the signal *center*: the flattest part of the passband and the
/// region listeners watch. ~1500 Hz is the conventional FT8 sweet spot.
const CENTER_HZ: f32 = 1500.0;

/// Recency half-life (ms) for the occupancy histogram — older spectrum rows count
/// for exponentially less, so the map reflects "what's busy now."
const HALF_LIFE_MS: f32 = 8_000.0;

/// Only weigh decodes seen this recently. A station between overs is silent on the
/// spectrum but still owns its lane, so we keep avoiding it for ~3 FT8 slots.
const DECODE_RECENCY_MS: i64 = 45_000;

/// Quiet-floor percentile (0..1) subtracted before scoring, so a clear lane reads
/// ~0 regardless of where the absolute noise floor sits (it rides high on a busy
/// band). Signals are the excursions above it.
const FLOOR_PCTL: f32 = 0.20;
/// Top percentile defining the dynamic range above the floor. Occupancy is
/// normalized to `[floor, p95]`, making scoring independent of the absolute u8
/// magnitude scale (which we don't pin down — `dsp` packs a dB range into 0..255).
const RANGE_PCTL: f32 = 0.95;
/// Floor on the normalization scale (magnitude units) so a dead-quiet band's tiny
/// noise wiggles aren't amplified into apparent occupancy.
const MIN_SCALE: f32 = 16.0;

// Scoring weights, all in the normalized 0..~1 occupancy space where 1 ≈ the
// band's strongest signal. `occ_cost = (W_PEAK*peak + W_MEAN*mean) / scale`.
const W_PEAK: f32 = 0.7;
const W_MEAN: f32 = 0.3;
/// Maximum contribution of the center bias, reached `CENTER_DEV_HZ` away. Kept
/// well below a real signal's normalized peak so clearness dominates and center
/// only decides among similarly-clear lanes.
const CENTER_FRAC: f32 = 0.25;
const CENTER_DEV_HZ: f32 = 1250.0;
/// Penalty for a lane sitting on a recent decode, fading to 0 at `decode_margin`.
/// ~1.0 (≈ the strongest signal) so we route around known stations even when
/// they're briefly idle between overs.
const DECODE_PENALTY: f32 = 1.0;

/// Recency-weighted occupancy over `[0, MAX_HZ]` at `STEP_HZ`, with the quiet
/// floor already subtracted out (`cells[c]` is excess energy at `c * STEP_HZ`).
struct Occupancy {
    cells: Vec<f32>,
    /// Normalization scale: the magnitude range from the floor to the `RANGE_PCTL`
    /// percentile, clamped to `MIN_SCALE`. Excess divided by this is ~1 at the
    /// band's strongest signal, so cost weights are scale-independent.
    scale: f32,
}

impl Occupancy {
    fn cell_count() -> usize {
        (MAX_HZ / STEP_HZ).ceil() as usize + 1
    }

    /// Build the histogram from recent RX spectrum rows. Each row is resampled
    /// onto the common grid by nearest bin, weighted by recency against the newest
    /// row. OwnTx rows (our own transmissions) are ignored.
    fn build(rows: &[SpectrumRow]) -> Self {
        let n = Self::cell_count();
        let mut acc = vec![0.0f32; n];
        let mut wsum = vec![0.0f32; n];

        let newest = rows
            .iter()
            .filter(|r| r.source == SignalSource::Received)
            .map(|r| r.t.0)
            .max();
        let Some(newest) = newest else {
            return Self {
                cells: vec![0.0; n],
                scale: MIN_SCALE,
            };
        };

        for r in rows {
            if r.source != SignalSource::Received || r.mags.is_empty() || r.bin_hz <= 0.0 {
                continue;
            }
            let age = (newest - r.t.0).max(0) as f32;
            let w = 0.5f32.powf(age / HALF_LIFE_MS);
            for (c, slot) in acc.iter_mut().enumerate() {
                let f = c as f32 * STEP_HZ;
                let bin = ((f - r.bin0_offset.0) / r.bin_hz).round();
                if bin < 0.0 {
                    continue;
                }
                let bin = bin as usize;
                if let Some(&m) = r.mags.get(bin) {
                    *slot += w * m as f32;
                    wsum[c] += w;
                }
            }
        }

        let mut cells: Vec<f32> = acc
            .iter()
            .zip(&wsum)
            .map(|(a, w)| if *w > 0.0 { a / w } else { 0.0 })
            .collect();

        // Subtract the quiet floor and derive the normalization scale (the floor→p95
        // range) over the usable window, so clear lanes sit at ~0 and scoring is
        // independent of the absolute magnitude scale.
        let lo = (USABLE_LO_HZ / STEP_HZ) as usize;
        let hi = ((USABLE_HI_HZ / STEP_HZ) as usize + 1).min(n);
        let (floor, scale) = if hi > lo {
            let mut v: Vec<f32> = cells[lo..hi].to_vec();
            v.sort_by(|a, b| a.total_cmp(b));
            let at = |p: f32| v[(((v.len() - 1) as f32 * p) as usize).min(v.len() - 1)];
            let floor = at(FLOOR_PCTL);
            (floor, (at(RANGE_PCTL) - floor).max(MIN_SCALE))
        } else {
            (0.0, MIN_SCALE)
        };
        for cell in &mut cells {
            *cell = (*cell - floor).max(0.0);
        }
        Self { cells, scale }
    }

    /// Worst (peak) and average excess energy across `[lo_hz, hi_hz]`.
    fn region(&self, lo_hz: f32, hi_hz: f32) -> (f32, f32) {
        let lo = (lo_hz.max(0.0) / STEP_HZ) as usize;
        let hi = ((hi_hz / STEP_HZ).ceil() as usize + 1).min(self.cells.len());
        if hi <= lo {
            return (0.0, 0.0);
        }
        let mut peak = 0.0f32;
        let mut sum = 0.0f32;
        for &v in &self.cells[lo..hi] {
            peak = peak.max(v);
            sum += v;
        }
        (peak, sum / (hi - lo) as f32)
    }
}

/// The base offset that parks the signal at passband center, clamped to the usable
/// window. Used as the opinionated default when there's no spectrum to score.
fn center_base(bandwidth_hz: f32) -> f32 {
    (CENTER_HZ - bandwidth_hz / 2.0).clamp(USABLE_LO_HZ, USABLE_HI_HZ - bandwidth_hz)
}

/// Pick the best base audio offset (Hz) for a CQ in the current mode.
///
/// `rows` is recent RX spectrum, `decodes` recent decodes (both newest-first or
/// not — order doesn't matter), `bandwidth_hz` the mode's occupied width (FT8 ~50,
/// FT4 ~83), `now_ms` the current UTC time for decode recency. Returns `None` only
/// if the usable window can't fit the signal (degenerate `bandwidth_hz`).
pub fn pick_cq_offset(
    rows: &[SpectrumRow],
    decodes: &[Decode],
    bandwidth_hz: f32,
    now_ms: i64,
) -> Option<f32> {
    if bandwidth_hz <= 0.0 || USABLE_HI_HZ - bandwidth_hz < USABLE_LO_HZ {
        return None;
    }
    // No spectrum yet → opinionated default: park at passband center.
    if rows.iter().all(|r| r.source != SignalSource::Received) {
        return Some(center_base(bandwidth_hz));
    }

    let occ = Occupancy::build(rows);
    let decode_offsets: Vec<f32> = decodes
        .iter()
        .filter(|d| now_ms - d.t.0 <= DECODE_RECENCY_MS)
        .map(|d| d.offset.0)
        .collect();
    Some(pick(&occ, &decode_offsets, bandwidth_hz))
}

/// Core scoring over a prebuilt occupancy map and recent decode offsets. Split out
/// so tests can drive it with synthetic occupancy and plain offsets.
fn pick(occ: &Occupancy, decode_offsets: &[f32], bandwidth_hz: f32) -> f32 {
    let guard = (bandwidth_hz * 0.5).max(10.0);
    let decode_margin = bandwidth_hz.max(40.0);
    let hi_base = USABLE_HI_HZ - bandwidth_hz;
    let steps = ((hi_base - USABLE_LO_HZ) / STEP_HZ).floor() as usize;

    let mut best_base = center_base(bandwidth_hz);
    let mut best_cost = f32::INFINITY;
    for k in 0..=steps {
        let base = USABLE_LO_HZ + k as f32 * STEP_HZ;
        let (peak, mean) = occ.region(base - guard, base + bandwidth_hz + guard);
        let mut cost = (W_PEAK * peak + W_MEAN * mean) / occ.scale;

        // Decode proximity: gap between my lane and the decode's lane (0 if they
        // overlap); penalty fades linearly to 0 at `decode_margin`.
        for &d in decode_offsets {
            let gap = ((base - (d + bandwidth_hz)).max(d - (base + bandwidth_hz))).max(0.0);
            if gap < decode_margin {
                cost += DECODE_PENALTY * (1.0 - gap / decode_margin);
            }
        }

        // Center bias on the signal's center frequency.
        let center = base + bandwidth_hz / 2.0;
        cost += CENTER_FRAC * (center - CENTER_HZ).abs() / CENTER_DEV_HZ;

        if cost < best_cost {
            best_cost = cost;
            best_base = base;
        }
    }
    best_base
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{OffsetHz, OverAirMode, RadioId, Timestamp};

    const FT8_BW: f32 = 50.0;

    /// A received spectrum row whose magnitudes are `mags`, bin 0 at offset 0,
    /// `bin_hz` spacing, stamped at `t_ms`.
    fn row(mags: Vec<u8>, bin_hz: f32, t_ms: i64) -> SpectrumRow {
        SpectrumRow {
            radio: RadioId("rig0".into()),
            mode: OverAirMode::Ft8,
            t: Timestamp(t_ms),
            bin0_offset: OffsetHz(0.0),
            bin_hz,
            mags,
            source: SignalSource::Received,
        }
    }

    /// A flat noise floor of `floor` across 0..3000 Hz at 5 Hz/bin, with optional
    /// strong "signals" (value 230) painted into the given Hz spans.
    fn floor_with_signals(floor: u8, signals: &[(f32, f32)]) -> SpectrumRow {
        let bin_hz = 5.0;
        let n = (3000.0 / bin_hz) as usize + 1;
        let mut mags = vec![floor; n];
        for &(lo, hi) in signals {
            let a = (lo / bin_hz) as usize;
            let b = ((hi / bin_hz) as usize).min(n - 1);
            for m in mags.iter_mut().take(b + 1).skip(a) {
                *m = 230;
            }
        }
        row(mags, bin_hz, 0)
    }

    fn occ_from(rows: &[SpectrumRow]) -> Occupancy {
        Occupancy::build(rows)
    }

    #[test]
    fn empty_spectrum_defaults_to_center() {
        let off = pick_cq_offset(&[], &[], FT8_BW, 0).unwrap();
        // Signal centered on ~1500 Hz → base ~1475.
        assert!((off - (CENTER_HZ - FT8_BW / 2.0)).abs() < STEP_HZ);
    }

    #[test]
    fn degenerate_bandwidth_returns_none() {
        assert!(pick_cq_offset(&[], &[], 0.0, 0).is_none());
    }

    #[test]
    fn occupancy_floor_subtracted_clear_band_is_zero() {
        let occ = occ_from(&[floor_with_signals(120, &[])]);
        // A flat band, post-floor, is ~0 everywhere.
        let (peak, _) = occ.region(USABLE_LO_HZ, USABLE_HI_HZ);
        assert!(peak < 1.0, "flat band should be ~0 excess, got {peak}");
    }

    #[test]
    fn occupancy_marks_a_signal() {
        let occ = occ_from(&[floor_with_signals(60, &[(1480.0, 1530.0)])]);
        let (on_peak, _) = occ.region(1480.0, 1530.0);
        let (off_peak, _) = occ.region(800.0, 850.0);
        assert!(on_peak > 100.0, "signal cell should be hot, got {on_peak}");
        assert!(off_peak < 1.0, "clear cell should be ~0, got {off_peak}");
    }

    #[test]
    fn avoids_a_busy_center_picks_clear_lane() {
        // The center is jammed; the picker must move off it to a clear lane.
        let occ = occ_from(&[floor_with_signals(50, &[(1350.0, 1650.0)])]);
        let off = pick(&occ, &[], FT8_BW);
        let lane_lo = off;
        let lane_hi = off + FT8_BW;
        assert!(
            lane_hi <= 1350.0 || lane_lo >= 1650.0,
            "lane [{lane_lo},{lane_hi}] should clear the busy 1350..1650 block"
        );
    }

    #[test]
    fn among_clear_lanes_prefers_center() {
        // Two signals leave a clear gap around center and clear space at the edges;
        // the center bias should win the tie near 1500.
        let occ = occ_from(&[floor_with_signals(50, &[(600.0, 700.0), (2300.0, 2400.0)])]);
        let off = pick(&occ, &[], FT8_BW);
        let center = off + FT8_BW / 2.0;
        assert!(
            (center - CENTER_HZ).abs() < 150.0,
            "expected a near-center pick, got center {center}"
        );
    }

    #[test]
    fn routes_around_a_recent_decode_on_center() {
        // Spectrum is clear, but a station was just decoded right at center (idle
        // between overs). The picker should step away from it.
        let occ = occ_from(&[floor_with_signals(50, &[])]);
        let decode_at_center = CENTER_HZ - FT8_BW / 2.0;
        let off = pick(&occ, &[decode_at_center], FT8_BW);
        let gap = ((off - (decode_at_center + FT8_BW)).max(decode_at_center - (off + FT8_BW))).max(0.0);
        assert!(gap > 0.0, "pick {off} should not overlap the decode at {decode_at_center}");
    }

    #[test]
    fn pick_stays_in_usable_window() {
        let occ = occ_from(&[floor_with_signals(50, &[(1350.0, 1650.0)])]);
        let off = pick(&occ, &[], FT8_BW);
        assert!(off >= USABLE_LO_HZ && off + FT8_BW <= USABLE_HI_HZ);
    }

    #[test]
    fn recency_weights_newer_rows() {
        // An old row says center is busy; a fresh row says it's clear. The fresh
        // row should dominate, leaving center pickable.
        let mut old = floor_with_signals(50, &[(1400.0, 1600.0)]);
        old.t = Timestamp(0);
        let mut fresh = floor_with_signals(50, &[]);
        fresh.t = Timestamp(60_000); // 60 s newer ≫ 8 s half-life
        let occ = occ_from(&[old, fresh]);
        let (peak, _) = occ.region(1450.0, 1550.0);
        assert!(peak < 20.0, "stale signal should have decayed, peak {peak}");
    }
}
