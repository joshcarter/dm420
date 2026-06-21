//! Multi-pass subtraction: remove a decoded FT8 signal from the time-domain
//! audio so that weaker signals it was masking can be decoded on a later pass.
//!
//! This is the single biggest crowded-band win — masking (a louder neighbor a
//! few Hz away corrupting a quiet signal's soft symbols) is ~half of what we miss
//! versus WSJT-X (see `docs/decoder_sensitivity_plan.md`). The decoder works off
//! a magnitude-only waterfall, so subtraction must happen here, on the audio:
//! re-synthesize the decode's GFSK phase, fit its complex amplitude, subtract,
//! then the caller rebuilds the waterfall on the residual and decodes again.
//!
//! Amplitude is fit **per symbol** rather than globally. Two reasons: the
//! candidate's frequency is only good to ~±1.5 Hz, which would smear a global fit
//! over the 12.6 s transmission, but barely moves within one 0.16 s symbol; and a
//! per-symbol gain tracks fading. The candidate's timing grid is coarse (half a
//! symbol), so we first refine the start sample by a short search for peak signal
//! energy.

use crate::encode::ft8_reference_phase;
use crate::waterfall::Protocol;

/// Sample offsets tried when refining the start time. The sync grid is ~half a
/// symbol (960 samples at 12 kHz), so the true start is within ±¼ symbol of the
/// candidate; ±480 in steps of 120 covers that.
const TIMING_SEARCH: [i64; 9] = [-480, -360, -240, -120, 0, 120, 240, 360, 480];

/// Estimate and subtract the FT8 signal `payload` (audio frequency `f0`, approx
/// start `dt` seconds into the slot) from `residual` in place.
pub fn subtract_ft8(residual: &mut [f32], payload: &[u8; 10], f0: f32, dt: f32, sample_rate: u32) {
    let phase = ft8_reference_phase(payload, f0, sample_rate);
    let n_spsym = (0.5 + sample_rate as f32 * Protocol::Ft8.symbol_period()) as usize;
    let n_sym = phase.len() / n_spsym;

    // Precompute the complex reference (cos/sin of the phase) once — the hot loops
    // are then plain multiply-accumulate, no trig.
    let cos_t: Vec<f32> = phase.iter().map(|p| p.cos()).collect();
    let sin_t: Vec<f32> = phase.iter().map(|p| p.sin()).collect();

    let nominal = (dt * sample_rate as f32).round() as i64;
    let start = TIMING_SEARCH
        .iter()
        .map(|&sh| (corr_energy(residual, &cos_t, &sin_t, nominal + sh, n_spsym, n_sym), sh))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, sh)| nominal + sh)
        .unwrap_or(nominal);

    // Per-symbol: g = (2/n) Σ x·e^{-jφ}, then subtract Re(g·e^{jφ}) = gre·cosφ − gim·sinφ.
    for s in 0..n_sym {
        let w0 = start + (s * n_spsym) as i64;
        let base = s * n_spsym;
        let (mut sre, mut sim, mut cnt) = (0.0f32, 0.0f32, 0usize);
        for k in 0..n_spsym {
            let Some(x) = sample_at(residual, w0 + k as i64) else {
                continue;
            };
            sre += x * cos_t[base + k];
            sim -= x * sin_t[base + k];
            cnt += 1;
        }
        if cnt == 0 {
            continue;
        }
        let gre = 2.0 * sre / cnt as f32;
        let gim = 2.0 * sim / cnt as f32;
        for k in 0..n_spsym {
            let idx = w0 + k as i64;
            if idx < 0 || idx as usize >= residual.len() {
                continue;
            }
            residual[idx as usize] -= gre * cos_t[base + k] - gim * sin_t[base + k];
        }
    }
}

/// Total per-symbol correlation magnitude of the reference against `residual` at
/// a given start sample — the timing-search objective (larger = better aligned).
fn corr_energy(
    residual: &[f32],
    cos_t: &[f32],
    sin_t: &[f32],
    start: i64,
    n_spsym: usize,
    n_sym: usize,
) -> f32 {
    let mut e = 0.0f32;
    for s in 0..n_sym {
        let w0 = start + (s * n_spsym) as i64;
        let base = s * n_spsym;
        let (mut sre, mut sim) = (0.0f32, 0.0f32);
        for k in 0..n_spsym {
            let Some(x) = sample_at(residual, w0 + k as i64) else {
                continue;
            };
            sre += x * cos_t[base + k];
            sim -= x * sin_t[base + k];
        }
        e += (sre * sre + sim * sim).sqrt();
    }
    e
}

#[inline]
fn sample_at(buf: &[f32], idx: i64) -> Option<f32> {
    if idx < 0 || idx as usize >= buf.len() {
        None
    } else {
        Some(buf[idx as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::synth_ft8;

    fn power(x: &[f32]) -> f32 {
        x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32
    }

    #[test]
    fn subtraction_removes_most_of_a_clean_signal() {
        // Synthesize one FT8 signal, then subtract it back out: the residual
        // power should collapse, confirming the fit + timing search lock on.
        let payload = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x10];
        let f0 = 1234.0;
        let sr = 12_000;
        let signal = synth_ft8(&payload, f0, sr);
        let before = power(&signal);

        // synth_ft8 centers the signal; that placement is dt = n_silence / sr.
        let n_data = (0.5 + 79.0 * Protocol::Ft8.symbol_period() * sr as f32) as usize;
        let n_silence = (signal.len() - n_data) / 2;
        let dt = n_silence as f32 / sr as f32;

        let mut residual = signal.clone();
        subtract_ft8(&mut residual, &payload, f0, dt, sr);
        let after = power(&residual);

        assert!(
            after < before * 0.05,
            "subtraction should remove >95% of the signal power: before={before}, after={after}"
        );
    }

    #[test]
    fn subtraction_leaves_a_distinct_signal_intact() {
        // Two signals at different frequencies; subtracting one must barely touch
        // the other (they are near-orthogonal), so most power survives.
        let pa = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x10];
        let pb = [0xA1u8, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29, 0x30];
        let sr = 12_000;
        let (fa, fb) = (1000.0, 1600.0);
        let a = synth_ft8(&pa, fa, sr);
        let b = synth_ft8(&pb, fb, sr);

        let n_data = (0.5 + 79.0 * Protocol::Ft8.symbol_period() * sr as f32) as usize;
        let dt = ((a.len() - n_data) / 2) as f32 / sr as f32;

        let mut sum: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let b_power = power(&b);
        subtract_ft8(&mut sum, &pa, fa, dt, sr); // remove signal A
        // What's left should be ~signal B: compare residual to B directly.
        let diff: Vec<f32> = sum.iter().zip(&b).map(|(x, y)| x - y).collect();
        assert!(
            power(&diff) < b_power * 0.1,
            "the other signal should survive: residual differs from B by {} vs B power {b_power}",
            power(&diff)
        );
    }
}
