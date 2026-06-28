//! Candidate sync search, soft-symbol demodulation, and the top-level decode
//! (port of ft8_lib `decode.c` + the `decode_ft8.c` driver loop). Handles FT8
//! and FT4. Output is a list of [`Decode`]s per slot.

use crate::ap;
use crate::cohere;
use crate::cohere_ft4;
use crate::constants::{FT4_COSTAS, FT4_GRAY, FT8_COSTAS, FT8_GRAY};
use crate::crc;
use crate::ldpc::{N, bp_decode};
use crate::message::{self, CallHash, MessageType};
use crate::osd;
use crate::subtract;
use std::sync::OnceLock;
use crate::waterfall::{Monitor, Protocol, Waterfall, mag_db};
use std::collections::HashSet;

const MIN_SCORE: i32 = 10;
const MAX_CANDIDATES: usize = 140;

/// Minimum sync score, overridable for diagnostics via `DM420_MIN_SCORE`.
fn min_score() -> i32 {
    static N: OnceLock<i32> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("DM420_MIN_SCORE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(MIN_SCORE)
    })
}

/// Candidate cap, overridable for diagnostics via `DM420_MAX_CANDIDATES`.
fn max_candidates() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("DM420_MAX_CANDIDATES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n >= 1)
            .unwrap_or(MAX_CANDIDATES)
    })
}
const LDPC_ITERS: usize = 25;
/// Run the OSD backstop only when belief-propagation got within this many parity
/// errors (out of 83). Real near-misses leave a handful; pure noise plateaus much
/// higher, so this skips OSD on hopeless candidates without costing recall.
const OSD_MAX_ERRORS: i32 = 40;
/// FT8 decode passes with subtraction between them (1 = subtraction disabled).
const SUBTRACT_PASSES: usize = 3;
const TIME_OSR: usize = 2;
const FREQ_OSR: usize = 2;

// Sync geometry (Costas block count / length / stride), the data-symbol count,
// and the total channel-symbol count now live on `Protocol` (see `waterfall.rs`)
// so encode and decode read them from one place.

/// One decoded transmission within a slot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Decode {
    /// Relative signal strength (Costas sync score; higher = stronger).
    pub score: i32,
    /// Estimated SNR in dB, referenced to a 2500 Hz noise bandwidth (WSJT-X
    /// convention): signal power at the Costas sync tones vs. the neighbouring
    /// noise bins. A power ratio, so it is independent of the input gain.
    pub snr_db: f32,
    /// Time offset of the transmission from the start of the analyzed audio (s).
    pub dt: f32,
    /// Audio (baseband) frequency of the transmission (Hz).
    pub freq_hz: f32,
    /// Decoded message text.
    pub message: String,
    /// Message category.
    pub msg_type: MessageType,
}

impl Decode {
    /// True if this is a CQ call (a station soliciting contacts).
    pub fn is_cq(&self) -> bool {
        self.message.starts_with("CQ ") || self.message == "CQ"
    }

    /// The calling station (the "DE" callsign) — second token for CQ/standard
    /// messages. Best-effort for display.
    pub fn caller(&self) -> Option<&str> {
        let mut it = self.message.split_whitespace();
        if self.is_cq() {
            // CQ [MOD] CALL [GRID] — caller is the token before the grid, but for
            // plain "CQ CALL ..." it's the 2nd token.
            let toks: Vec<&str> = self.message.split_whitespace().collect();
            // skip "CQ" and an optional modifier (DX / 3 digits / letters)
            let idx = if toks.len() >= 3 && is_cq_modifier(toks[1]) {
                2
            } else {
                1
            };
            toks.get(idx).copied()
        } else {
            it.nth(1)
        }
    }
}

fn is_cq_modifier(tok: &str) -> bool {
    tok == "DX"
        || (tok.len() == 3 && tok.bytes().all(|b| b.is_ascii_digit()))
        || (!tok.is_empty() && tok.len() <= 4 && tok.bytes().all(|b| b.is_ascii_uppercase()))
}

#[derive(Clone, Copy)]
struct Candidate {
    score: i32,
    time_offset: i32,
    freq_offset: i32,
    time_sub: usize,
    freq_sub: usize,
}

/// Raw magnitude (u8) for a candidate at absolute block `block_abs` and tone
/// index `tone` (0..num_tones). Caller guarantees block_abs is in range and
/// freq_offset+tone < num_bins.
#[inline]
fn mag_at(wf: &Waterfall, c: &Candidate, block_abs: i32, tone: i32) -> u8 {
    let idx = block_abs as usize * wf.block_stride
        + (c.time_sub * wf.freq_osr + c.freq_sub) * wf.num_bins
        + (c.freq_offset + tone) as usize;
    wf.mag[idx]
}

// The loop index also drives block/time math, so the range loops are intentional.
#[allow(clippy::needless_range_loop)]
fn ft8_sync_score(wf: &Waterfall, c: &Candidate) -> i32 {
    let num_sync = wf.protocol.num_sync();
    let length_sync = wf.protocol.length_sync();
    let sync_offset = wf.protocol.sync_offset();
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..num_sync {
        for k in 0..length_sync {
            let block = (sync_offset * m + k) as i32;
            let block_abs = c.time_offset + block;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= nb {
                break;
            }
            let sm = FT8_COSTAS[k] as i32;
            let here = mag_at(wf, c, block_abs, sm) as i32;
            if sm > 0 {
                score += here - mag_at(wf, c, block_abs, sm - 1) as i32;
                num += 1;
            }
            if sm < 7 {
                score += here - mag_at(wf, c, block_abs, sm + 1) as i32;
                num += 1;
            }
            if k > 0 && block_abs > 0 {
                score += here - mag_at(wf, c, block_abs - 1, sm) as i32;
                num += 1;
            }
            if (k + 1) < length_sync && (block_abs + 1) < nb {
                score += here - mag_at(wf, c, block_abs + 1, sm) as i32;
                num += 1;
            }
        }
    }
    if num > 0 { score / num } else { 0 }
}

