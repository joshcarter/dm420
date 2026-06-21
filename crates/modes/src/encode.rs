//! FT8/FT4 modulation — payload -> channel tones -> GFSK audio.
//!
//! Ported from ft8_lib `encode.c` (tone layout) and `gen_ft8.c`/`gen_ft4.c` (GFSK
//! synthesis). Used to synthesize known signals so the decode pipeline is
//! self-verifying without a radio, and to produce the live TX waveform.
//!
//! Per-mode parameters (symbol period, slot length, GFSK BT, tone/Costas/Gray
//! tables, whitening) come from [`Protocol`]; the GFSK kernel below is shared.

use crate::constants::{FT4_COSTAS, FT4_GRAY, FT8_COSTAS, FT8_GRAY};
use crate::ldpc::encode174;
use crate::message::payload_with_crc;
use crate::waterfall::Protocol;

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

/// Map a 77-bit payload to the 105 FT4 channel tones: a leading ramp symbol, then
/// the four Costas sync blocks (4 tones each) — the first three each followed by a
/// 29-symbol Gray-coded data group — and a trailing ramp symbol. The payload is
/// whitened with `FT4_XOR` *before* the CRC (the exact inverse of the decoder's
/// post-CRC de-whitening), then LDPC-encoded; data is laid out 2 bits per symbol.
pub fn ft4_tones(payload: &[u8; 10]) -> [u8; 105] {
    // Whiten → CRC → LDPC: the inverse of `decode::decode_candidate`'s tail.
    let mut whitened = *payload;
    if let Some(xor) = Protocol::Ft4.whitening() {
        for (b, x) in whitened.iter_mut().zip(xor.iter()) {
            *b ^= *x;
        }
    }
    let a91 = payload_with_crc(&whitened);
    let mut codeword = [0u8; 22];
    encode174(&a91, &mut codeword);

    let mut bitpos = 0usize;
    let next2 = |bp: &mut usize| -> usize {
        let mut v = 0usize;
        for _ in 0..2 {
            let bit = (codeword[*bp / 8] >> (7 - (*bp % 8))) & 1;
            v = (v << 1) | bit as usize;
            *bp += 1;
        }
        v
    };

    // tones[0] and tones[104] stay 0 — the lead-in/lead-out ramp symbols. Sync
    // blocks start one symbol in (matches `decode::ft4_sync_score`'s `1 + ...`).
    let mut tones = [0u8; 105];
    let mut i = 1;
    for (m, costas) in FT4_COSTAS.iter().enumerate() {
        for &sym in costas {
            tones[i] = sym;
            i += 1;
        }
        if m < 3 {
            for _ in 0..29 {
                tones[i] = FT4_GRAY[next2(&mut bitpos)];
                i += 1;
            }
        }
    }
    debug_assert_eq!(i, 104, "FT4 layout must fill tones[1..104]");
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

/// The per-sample GFSK phase trajectory for `symbols` at base frequency `f0` —
/// the running integral of the pulse-shaped instantaneous frequency. `synth_gfsk`
/// is just this with `sin` applied; signal *subtraction* needs the raw phase to
/// build a complex reference. Returns n_sym×n_spsym values with `phi[0] = 0`.
pub(crate) fn gfsk_phase(
    symbols: &[u8],
    f0: f32,
    bt: f32,
    symbol_period: f32,
    sample_rate: u32,
) -> Vec<f32> {
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

    let mut phi = 0.0f32;
    let mut out = vec![0.0f32; n_wave];
    for (k, p) in out.iter_mut().enumerate() {
        *p = phi;
        phi = (phi + dphi[k + n_spsym]) % two_pi;
    }
    out
}

/// GFSK-synthesize `symbols` at base frequency `f0`. Returns n_sym×n_spsym
/// samples (the modulated part only).
fn synth_gfsk(symbols: &[u8], f0: f32, bt: f32, symbol_period: f32, sample_rate: u32) -> Vec<f32> {
    let n_spsym = (0.5 + sample_rate as f32 * symbol_period) as usize;
    let phi = gfsk_phase(symbols, f0, bt, symbol_period, sample_rate);
    let n_wave = phi.len();
    let mut signal: Vec<f32> = phi.iter().map(|p| p.sin()).collect();

    // Raised-cosine envelope ramp on the ends.
    let two_pi = 2.0 * std::f32::consts::PI;
    let n_ramp = n_spsym / 8;
    for i in 0..n_ramp {
        let env = (1.0 - (two_pi * i as f32 / (2.0 * n_ramp as f32)).cos()) / 2.0;
        signal[i] *= env;
        signal[n_wave - 1 - i] *= env;
    }
    signal
}

/// The complex-reference GFSK phase for the FT8 transmission of `payload` at
/// audio frequency `f0` — the same trajectory `synth` uses for FT8, minus the
/// envelope ramp. Used by signal subtraction to model and remove a decode.
pub(crate) fn ft8_reference_phase(payload: &[u8; 10], f0: f32, sample_rate: u32) -> Vec<f32> {
    let tones = ft8_tones(payload);
    gfsk_phase(
        &tones,
        f0,
        Protocol::Ft8.gfsk_bt(),
        Protocol::Ft8.symbol_period(),
        sample_rate,
    )
}

/// Synthesize a full slot of audio for `payload` in `protocol` at audio frequency
/// `f0`, with the signal centered (silence padded) like a real transmission. The
/// per-mode timing/shape comes from [`Protocol`]; the GFSK kernel is shared.
pub fn synth(payload: &[u8; 10], protocol: Protocol, f0: f32, sample_rate: u32) -> Vec<f32> {
    let tones: Vec<u8> = match protocol {
        Protocol::Ft8 => ft8_tones(payload).to_vec(),
        Protocol::Ft4 => ft4_tones(payload).to_vec(),
    };
    let sr = sample_rate as f32;
    let symbol_period = protocol.symbol_period();
    let n_data = (0.5 + tones.len() as f32 * symbol_period * sr) as usize;
    let n_total = (protocol.slot_time() * sr) as usize;
    let n_silence = (n_total - n_data) / 2;

    let mut signal = vec![0.0f32; n_total];
    let data = synth_gfsk(&tones, f0, protocol.gfsk_bt(), symbol_period, sample_rate);
    signal[n_silence..n_silence + data.len()].copy_from_slice(&data);
    signal
}

/// Synthesize a full FT8 slot for `payload`. Convenience wrapper over [`synth`].
pub fn synth_ft8(payload: &[u8; 10], f0: f32, sample_rate: u32) -> Vec<f32> {
    synth(payload, Protocol::Ft8, f0, sample_rate)
}

/// Synthesize a full FT4 slot for `payload`. Convenience wrapper over [`synth`].
pub fn synth_ft4(payload: &[u8; 10], f0: f32, sample_rate: u32) -> Vec<f32> {
    synth(payload, Protocol::Ft4, f0, sample_rate)
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

    #[test]
    fn ft4_tones_have_valid_layout() {
        let payload = [0x00u8, 1, 2, 3, 4, 5, 6, 7, 8, 0];
        let tones = ft4_tones(&payload);
        // 105 channel symbols, all valid 4-FSK tones (0..4).
        assert_eq!(tones.len(), 105);
        assert!(tones.iter().all(|&t| t < 4));
        // Leading/trailing ramp symbols are tone 0.
        assert_eq!(tones[0], 0);
        assert_eq!(tones[104], 0);
        // The four Costas sync blocks sit at 1 + 33*m, matching the decoder.
        for (m, costas) in FT4_COSTAS.iter().enumerate() {
            let base = 1 + 33 * m;
            assert_eq!(&tones[base..base + 4], costas, "Costas block {m}");
        }
    }

    #[test]
    fn ft4_synth_produces_full_slot() {
        let payload = [0x00u8, 1, 2, 3, 4, 5, 6, 7, 8, 0];
        let sig = synth_ft4(&payload, 1200.0, 12000);
        assert_eq!(sig.len(), 90000); // 7.5 s @ 12 kHz
        let peak = sig.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.5, "signal should be near full-scale, peak={peak}");
    }
}
