//! Candidate sync search, soft-symbol demodulation, and the top-level decode
//! (port of ft8_lib `decode.c` + the `decode_ft8.c` driver loop). Handles FT8
//! and FT4. Output is a list of [`Decode`]s per slot.

use crate::constants::{FT4_COSTAS, FT4_GRAY, FT4_XOR, FT8_COSTAS, FT8_GRAY};
use crate::crc;
use crate::ldpc::{N, bp_decode};
use crate::message::{self, CallHash, MessageType};
use crate::waterfall::{Monitor, Protocol, Waterfall, mag_db};
use std::collections::HashSet;

const MIN_SCORE: i32 = 10;
const MAX_CANDIDATES: usize = 140;
const LDPC_ITERS: usize = 25;
const TIME_OSR: usize = 2;
const FREQ_OSR: usize = 2;

// FT8 sync geometry.
const FT8_NUM_SYNC: usize = 3;
const FT8_LENGTH_SYNC: usize = 7;
const FT8_SYNC_OFFSET: usize = 36;
const FT8_ND: usize = 58;
// FT4 sync geometry.
const FT4_NUM_SYNC: usize = 4;
const FT4_LENGTH_SYNC: usize = 4;
const FT4_SYNC_OFFSET: usize = 33;
const FT4_ND: usize = 87;

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
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..FT8_NUM_SYNC {
        for k in 0..FT8_LENGTH_SYNC {
            let block = (FT8_SYNC_OFFSET * m + k) as i32;
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
            if (k + 1) < FT8_LENGTH_SYNC && (block_abs + 1) < nb {
                score += here - mag_at(wf, c, block_abs + 1, sm) as i32;
                num += 1;
            }
        }
    }
    if num > 0 { score / num } else { 0 }
}

#[allow(clippy::needless_range_loop)]
fn ft4_sync_score(wf: &Waterfall, c: &Candidate) -> i32 {
    let mut score = 0i32;
    let mut num = 0i32;
    let nb = wf.num_blocks as i32;
    for m in 0..FT4_NUM_SYNC {
        for k in 0..FT4_LENGTH_SYNC {
            let block = (1 + FT4_SYNC_OFFSET * m + k) as i32;
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
            if (k + 1) < FT4_LENGTH_SYNC && (block_abs + 1) < nb {
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
    let (num_sync, len_sync, sync_offset) = if wf.protocol == Protocol::Ft4 {
        (FT4_NUM_SYNC, FT4_LENGTH_SYNC, FT4_SYNC_OFFSET)
    } else {
        (FT8_NUM_SYNC, FT8_LENGTH_SYNC, FT8_SYNC_OFFSET)
    };

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

fn find_candidates(wf: &Waterfall) -> Vec<Candidate> {
    let num_tones = wf.protocol.num_tones() as i32;
    let score_fn = if wf.protocol == Protocol::Ft4 {
        ft4_sync_score
    } else {
        ft8_sync_score
    };
    // Costas sync search spans the whole slot, not just a small offset window:
    // recordings/generated files aren't always tight to the slot start (FT4 in
    // particular sits ~25 symbols in), so bound the upper end by where the last
    // data symbol still fits rather than the reference's fixed +20.
    let total_symbols = if wf.protocol == Protocol::Ft4 {
        105
    } else {
        79
    };
    let upper = (wf.num_blocks as i32 - total_symbols + 11).max(20);
    let mut cands = Vec::new();
    for time_sub in 0..wf.time_osr {
        for freq_sub in 0..wf.freq_osr {
            for time_offset in -10..upper {
                for freq_offset in 0..(wf.num_bins as i32 - num_tones + 1) {
                    let mut c = Candidate {
                        score: 0,
                        time_offset,
                        freq_offset,
                        time_sub,
                        freq_sub,
                    };
                    c.score = score_fn(wf, &c);
                    if c.score >= MIN_SCORE {
                        cands.push(c);
                    }
                }
            }
        }
    }
    cands.sort_by_key(|c| std::cmp::Reverse(c.score)); // strongest first
    cands.truncate(MAX_CANDIDATES);
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
        for k in 0..FT4_ND {
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
        for k in 0..FT8_ND {
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

fn normalize_llr(log174: &mut [f32; N]) {
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
    if errors > 0 {
        return None;
    }
    let a91 = pack_bits(&plain);
    let crc_extracted = crc::extract_crc(&a91);
    let mut chk = a91;
    chk[9] &= 0xF8;
    chk[10] = 0;
    let crc_calculated = crc::compute_crc(&chk, 96 - 14);
    if crc_extracted != crc_calculated {
        return None;
    }
    let mut payload = [0u8; 10];
    if wf.protocol == Protocol::Ft4 {
        for i in 0..10 {
            payload[i] = a91[i] ^ FT4_XOR[i];
        }
    } else {
        payload.copy_from_slice(&a91[..10]);
    }
    Some(payload)
}

/// Decode a slot of mono audio at `sample_rate` (12 kHz expected) and return the
/// transmissions found, strongest first.
pub fn decode(samples: &[f32], sample_rate: u32, protocol: Protocol) -> Vec<Decode> {
    let mut mon = Monitor::new(sample_rate, protocol, TIME_OSR, FREQ_OSR, 200.0, 3000.0);
    let bs = mon.block_size;
    let mut pos = 0;
    while pos + bs <= samples.len() {
        mon.process(&samples[pos..pos + bs]);
        pos += bs;
    }

    let wf = &mon.wf;
    let noise = noise_floor(wf);
    let cands = find_candidates(wf);
    let mut hash = CallHash::new();
    let mut seen: HashSet<[u8; 10]> = HashSet::new();
    let mut out = Vec::new();
    let sp = wf.symbol_period;
    for c in &cands {
        let Some(payload) = decode_candidate(wf, c) else {
            continue;
        };
        if !seen.insert(payload) {
            continue;
        }
        let Some((text, msg_type)) = message::decode(&payload, &mut hash) else {
            continue;
        };
        let freq_hz =
            (wf.min_bin as f32 + c.freq_offset as f32 + c.freq_sub as f32 / wf.freq_osr as f32)
                / sp;
        let dt = (c.time_offset as f32 + c.time_sub as f32 / wf.time_osr as f32) * sp;
        out.push(Decode {
            score: c.score,
            snr_db: estimate_snr(wf, c, noise),
            dt,
            freq_hz,
            message: text,
            msg_type,
        });
    }
    // Present low-to-high in frequency by default at the call site; here keep
    // strongest-first (candidates are already sorted by score).
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::synth_ft8;
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