#[allow(clippy::needless_range_loop)]
fn ft4_sync_score(wf: &Waterfall, c: &Candidate) -> i32 {
    let num_sync = wf.protocol.num_sync();
    let length_sync = wf.protocol.length_sync();
    let sync_offset = wf.protocol.sync_offset();
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..num_sync {
        for k in 0..length_sync {
            let block = (1 + sync_offset * m + k) as i32;
            let block_abs = c.time_offset + block;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= nb {
                break;
            }
            let sm = FT4_COSTAS[m][k] as i32;
            let here = mag_at(wf, c, block_abs, sm) as i32;
            if sm > 0 {
                score += here - mag_at(wf, c, block_abs, sm - 1) as i32;
                num += 1;
            }
            if sm < 3 {
                score += here - mag_at(wf, c, block_abs, sm + 1) as i32;
                num += 1;
            }
            if k > 0 && block_abs > 0 {
                score += here - mag_at(wf, c, block_abs - 1, sm) as i32;
                num += 1;
            }
            if (k + 1) < length_sync && (block_abs + 1) < nb {
                score += here - mag_at(wf, c, block_abs + 1, sm) as i32;
                num += 1;
            }
        }
    }
    if num > 0 { score / num } else { 0 }
}

/// The per-slot noise floor as linear power per analysis bin: the median of every
/// stored magnitude over the filled blocks. FT8/FT4 signals occupy only a small
/// fraction of the time–frequency bins, so the median tracks the noise, not the
/// signals — and reading a global floor (rather than the tone bins beside a strong
/// signal) avoids the spectral-leakage contamination that makes loud signals
/// under-report.
fn noise_floor(wf: &Waterfall) -> f64 {
    let mags = &wf.mag[..wf.num_blocks * wf.block_stride];
    if mags.is_empty() {
        return 1e-12;
    }
    let mut hist = [0u32; 256];
    for &v in mags {
        hist[v as usize] += 1;
    }
    let target = mags.len() as u32 / 2; // median bin
    let mut acc = 0u32;
    let mut med = 0u8;
    for (v, &count) in hist.iter().enumerate() {
        acc += count;
        if acc > target {
            med = v as u8;
            break;
        }
    }
    10f64.powf(mag_db(med) as f64 / 10.0)
}

/// Per-frequency noise floor (in stored u8 magnitude units), one value per
/// oversampled frequency column `freq_sub * num_bins + bin`. For each column we
/// take a low percentile (≈30th) of the magnitude over every time block and
/// time-sub, so the value tracks the *noise in that frequency lane* rather than the
/// signals that occupy it only briefly. This is the per-frequency analog of
/// [`noise_floor`] and the core of the crowded-band candidate-finder fix: it lets
/// [`ft4_sync_score_baseline`] reference each Costas tone to the noise in its own
/// lane (WSJT-X `getcandidates4` / `ft4_baseline` style) instead of to adjacent
/// bins which, on a crowded band, hold *other signals* and collapse the contrast.
fn per_freq_floor(wf: &Waterfall) -> Vec<u8> {
    let ncols = wf.freq_osr * wf.num_bins;
    let mut floor = vec![0u8; ncols];
    if wf.num_blocks == 0 {
        return floor;
    }
    let mut hist = [0u32; 256];
    for (col, floor_val) in floor.iter_mut().enumerate() {
        let freq_sub = col / wf.num_bins;
        let bin = col % wf.num_bins;
        hist.iter_mut().for_each(|h| *h = 0);
        let mut count = 0u32;
        for block in 0..wf.num_blocks {
            for time_sub in 0..wf.time_osr {
                let idx = block * wf.block_stride
                    + (time_sub * wf.freq_osr + freq_sub) * wf.num_bins
                    + bin;
                hist[wf.mag[idx] as usize] += 1;
                count += 1;
            }
        }
        let target = count * 3 / 10; // 30th percentile of this lane's magnitudes
        let mut acc = 0u32;
        for (v, &h) in hist.iter().enumerate() {
            acc += h;
            if acc > target {
                *floor_val = v as u8;
                break;
            }
        }
    }
    floor
}

/// FT4 sync score referenced to the per-frequency noise floor instead of to
/// adjacent bins. `floor` comes from [`per_freq_floor`]. For each Costas sync tone
/// we add how far its magnitude rises *above the noise in its own lane*, averaged
/// over all 16 sync tones. Unlike [`ft4_sync_score`] (which subtracts neighboring
/// time/freq bins), a strong signal packed beside other signals still scores high —
/// its Costas tones sit well above the lane noise regardless of the neighbors — so
/// crowded-band strong signals get nominated instead of scoring ~0. This also
/// sidesteps the u8 magnitude saturation (a tone pegged at 255 still scores high
/// against a floor well below 255), so the shared `mag` scale needs no change.
#[allow(clippy::needless_range_loop)]
fn ft4_sync_score_baseline(wf: &Waterfall, c: &Candidate, floor: &[u8]) -> i32 {
    let num_sync = wf.protocol.num_sync();
    let length_sync = wf.protocol.length_sync();
    let sync_offset = wf.protocol.sync_offset();
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..num_sync {
        for k in 0..length_sync {
            let block = (1 + sync_offset * m + k) as i32;
            let block_abs = c.time_offset + block;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= nb {
                break;
            }
            let sm = FT4_COSTAS[m][k] as i32;
            let here = mag_at(wf, c, block_abs, sm) as i32;
            let col = c.freq_sub * wf.num_bins + (c.freq_offset + sm) as usize;
            score += here - floor[col] as i32;
            num += 1;
        }
    }
    if num > 0 { score / num } else { 0 }
}

