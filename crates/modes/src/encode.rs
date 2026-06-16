//! FT8 modulation — payload -> 79 tones -> GFSK audio.
//!
//! Ported from ft8_lib `encode.c` (tone layout) and `gen_ft8.c` (GFSK synthesis).
//! Used to synthesize known signals so the decode pipeline is self-verifying
//! without a radio, and to produce test/demo audio.

use crate::constants::{FT8_COSTAS, FT8_GRAY};
use crate::ldpc::encode174;
use crate::message::payload_with_crc;

pub const FT8_SYMBOL_PERIOD: f32 = 0.160;
pub const FT8_SLOT_TIME: f32 = 15.0;
const FT8_SYMBOL_BT: f32 = 2.0;
const GFSK_CONST_K: f32 = 5.336446; // pi * sqrt(2 / ln 2)

/// Map a 77-bit payload to the 79 FT8 channel tones (3 Costas sync groups
/// interleaved with 58 Gray-coded data symbols).
pub fn ft8_tones(payload: &[u8; 10]) -> [u8; 79] {
    let a91 = payload_with_crc(payload);
    let mut codeword = [0u8; 22];
    encode174(&a91, &mut codeword);

    let mut tones = [0u8; 79];
    let mut bitpos = 0usize;
    let next3 = |bp: &mut usize| -> usize {
        let mut v = 0usize;
        for _ in 0..3 {
            let bit = (codeword[*bp / 8] >> (7 - (*bp % 8))) & 1;
            v = (v << 1) | bit as usize;
            *bp += 1;
        }
        v
    };

    for (i, tone) in tones.iter_mut().enumerate() {
        *tone = if i < 7 {
            FT8_COSTAS[i]
        } else if (36..43).contains(&i) {
            FT8_COSTAS[i - 36]
        } else if (72..79).contains(&i) {
            FT8_COSTAS[i - 72]
        } else {
            FT8_GRAY[next3(&mut bitpos)]
        };
    }
    tones
}

/// Error function (Abramowitz & Stegun 7.1.26), good to ~1e-7 — plenty for the
/// GFSK pulse shape. (Constants are the published coefficients; extra digits
/// beyond f32 are harmless.)
#[allow(clippy::excessive_precision)]
fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

fn gfsk_pulse(n_spsym: usize, bt: f32) -> Vec<f32> {
    (0..3 * n_spsym)
        .map(|i| {
            let t = i as f32 / n_spsym as f32 - 1.5;
            let arg1 = GFSK_CONST_K * bt * (t + 0.5);
            let arg2 = GFSK_CONST_K * bt * (t - 0.5);
            (erf(arg1) - erf(arg2)) / 2.0
        })
        .collect()
}

/// GFSK-synthesize `symbols` at base frequency `f0`. Returns n_sym×n_spsym
/// samples (the modulated part only).
fn synth_gfsk(symbols: &[u8], f0: f32, bt: f32, symbol_period: f32, sample_rate: u32) -> Vec<f32> {
    let sr = sample_rate as f32;
    let n_spsym = (0.5 + sr * symbol_period) as usize;
    let n_sym = symbols.len();
    let n_wave = n_sym * n_spsym;
    let two_pi = 2.0 * std::f32::consts::PI;
    let dphi_peak = two_pi / n_spsym as f32;

    let mut dphi = vec![two_pi * f0 / sr; n_wave + 2 * n_spsym];
    let pulse = gfsk_pulse(n_spsym, bt);

    for (i, &sym) in symbols.iter().enumerate() {
        let ib = i * n_spsym;
        for j in 0..3 * n_spsym {
            dphi[j + ib] += dphi_peak * sym as f32 * pulse[j];
        }
    }
    // Extend first/last symbols into the lead-in/lead-out.
    for j in 0..2 * n_spsym {
        dphi[j] += dphi_peak * pulse[j + n_spsym] * symbols[0] as f32;
        dphi[j + n_sym * n_spsym] += dphi_peak * pulse[j] * symbols[n_sym - 1] as f32;
    }

    let mut signal = vec![0.0f32; n_wave];
    let mut phi = 0.0f32;
    for (k, s) in signal.iter_mut().enumerate() {
        *s = phi.sin();
        phi = (phi + dphi[k + n_spsym]) % two_pi;
    }
    // Raised-cosine envelope ramp on the ends.
    let n_ramp = n_spsym / 8;
    for i in 0..n_ramp {
        let env = (1.0 - (two_pi * i as f32 / (2.0 * n_ramp as f32)).cos()) / 2.0;
        signal[i] *= env;
        signal[n_wave - 1 - i] *= env;
    }
    signal
}

/// Synthesize a full 15-second FT8 slot for `payload` at audio frequency `f0`,
/// with the signal centered (silence padded) like a real transmission.
pub fn synth_ft8(payload: &[u8; 10], f0: f32, sample_rate: u32) -> Vec<f32> {
    let tones = ft8_tones(payload);
    let sr = sample_rate as f32;
    let n_data = (0.5 + tones.len() as f32 * FT8_SYMBOL_PERIOD * sr) as usize;
    let n_total = (FT8_SLOT_TIME * sr) as usize;
    let n_silence = (n_total - n_data) / 2;

    let mut signal = vec![0.0f32; n_total];
    let data = synth_gfsk(&tones, f0, FT8_SYMBOL_BT, FT8_SYMBOL_PERIOD, sample_rate);
    signal[n_silence..n_silence + data.len()].copy_from_slice(&data);
    signal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_have_valid_costas_and_range() {
        let payload = [0x00u8, 1, 2, 3, 4, 5, 6, 7, 8, 0];
        let tones = ft8_tones(&payload);
        assert_eq!(&tones[0..7], &FT8_COSTAS);
        assert_eq!(&tones[36..43], &FT8_COSTAS);
        assert_eq!(&tones[72..79], &FT8_COSTAS);
        assert!(tones.iter().all(|&t| t < 8));
    }

    #[test]
    fn synth_produces_full_slot() {
        let payload = [0x00u8, 1, 2, 3, 4, 5, 6, 7, 8, 0];
        let sig = synth_ft8(&payload, 1000.0, 12000);
        assert_eq!(sig.len(), 180000);
        let peak = sig.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.5, "signal should be near full-scale, peak={peak}");
    }
}