/// FT8 sibling of [`ft4_sync_score_baseline`]: score each FT8 Costas tone against
/// the per-frequency noise floor instead of its adjacent bins. Same rationale —
/// the `wsjtx_ft8` corpus is also a busy 20m band where neighbor bins hold other
/// signals. Whether this is a net win for FT8 is decided by measurement (see
/// `FT8_BASELINE_DEFAULT`); the floor backstop is 787/16% on `sample_data/wsjtx_ft8`.
#[allow(clippy::needless_range_loop)]
fn ft8_sync_score_baseline(wf: &Waterfall, c: &Candidate, floor: &[u8]) -> i32 {
    let num_sync = wf.protocol.num_sync();
    let length_sync = wf.protocol.length_sync();
    let sync_offset = wf.protocol.sync_offset();
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..num_sync {
        for k in 0..length_sync {
            let block = (sync_offset * m + k) as i32;
            let block_abs = c.time_offset + block;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= nb {
                break;
            }
            let sm = FT8_COSTAS[k] as i32;
            let here = mag_at(wf, c, block_abs, sm) as i32;
            let col = c.freq_sub * wf.num_bins + (c.freq_offset + sm) as usize;
            score += here - floor[col] as i32;
            num += 1;
        }
    }
    if num > 0 { score / num } else { 0 }
}

/// Estimate SNR in dB (≈2500 Hz reference, WSJT-X convention) for a candidate.
///
/// Takes the signal power at the known Costas **sync tones**, subtracts the
/// per-slot `noise` floor to get the pure signal, and corrects the ratio from the
/// per-bin analysis bandwidth to the 2500 Hz reference. Because it is a power
/// *ratio* it is independent of the input gain — a strong signal reads high and one
/// at the decode limit lands near −21 dB regardless of how hot the audio is driven
/// (which the old `score / 2` placeholder did not).
// The loop indices drive the block/time math and the Costas lookup, so the range
// loops are intentional (matching `ft8_sync_score`).
#[allow(clippy::needless_range_loop)]
fn estimate_snr(wf: &Waterfall, c: &Candidate, noise: f64) -> f32 {
    if noise <= 0.0 {
        return 49.0;
    }
    let nb = wf.num_blocks as i32;
    let num_sync = wf.protocol.num_sync();
    let len_sync = wf.protocol.length_sync();
    let sync_offset = wf.protocol.sync_offset();

    // Linear power recovered from the stored u8 magnitude (dB → linear).
    let lin = |v: u8| 10f64.powf(mag_db(v) as f64 / 10.0);

    let mut sig_sum = 0.0f64; // signal+noise power at the sync tones
    let mut sig_n = 0u32;
    for m in 0..num_sync {
        for k in 0..len_sync {
            // FT4's sync starts one symbol into the slot (see `ft4_sync_score`).
            let block = if wf.protocol == Protocol::Ft4 {
                (1 + sync_offset * m + k) as i32
            } else {
                (sync_offset * m + k) as i32
            };
            let block_abs = c.time_offset + block;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= nb {
                break;
            }
            let costas = if wf.protocol == Protocol::Ft4 {
                FT4_COSTAS[m][k] as i32
            } else {
                FT8_COSTAS[k] as i32
            };
            sig_sum += lin(mag_at(wf, c, block_abs, costas));
            sig_n += 1;
        }
    }
    if sig_n == 0 {
        return -24.0; // no sync coverage in range — report the decode floor
    }

    // Pure signal power (subtract the noise carried in the sync-tone bin), floored
    // so a sync tone at/under the noise still yields a finite dB.
    let s_plus_n = sig_sum / sig_n as f64;
    let signal = (s_plus_n - noise).max(noise * 1e-3);
    // Per-bin analysis bandwidth → 2500 Hz reference: -10*log10(2500 / bin_hz).
    let bin_hz = 1.0 / (wf.symbol_period * wf.freq_osr as f32);
    let correction = -10.0 * (2500.0 / bin_hz as f64).log10();
    let snr_db = 10.0 * (signal / noise).log10() + correction;
    (snr_db as f32).clamp(-28.0, 49.0)
}

/// Minimum noise-relative score for an additive "rescue" candidate, tunable via
/// `DM420_RESCUE_SCORE`. The rescue pass is purely additive and CRC-gated, so this
/// only trades decode work against how weak a crowded-masked signal it will try.
const RESCUE_MIN_SCORE: i32 = 12;
fn rescue_min_score() -> i32 {
    static N: OnceLock<i32> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("DM420_RESCUE_SCORE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RESCUE_MIN_SCORE)
    })
}

/// Append noise-relative "rescue" candidates (already sorted strongest-first) onto
/// the primary list, skipping any within (`FREQ_TOL`, `TIME_TOL`) of a candidate
/// already present — collapsing each rescued signal's grid-point smear to one
/// representative and avoiding redundant re-decode of what the primary finder
/// already nominated. Adds at most `budget`. Tolerances are oversampled (half-bin /
/// half-block) units; a frequency lane holds ≤1 signal per slot, so the time
/// tolerance can be loose while the freq tolerance stays under the closest real
/// signal spacing. Purely additive: it never reorders or drops a primary candidate.
fn append_rescue(cands: &mut Vec<Candidate>, extra: &[Candidate], wf: &Waterfall, budget: usize) {
    const FREQ_TOL: i32 = 2; // half-bins (≈ one base bin)
    const TIME_TOL: i32 = 8; // half-blocks
    let fkey = |c: &Candidate| c.freq_offset * wf.freq_osr as i32 + c.freq_sub as i32;
    let tkey = |c: &Candidate| c.time_offset * wf.time_osr as i32 + c.time_sub as i32;
    // Collapse each rescued signal's grid-point smear, but dedup ONLY among the
    // rescue extras — NOT against the primary list. A strong crowded miss often sits
    // a bin away from an unrelated primary candidate; checking against primary would
    // wrongly drop exactly the signal we're trying to rescue. Primary/extra payload
    // overlaps are harmless (the downstream `seen` set skips the re-decode).
    let added_from = cands.len();
    for c in extra {
        if cands.len() - added_from >= budget {
            break;
        }
        let (fk, tk) = (fkey(c), tkey(c));
        let clash = cands[added_from..]
            .iter()
            .any(|k| (fk - fkey(k)).abs() <= FREQ_TOL && (tk - tkey(k)).abs() <= TIME_TOL);
        if !clash {
            cands.push(*c);
        }
    }
}

fn find_candidates(wf: &Waterfall) -> Vec<Candidate> {
    let num_tones = wf.protocol.num_tones() as i32;
    // Costas sync search spans the whole slot, not just a small offset window:
    // recordings/generated files aren't always tight to the slot start (FT4 in
    // particular sits ~25 symbols in), so bound the upper end by where the last
    // data symbol still fits rather than the reference's fixed +20.
    let total_symbols = wf.protocol.channel_symbols() as i32;
    let upper = (wf.num_blocks as i32 - total_symbols + 11).max(20);
    let freq_hi = wf.num_bins as i32 - num_tones + 1;

    // Scan the full (time-sub × freq-sub × time-offset × freq-offset) grid, scoring
    // each point with `score` and keeping those at/above `thresh`.
    let scan = |score: &dyn Fn(&Candidate) -> i32, thresh: i32| -> Vec<Candidate> {
        let mut v = Vec::new();
        for time_sub in 0..wf.time_osr {
            for freq_sub in 0..wf.freq_osr {
                for time_offset in -10..upper {
                    for freq_offset in 0..freq_hi {
                        let mut c = Candidate { score: 0, time_offset, freq_offset, time_sub, freq_sub };
                        c.score = score(&c);
                        if c.score >= thresh {
                            v.push(c);
                        }
                    }
                }
            }
        }
        v
    };

    // Primary finder: the original neighbor-contrast Costas score (ft8_lib heap),
    // left exactly as-is — it is sharp/peaky and recovers the bulk of every corpus,
    // so keeping it untouched is what guarantees FT8 and sparse-FT4 cannot regress.
    let mut cands = if wf.protocol == Protocol::Ft4 {
        scan(&|c| ft4_sync_score(wf, c), min_score())
    } else {
        scan(&|c| ft8_sync_score(wf, c), min_score())
    };
    cands.sort_by_key(|c| std::cmp::Reverse(c.score)); // strongest first
    cands.truncate(max_candidates());

    // Additive crowded-band rescue: a strong signal packed beside other signals
    // scores ~0 on neighbor-contrast (its neighbor bins hold those other signals, and
    // the u8 mag saturates), so it is never nominated above. Re-score every grid point
    // against the per-frequency noise floor (`per_freq_floor`), keep the strong peaks,
    // then APPEND those the primary list doesn't already cover (`append_rescue`).
    // Every decode is CRC-gated downstream, so this only ADDS decodes — it never drops
    // or reorders a primary candidate, which is what keeps FT8/sparse-FT4 safe.
    // Per-mode default in `baseline_enabled`; `DM420_BASELINE=0` disables it.
    if baseline_enabled(wf.protocol) {
        let floor = per_freq_floor(wf);
        let mut extra = if wf.protocol == Protocol::Ft4 {
            scan(&|c| ft4_sync_score_baseline(wf, c, &floor), rescue_min_score())
        } else {
            scan(&|c| ft8_sync_score_baseline(wf, c, &floor), rescue_min_score())
        };
        extra.sort_by_key(|c| std::cmp::Reverse(c.score));
        append_rescue(&mut cands, &extra, wf, max_candidates());
    }

    if std::env::var("DM420_CAND_STATS").is_ok() {
        eprintln!(
            "cand-stats: {} candidates ({})",
            cands.len(),
            if baseline_enabled(wf.protocol) { "legacy + baseline-rescue" } else { "legacy only" },
        );
    }
    cands
}

#[inline]
fn max2(a: f32, b: f32) -> f32 {
    if a >= b { a } else { b }
}
#[inline]
fn max4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    max2(max2(a, b), max2(c, d))
}

fn extract_llr(wf: &Waterfall, c: &Candidate) -> [f32; N] {
    let mut log174 = [0.0f32; N];
    let nb = wf.num_blocks as i32;
    if wf.protocol == Protocol::Ft4 {
        for k in 0..wf.protocol.data_symbols() {
            let sync = if k < 29 {
                5
            } else if k < 58 {
                9
            } else {
                13
            };
            let sym_idx = (k + sync) as i32;
            let bit = 2 * k;
            let block_abs = c.time_offset + sym_idx;
            if block_abs < 0 || block_abs >= nb {
                continue;
            }
            let s: [f32; 4] =
                std::array::from_fn(|j| mag_db(mag_at(wf, c, block_abs, FT4_GRAY[j] as i32)));
            log174[bit] = max2(s[2], s[3]) - max2(s[0], s[1]);
            log174[bit + 1] = max2(s[1], s[3]) - max2(s[0], s[2]);
        }
    } else {
        for k in 0..wf.protocol.data_symbols() {
            let sync = if k < 29 { 7 } else { 14 };
            let sym_idx = (k + sync) as i32;
            let bit = 3 * k;
            let block_abs = c.time_offset + sym_idx;
            if block_abs < 0 || block_abs >= nb {
                continue;
            }
            let s: [f32; 8] =
                std::array::from_fn(|j| mag_db(mag_at(wf, c, block_abs, FT8_GRAY[j] as i32)));
            log174[bit] = max4(s[4], s[5], s[6], s[7]) - max4(s[0], s[1], s[2], s[3]);
            log174[bit + 1] = max4(s[2], s[3], s[6], s[7]) - max4(s[0], s[1], s[4], s[5]);
            log174[bit + 2] = max4(s[1], s[3], s[5], s[7]) - max4(s[0], s[2], s[4], s[6]);
        }
    }
    log174
}

pub(crate) fn normalize_llr(log174: &mut [f32; N]) {
    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    for &v in log174.iter() {
        sum += v;
        sum2 += v * v;
    }
    let inv_n = 1.0 / N as f32;
    let variance = (sum2 - sum * sum * inv_n) * inv_n;
    if variance <= 0.0 {
        return;
    }
    let norm = (24.0 / variance).sqrt();
    for v in log174.iter_mut() {
        *v *= norm;
    }
}

fn pack_bits(plain: &[u8; N]) -> [u8; 12] {
    let mut packed = [0u8; 12];
    for i in 0..crate::ldpc::K {
        if plain[i] != 0 {
            packed[i / 8] |= 0x80 >> (i % 8);
        }
    }
    packed
}

fn decode_candidate(wf: &Waterfall, c: &Candidate) -> Option<[u8; 10]> {
    let mut log174 = extract_llr(wf, c);
    normalize_llr(&mut log174);
    let (plain, errors) = bp_decode(&log174, LDPC_ITERS);
    if errors == 0 {
        if let Some(p) = verify_codeword(wf.protocol, &plain) {
            return Some(p);
        }
    } else if osd_enabled() && errors <= OSD_MAX_ERRORS {
        // Belief-propagation stalled with a few residual parity errors — a
        // near-miss OSD can often finish. Each candidate codeword is parity-valid
        // by construction, so CRC is what separates a real decode from a wrong one.
        for cand in osd::osd_decode(&log174) {
            if let Some(p) = verify_codeword(wf.protocol, &cand) {
                return Some(p);
            }
        }
    }
    // A-priori fallback: blind (BP + OSD) missed — retry CRC-gated hypotheses.
    if ap::ap_enabled() {
        if let Some(p) = ap::try_ap(&log174, wf.protocol) {
            return Some(p);
        }
    }
    None
}

/// Whether the OSD backstop runs (default on). `DM420_OSD=0` disables it — an
/// escape hatch and the A/B switch for measuring OSD's contribution.
fn osd_enabled() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| std::env::var("DM420_OSD").map(|v| v != "0").unwrap_or(true))
}

/// Whether multi-pass subtraction runs (default on). `DM420_SUBTRACT=0` disables
/// it — escape hatch and the A/B switch for measuring its contribution.
fn subtract_enabled() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| std::env::var("DM420_SUBTRACT").map(|v| v != "0").unwrap_or(true))
}

/// Per-mode default for the noise-relative candidate scorer + local-max dedup
/// (the crowded-band candidate-finder fix). FT4 wins decisively, so it's on by
/// default; FT8's default is set by measurement on `sample_data/wsjtx_ft8` (see
/// the A/B table in the handoff) — flip this if a future FT8 corpus changes the
/// verdict. `DM420_BASELINE` overrides both modes for testing.
const FT8_BASELINE_DEFAULT: bool = true;
const FT4_BASELINE_DEFAULT: bool = true;

/// Explicit `DM420_BASELINE` override, if set: `0` forces the legacy
/// neighbor-contrast scorer for *both* modes, anything else forces the new scorer.
/// `None` (unset) → use the per-mode compiled default above.
fn baseline_override() -> Option<bool> {
    static O: OnceLock<Option<bool>> = OnceLock::new();
    *O.get_or_init(|| std::env::var("DM420_BASELINE").ok().map(|v| v != "0"))
}

/// Whether the noise-relative candidate scorer (`*_sync_score_baseline`) + dedup
/// run for this protocol. `DM420_BASELINE=0` reverts to the legacy
/// `ft8_sync_score`/`ft4_sync_score` for both modes — the A/B switch for the
/// crowded-band fix and for testing FT8 each way.
fn baseline_enabled(protocol: Protocol) -> bool {
    baseline_override().unwrap_or(match protocol {
        Protocol::Ft4 => FT4_BASELINE_DEFAULT,
        Protocol::Ft8 => FT8_BASELINE_DEFAULT,
    })
}

/// Whether the coherent front-end runs (default on). `DM420_COHERENT=0` falls
/// back to the magnitude-only path — the A/B switch for measuring the coherent
/// demod's contribution against the magnitude baseline on the same corpus.
fn coherent_enabled() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| std::env::var("DM420_COHERENT").map(|v| v != "0").unwrap_or(true))
}

/// Number of subtraction passes. `DM420_SUBTRACT_PASSES` overrides the default
/// for tuning; the loop stops early once a pass finds nothing new.
fn subtract_passes() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("DM420_SUBTRACT_PASSES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n >= 1)
            .unwrap_or(SUBTRACT_PASSES)
    })
}

/// Validate a hard-decision codeword: check the CRC-14 and, on success, return
/// the 10-byte payload (un-whitening FT4). Returns `None` on CRC mismatch.
pub(crate) fn verify_codeword(protocol: Protocol, plain: &[u8; N]) -> Option<[u8; 10]> {
    let a91 = pack_bits(plain);
    let crc_extracted = crc::extract_crc(&a91);
    let mut chk = a91;
    chk[9] &= 0xF8;
    chk[10] = 0;
    let crc_calculated = crc::compute_crc(&chk, 96 - 14);
    if crc_extracted != crc_calculated {
        return None;
    }
    let mut payload = [0u8; 10];
    payload.copy_from_slice(&a91[..10]);
    if let Some(xor) = protocol.whitening() {
        for (b, x) in payload.iter_mut().zip(xor.iter()) {
            *b ^= *x;
        }
    }
    Some(payload)
}

/// Decode a slot of mono audio at `sample_rate` (12 kHz expected) and return the
/// transmissions found, strongest first.
pub fn decode(samples: &[f32], sample_rate: u32, protocol: Protocol) -> Vec<Decode> {
    // Collect the streamed decodes; order is strongest-first (candidate score).
    // The call site re-sorts low-to-high in frequency if it wants that.
    let mut out = Vec::new();
    // One-shot decode: a fresh hash table lives only for this slot's passes.
    let mut hash = CallHash::new();
    decode_streaming(samples, sample_rate, protocol, &mut hash, |d| out.push(d));
    out
}

/// Like [`decode`], but hand each transmission to `on_decode` the instant it is
/// found (strongest first) rather than collecting them into a `Vec`. This lets the
/// live pipeline publish each decode onto the bus as it lands, so the UI and the
/// QSO engine see the strongest signals (e.g. a CQ being answered) first — a beat
/// before the whole slot's batch would have finished — instead of all at once.
/// Per-mode coherent demodulator, selected once per slot. FT8 and FT4 have
/// different geometry and bit-metric steps, so each owns its own `Demod`.
enum CoherentDemod {
    Ft8(cohere::Demod),
    Ft4(cohere_ft4::Demod),
}

pub fn decode_streaming(
    samples: &[f32],
    sample_rate: u32,
    protocol: Protocol,
    hash: &mut CallHash,
    mut on_decode: impl FnMut(Decode),
) {
    // Multi-pass subtraction (FT8 and FT4): decode a pass, subtract every decode's
    // waveform from the audio, rebuild the waterfall on the residual, and decode
    // again — recovering signals that a louder neighbor was masking. The residual
    // carries across passes, so each signal is subtracted once.
    let passes = if subtract_enabled() {
        subtract_passes()
    } else {
        1
    };
    let mut residual = samples.to_vec();
    let mut seen: HashSet<[u8; 10]> = HashSet::new();
    for pass in 0..passes {
        let mut mon = Monitor::new(sample_rate, protocol, TIME_OSR, FREQ_OSR, 200.0, 3000.0);
        let bs = mon.block_size;
        let mut pos = 0;
        while pos + bs <= residual.len() {
            mon.process(&residual[pos..pos + bs]);
            pos += bs;
        }

        let wf = &mon.wf;
        let noise = noise_floor(wf);
        let cands = find_candidates(wf); // already sorted strongest-first by score
        let sp = wf.symbol_period;

        // Both modes get a coherent front-end: per-candidate complex baseband + fine
        // sync + multi-symbol coherent demod, rebuilt on each pass's residual so
        // coherent demod and subtraction compose — every pass decodes a cleaner
        // residual coherently, and the refined freq/dt feed the next subtract.
        // Coherent is tried first with the magnitude path as a fallback, so it can
        // only add decodes, never regress.
        let mut demod = coherent_enabled().then(|| match protocol {
            Protocol::Ft8 => {
                let mut d = cohere::Demod::new();
                d.set_slot(&residual);
                CoherentDemod::Ft8(d)
            }
            Protocol::Ft4 => {
                let mut d = cohere_ft4::Demod::new();
                d.set_slot(&residual);
                CoherentDemod::Ft4(d)
            }
        });

        // (payload, freq, dt) for each signal newly decoded this pass — used to
        // subtract them before the next pass.
        let mut decoded: Vec<([u8; 10], f32, f32)> = Vec::new();
        for c in &cands {
            let coarse_freq = (wf.min_bin as f32
                + c.freq_offset as f32
                + c.freq_sub as f32 / wf.freq_osr as f32)
                / sp;
            let coarse_dt = (c.time_offset as f32 + c.time_sub as f32 / wf.time_osr as f32) * sp;

            // Coherent path first; magnitude path as a safety fallback so the
            // coherent front-end can only add decodes, never regress.
            let dec = demod
                .as_mut()
                .and_then(|d| match d {
                    CoherentDemod::Ft8(d) => decode_candidate_coherent(d, wf, c),
                    CoherentDemod::Ft4(d) => decode_candidate_coherent_ft4(d, wf, c),
                })
                .or_else(|| decode_candidate(wf, c).map(|p| (p, coarse_freq, coarse_dt)));
            let Some((payload, freq_hz, dt)) = dec else {
                continue;
            };
            if !seen.insert(payload) {
                continue; // already found (and subtracted) — don't emit or re-subtract
            }
            decoded.push((payload, freq_hz, dt));
            if let Some((text, msg_type)) = message::decode(&payload, hash) {
                on_decode(Decode {
                    score: c.score,
                    snr_db: estimate_snr(wf, c, noise),
                    dt,
                    freq_hz,
                    message: text,
                    msg_type,
                });
            }
        }

        // Nothing new, or this was the last pass: stop before subtracting.
        if decoded.is_empty() || pass + 1 == passes {
            break;
        }
        for (payload, f0, dt) in &decoded {
            match protocol {
                Protocol::Ft8 => subtract::subtract_ft8(&mut residual, payload, *f0, *dt, sample_rate),
                Protocol::Ft4 => subtract::subtract_ft4(&mut residual, payload, *f0, *dt, sample_rate),
            }
        }
    }
}

/// Coherent decode of one candidate (FT8): analyze to LLR variants, then try BP +
/// OSD + CRC on each variant in WSJT-X pass order, returning the first valid
/// payload along with the coherently-refined frequency and time offset.
fn decode_candidate_coherent(
    demod: &mut cohere::Demod,
    wf: &Waterfall,
    c: &Candidate,
) -> Option<([u8; 10], f32, f32)> {
    let sp = wf.symbol_period;
    let f0 =
        (wf.min_bin as f32 + c.freq_offset as f32 + c.freq_sub as f32 / wf.freq_osr as f32) / sp;
    // Downsampled start guess: 32 samples/symbol at 200 Hz (= NSPS/NDOWN). The
    // magnitude waterfall's candidate start runs ~one symbol high relative to the
    // coherent demod's baseband origin (its analysis frame is 2 symbols wide:
    // nfft = block_size·FREQ_OSR = 2·NSPS, Hann-windowed), so subtract one symbol
    // before fine sync. Measured via the DM420_TOFF sweep on the 24-slot real set:
    // a flat optimum plateau across −40..−20 downsampled samples, centered at −32 ≡
    // one symbol; the naive half-window geometric estimate (−16) undershoots by 2×
    // and loses ~13 decodes.
    const SPS2: f32 = 32.0; // downsampled samples per symbol
    let i0 = (c.time_offset as f32 + c.time_sub as f32 / wf.time_osr as f32) * SPS2 - SPS2;
    let an = demod.analyze(f0, i0)?;
    // Try all five LLR variants (nsym=1/2/3 coherent integration) in WSJT-X pass order.
    for llr in an.llrs.iter() {
        let (plain, errors) = bp_decode(llr, LDPC_ITERS);
        if errors == 0 {
            if let Some(p) = verify_codeword(wf.protocol, &plain) {
                return Some((p, an.freq_hz, an.dt));
            }
        } else if osd_enabled() && errors <= OSD_MAX_ERRORS {
            for cand in osd::osd_decode(llr) {
                if let Some(p) = verify_codeword(wf.protocol, &cand) {
                    return Some((p, an.freq_hz, an.dt));
                }
            }
        }
    }
    // A-priori fallback on the first (full-integration) LLR variant.
    if ap::ap_enabled() {
        if let Some(llr) = an.llrs.first() {
            if let Some(p) = ap::try_ap(llr, wf.protocol) {
                return Some((p, an.freq_hz, an.dt));
            }
        }
    }
    None
}

/// Empirical FT4 start-sample correction (downsampled samples) added to the
/// magnitude candidate's start guess before fine sync. FT4's start convention
/// (a ramp symbol at index 0, the 0.5 s slot offset, NSPS=576) differs from
/// FT8's, so this is measured via a `DM420_FT4_TOFF` sweep rather than derived —
/// see the FT8 gotcha in `docs/decoder_ft4_coherent_handoff.md`. Sweep on the
/// 14-slot real FT4 set (`sample_data/wsjtx_ft4`): a flat optimum across −16..−12
/// downsampled samples (matched 80, gap 18%), falling off either side; the baked
/// default −14 is the plateau center. Override via the env var to re-sweep.
fn ft4_toff() -> f32 {
    static T: OnceLock<f32> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("DM420_FT4_TOFF")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-14.0)
    })
}

/// Coherent decode of one candidate (FT4): the FT4 sibling of
/// [`decode_candidate_coherent`]. Analyze to LLR variants, then BP + OSD + CRC on
/// each in WSJT-X pass order, returning the first valid payload with the refined
/// frequency and time offset.
fn decode_candidate_coherent_ft4(
    demod: &mut cohere_ft4::Demod,
    wf: &Waterfall,
    c: &Candidate,
) -> Option<([u8; 10], f32, f32)> {
    let sp = wf.symbol_period;
    let f0 =
        (wf.min_bin as f32 + c.freq_offset as f32 + c.freq_sub as f32 / wf.freq_osr as f32) / sp;
    // 32 downsampled samples/symbol (NSPS/NDOWN = 576/18), the same as FT8. The
    // start-convention offset is measured empirically, not derived (see ft4_toff).
    const SPS2: f32 = 32.0;
    let i0 = (c.time_offset as f32 + c.time_sub as f32 / wf.time_osr as f32) * SPS2 + ft4_toff();
    let an = demod.analyze(f0, i0)?;
    for llr in an.llrs.iter() {
        let (plain, errors) = bp_decode(llr, LDPC_ITERS);
        if errors == 0 {
            if let Some(p) = verify_codeword(wf.protocol, &plain) {
                return Some((p, an.freq_hz, an.dt));
            }
        } else if osd_enabled() && errors <= OSD_MAX_ERRORS {
            for cand in osd::osd_decode(llr) {
                if let Some(p) = verify_codeword(wf.protocol, &cand) {
                    return Some((p, an.freq_hz, an.dt));
                }
            }
        }
    }
    // A-priori fallback on the first (full-integration) LLR variant.
    if ap::ap_enabled() {
        if let Some(llr) = an.llrs.first() {
            if let Some(p) = ap::try_ap(llr, wf.protocol) {
                return Some((p, an.freq_hz, an.dt));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{synth_ft4, synth_ft8};
    use crate::message::{CallHash as Ch, encode_std};

    // Deterministic white-ish noise via a simple LCG (no Math.random).
    fn add_noise(sig: &mut [f32], amp: f32, mut state: u64) {
        for s in sig.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = ((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0; // ~[-1,1)
            *s += amp * r;
        }
    }

    #[test]
    fn round_trip_single_signal() {
        let mut h = Ch::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let sig = synth_ft8(&payload, 1000.0, 12000);
        let decodes = decode(&sig, 12000, Protocol::Ft8);
        assert!(
            decodes.iter().any(|d| d.message == "CQ K1ABC FN42"),
            "got: {:?}",
            decodes.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn round_trip_single_signal_ft4() {
        let mut h = Ch::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let sig = synth_ft4(&payload, 1200.0, 12000);
        let decodes = decode(&sig, 12000, Protocol::Ft4);
        assert!(
            decodes.iter().any(|d| d.message == "CQ K1ABC FN42"),
            "got: {:?}",
            decodes.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn round_trip_ft4_with_noise_and_multiple_signals() {
        let mut h = Ch::new();
        let p1 = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let p2 = encode_std("W9XYZ", "K1ABC", "-09", &mut h).unwrap();
        let mut sig = synth_ft4(&p1, 800.0, 12000);
        let s2 = synth_ft4(&p2, 1600.0, 12000);
        for (a, b) in sig.iter_mut().zip(s2.iter()) {
            *a += *b;
        }
        add_noise(&mut sig, 0.05, 0x1234_5678_9abc_def0);
        let decodes = decode(&sig, 12000, Protocol::Ft4);
        let msgs: Vec<&String> = decodes.iter().map(|d| &d.message).collect();
        assert!(msgs.iter().any(|m| *m == "CQ K1ABC FN42"), "got {msgs:?}");
        assert!(msgs.iter().any(|m| *m == "W9XYZ K1ABC -09"), "got {msgs:?}");
    }

    /// An FT4-synthesized signal must not decode as FT8 — the two modes are
    /// different waveforms (8 vs 4 tones, 0.16 vs 0.048 s symbols). This is the
    /// concrete reason the synth/TX path has to be mode-aware.
    #[test]
    fn ft4_signal_does_not_decode_as_ft8() {
        let mut h = Ch::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let sig = synth_ft4(&payload, 1200.0, 12000);
        let as_ft8 = decode(&sig, 12000, Protocol::Ft8);
        assert!(
            !as_ft8.iter().any(|d| d.message == "CQ K1ABC FN42"),
            "FT4 waveform should not decode under FT8: {:?}",
            as_ft8.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// The crowded-band fix in one assertion: when a signal's Costas tones AND their
    /// neighbor bins are all loud (other signals packed alongside + u8 saturation),
    /// the legacy neighbor-contrast scorer collapses to ~0 and the signal is never
    /// nominated — while the noise-relative `*_baseline` scorer still scores it far
    /// above threshold, because it references each tone to the quiet noise floor in
    /// its own frequency lane. This is exactly the +13 dB FT4 miss on the Field Day
    /// corpus, reproduced deterministically on a hand-built waterfall.
    #[test]
    #[allow(clippy::needless_range_loop)] // m/k drive the Costas block geometry
    fn baseline_scorer_survives_hot_neighbors_that_kill_neighbor_contrast() {
        let p = Protocol::Ft4;
        let num_bins = 40usize;
        let block_stride = TIME_OSR * FREQ_OSR * num_bins;
        let num_blocks = 110usize; // covers FT4's last sync block (~103) + margin
        let mut wf = Waterfall {
            protocol: p,
            time_osr: TIME_OSR,
            freq_osr: FREQ_OSR,
            num_bins,
            block_stride,
            num_blocks,
            max_blocks: num_blocks,
            min_bin: 0,
            symbol_period: p.symbol_period(),
            mag: vec![150u8; num_blocks * block_stride], // quiet noise floor everywhere
        };
        let c = Candidate { score: 0, time_offset: 0, freq_offset: 10, time_sub: 0, freq_sub: 0 };
        let cell = |block: i32, tone: i32| -> usize {
            block as usize * block_stride
                + (c.time_sub * FREQ_OSR + c.freq_sub) * num_bins
                + (c.freq_offset + tone) as usize
        };
        let (ns, ls, so) = (p.num_sync(), p.length_sync(), p.sync_offset());
        // Pass 1: loud Costas sync tones (the real signal).
        for m in 0..ns {
            for k in 0..ls {
                let block = (1 + so * m + k) as i32;
                wf.mag[cell(block, FT4_COSTAS[m][k] as i32)] = 255;
            }
        }
        // Pass 2: loud neighbors (the crowding/saturation), without lowering a tone.
        for m in 0..ns {
            for k in 0..ls {
                let block = (1 + so * m + k) as i32;
                let sm = FT4_COSTAS[m][k] as i32;
                for (b, t) in [(block, sm - 1), (block, sm + 1), (block - 1, sm), (block + 1, sm)] {
                    if (0..4).contains(&t) && b >= 0 && (b as usize) < num_blocks {
                        let i = cell(b, t);
                        if wf.mag[i] != 255 {
                            wf.mag[i] = 250;
                        }
                    }
                }
            }
        }
        let legacy = ft4_sync_score(&wf, &c);
        let floor = per_freq_floor(&wf);
        let baseline = ft4_sync_score_baseline(&wf, &c, &floor);
        assert!(
            legacy < min_score(),
            "neighbor-contrast should collapse under hot neighbors, got {legacy}"
        );
        assert!(
            baseline > 3 * min_score(),
            "noise-relative scorer should still nominate strongly, got {baseline}"
        );
    }

    #[test]
    fn round_trip_with_noise_and_multiple_signals() {
        let mut h = Ch::new();
        let p1 = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let p2 = encode_std("W9XYZ", "K1ABC", "-09", &mut h).unwrap();
        let mut sig = synth_ft8(&p1, 800.0, 12000);
        let s2 = synth_ft8(&p2, 1600.0, 12000);
        for (a, b) in sig.iter_mut().zip(s2.iter()) {
            *a += *b;
        }
        add_noise(&mut sig, 0.05, 0x1234_5678_9abc_def0);
        let decodes = decode(&sig, 12000, Protocol::Ft8);
        let msgs: Vec<&String> = decodes.iter().map(|d| &d.message).collect();
        assert!(msgs.iter().any(|m| *m == "CQ K1ABC FN42"), "got {msgs:?}");
        assert!(msgs.iter().any(|m| *m == "W9XYZ K1ABC -09"), "got {msgs:?}");
    }

    #[test]
    fn snr_is_plausible_for_a_clean_signal() {
        let mut h = Ch::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let sig = synth_ft8(&payload, 1000.0, 12000);
        let d = decode(&sig, 12000, Protocol::Ft8)
            .into_iter()
            .find(|d| d.message == "CQ K1ABC FN42")
            .expect("decodes");
        // Lands in a sane FT8 range and reads as a strong signal (the GUI calls
        // anything above -12 dB "strong").
        assert!((-28.0..=49.0).contains(&d.snr_db), "snr out of range: {}", d.snr_db);
        assert!(d.snr_db > -12.0, "clean signal should be strong, got {}", d.snr_db);
    }

    #[test]
    fn snr_tracks_signal_level_relative_to_noise() {
        let mut h = Ch::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let sig = synth_ft8(&payload, 1000.0, 12000);
        // Same message + same fixed noise, two signal levels: the louder one must
        // report a higher SNR (the whole point — it's relative to the noise floor).
        let snr_at = |scale: f32| -> f32 {
            let mut s: Vec<f32> = sig.iter().map(|x| x * scale).collect();
            add_noise(&mut s, 0.10, 0xA5A5_5A5A_0F0F_F0F0);
            decode(&s, 12000, Protocol::Ft8)
                .into_iter()
                .find(|d| d.message == "CQ K1ABC FN42")
                .unwrap_or_else(|| panic!("decodes at scale {scale}"))
                .snr_db
        };
        let strong = snr_at(1.0);
        let weak = snr_at(0.15);
        assert!(
            strong > weak,
            "louder signal must read higher SNR: strong={strong} weak={weak}"
        );
    }
}
