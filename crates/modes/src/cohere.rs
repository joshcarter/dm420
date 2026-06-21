//! Coherent per-candidate demodulation for FT8 — a port of WSJT-X's
//! `ft8_downsample.f90` + `ft8b.f90` (the fine-sync + complex-symbol demod core).
//!
//! This is the front-end sensitivity lever our `ft8_lib`-derived magnitude path
//! lacks (see `docs/decoder_sensitivity_plan.md`, Phase 2). Where the magnitude
//! waterfall reads one tone power per symbol off a coarse 2×2 grid, this path:
//!
//! 1. **downconverts** each candidate to a complex 200 Hz baseband centered on its
//!    frequency (`ft8_downsample`: one cached long FFT of the slot, sliced and
//!    inverse-transformed per candidate);
//! 2. **fine-syncs** to ~0.5 Hz / 5 ms using a coherent Costas metric (`sync8d`);
//! 3. **demodulates coherently** — a 32-point complex FFT per symbol keeps phase,
//!    so soft bits can be built from **coherent 1-, 2-, and 3-symbol integration**
//!    (sum the complex tone amplitudes *before* taking magnitude). The 2/3-symbol
//!    sums are what WSJT-X's source calls "the main weak-signal gain."
//!
//! Output is up to five LLR variants per candidate; the caller runs each through
//! BP + OSD + the CRC gate, exactly as the magnitude path does. FT8 only for now;
//! FT4 still uses the magnitude path.

use crate::constants::{FT8_COSTAS, FT8_GRAY};
use crate::fft::{Cfft, Complex, Fft};

type C = Complex<f32>;

// FT8 geometry at 12 kHz (matches WSJT-X `ft8_params.f90` / `ft8_downsample.f90`).
const SR: usize = 12_000;
const NSPS: usize = 1920; // samples/symbol at 12 kHz
const NDOWN: usize = 60; // downsample factor → 200 Hz
const NFFT1: usize = 192_000; // long slot FFT (180000 samples zero-padded)
const NFFT2: usize = 3_200; // downsampled length (= NFFT1 / NDOWN)
const NP2: usize = 2_812; // valid span of the downsampled stream
const NN: usize = 79; // channel symbols
const SPS2: usize = NSPS / NDOWN; // 32 downsampled samples/symbol
const FS2: f32 = (SR / NDOWN) as f32; // 200 Hz
const DF: f32 = SR as f32 / NFFT1 as f32; // 0.0625 Hz/bin in the long FFT
const BAUD: f32 = SR as f32 / NSPS as f32; // 6.25 Hz tone spacing
const NTAPER: usize = 100; // cosine taper length on the slice edges

/// Result of coherently analyzing one candidate.
pub struct Analysis {
    /// LLR variants in WSJT-X pass order: nsym=1, nsym=2, nsym=3, nsym=1
    /// bit-normalized, and per-bit best-of-(1,2,3). The caller decodes each.
    pub llrs: Vec<[f32; 174]>,
    /// Refined audio frequency (Hz) after the ±2.5 Hz fine-frequency search.
    pub freq_hz: f32,
    /// Refined time offset (s) from the start of the analyzed audio.
    pub dt: f32,
}

/// Coherent demodulator: owns the FFT plans, the cached long-FFT spectrum of the
/// current slot, and reusable buffers. Build once, `set_slot` per slot, `analyze`
/// per candidate.
pub struct Demod {
    long: Fft,        // 192000-pt real FFT (slot → spectrum)
    inv: Cfft,        // 3200-pt inverse (baseband downconversion)
    symfft: Cfft,     // 32-pt forward (per-symbol)
    spec: Vec<C>,     // cached slot spectrum, NFFT1/2 + 1 bins
    re: Vec<f32>,     // long-FFT scratch
    im: Vec<f32>,     // long-FFT scratch
    cd0: Vec<C>,      // downsampled baseband, NFFT2
    taper: [f32; NTAPER + 1],
    csync: [[C; SPS2]; 7], // Costas reference waveforms
}

impl Default for Demod {
    fn default() -> Self {
        Self::new()
    }
}

impl Demod {
    pub fn new() -> Demod {
        let twopi = std::f32::consts::TAU;
        // Cosine taper, taper[0]=1 .. taper[NTAPER]=0 (matches the Fortran table).
        let taper = std::array::from_fn(|i| 0.5 * (1.0 + (i as f32 * std::f32::consts::PI / NTAPER as f32).cos()));
        // Costas waveforms: a unit complex exponential at each sync tone.
        let mut csync = [[C::default(); SPS2]; 7];
        for (i, row) in csync.iter_mut().enumerate() {
            let dphi = twopi * FT8_COSTAS[i] as f32 / SPS2 as f32;
            let mut phi = 0.0f32;
            for s in row.iter_mut() {
                *s = C::new(phi.cos(), phi.sin());
                phi = (phi + dphi) % twopi;
            }
        }
        Demod {
            long: Fft::new(NFFT1),
            inv: Cfft::new(NFFT2),
            symfft: Cfft::new(SPS2),
            spec: vec![C::default(); NFFT1 / 2 + 1],
            re: vec![0.0; NFFT1],
            im: vec![0.0; NFFT1],
            cd0: vec![C::default(); NFFT2],
            taper,
            csync,
        }
    }

    /// Compute and cache the long FFT of one slot's audio (≤ NFFT1 samples; the
    /// slot is zero-padded). Call once per slot before `analyze`.
    pub fn set_slot(&mut self, samples: &[f32]) {
        let mut x = vec![0.0f32; NFFT1];
        let n = samples.len().min(NFFT1);
        x[..n].copy_from_slice(&samples[..n]);
        self.long.forward_real(&x, &mut self.re, &mut self.im);
        for (k, c) in self.spec.iter_mut().enumerate() {
            *c = C::new(self.re[k], self.im[k]);
        }
    }

    /// Downconvert the cached slot to a complex 200 Hz baseband centered on `f0`,
    /// writing into `self.cd0`. Port of `ft8_downsample`.
    fn downsample(&mut self, f0: f32) {
        let i0 = (f0 / DF).round() as i64;
        let it = (((f0 + 8.5 * BAUD) / DF).round() as i64).min((NFFT1 / 2) as i64);
        let ib = (((f0 - 1.5 * BAUD) / DF).round() as i64).max(1);
        for c in self.cd0.iter_mut() {
            *c = C::default();
        }
        let mut k = 0usize;
        let mut i = ib;
        while i <= it && k < NFFT2 {
            self.cd0[k] = self.spec[i as usize];
            k += 1;
            i += 1;
        }
        if k == 0 {
            return;
        }
        // Cosine-taper both edges of the copied slice.
        for j in 0..=NTAPER.min(k - 1) {
            self.cd0[j] *= self.taper[NTAPER - j]; // rising on the low edge
            self.cd0[k - 1 - j] *= self.taper[j]; // falling on the high edge
        }
        // Shift the carrier bin (i0) to DC, then inverse-transform to time domain.
        let ish = (i0 - ib).rem_euclid(NFFT2 as i64) as usize;
        self.cd0.rotate_left(ish);
        self.inv.inverse(&mut self.cd0);
        let fac = 1.0 / ((NFFT1 as f32) * (NFFT2 as f32)).sqrt();
        for c in self.cd0.iter_mut() {
            *c *= fac;
        }
    }

    /// Coherent Costas sync power at downsampled start sample `i0`, optionally with
    /// a per-sample frequency tweak `ctwk`. Port of `sync8d`.
    fn sync8d(&self, i0: i64, ctwk: Option<&[C; SPS2]>) -> f32 {
        let mut sync = 0.0f32;
        for (i, base) in self.csync.iter().enumerate() {
            // The three Costas arrays sit at symbols i, i+36, i+72.
            let starts = [i as i64, i as i64 + 36, i as i64 + 72].map(|s| i0 + s * SPS2 as i64);
            for &start in &starts {
                if start < 0 || start as usize + SPS2 > NP2 {
                    continue;
                }
                let mut z = C::default();
                for j in 0..SPS2 {
                    let mut cs = base[j];
                    if let Some(tw) = ctwk {
                        cs *= tw[j];
                    }
                    z += self.cd0[start as usize + j] * cs.conj();
                }
                sync += z.norm_sqr();
            }
        }
        sync
    }

    /// Fine-sync (coarse time → ±2.5 Hz freq → fine time), returning the refined
    /// start sample, refined frequency, and `self.cd0` left holding the baseband at
    /// the refined frequency. Mirrors `ft8b.f90` lines 105–153.
    fn fine_sync(&mut self, f0: f32, i0_guess: f32) -> (i64, f32) {
        self.downsample(f0);

        // Coarse time: ±¼ symbol around the candidate's position. Kept tight — a wide
        // window risks locking onto a louder neighbor's Costas on crowded bands. (The
        // waterfall→start convention offset is applied by the caller, not here, so this
        // search stays centered on whatever start guess it is handed.)
        const COARSE_W: i64 = 10;
        let i0 = i0_guess.round() as i64;
        let mut ibest = i0;
        let mut smax = f32::NEG_INFINITY;
        for idt in (i0 - COARSE_W)..=(i0 + COARSE_W) {
            let s = self.sync8d(idt, None);
            if s > smax {
                smax = s;
                ibest = idt;
            }
        }

        // Fine frequency: ±2.5 Hz in 0.5 Hz steps, via a per-sample phase ramp.
        let twopi = std::f32::consts::TAU;
        let dt2 = 1.0 / FS2;
        let mut delf_best = 0.0f32;
        smax = f32::NEG_INFINITY;
        for ifr in -5..=5 {
            let delf = ifr as f32 * 0.5;
            let dphi = twopi * delf * dt2;
            let mut ctwk = [C::default(); SPS2];
            let mut phi = 0.0f32;
            for c in ctwk.iter_mut() {
                *c = C::new(phi.cos(), phi.sin());
                phi = (phi + dphi) % twopi;
            }
            let s = self.sync8d(ibest, Some(&ctwk));
            if s > smax {
                smax = s;
                delf_best = delf;
            }
        }

        // Re-extract at the corrected frequency, then refine time once more.
        let f1 = f0 + delf_best;
        self.downsample(f1);
        let mut ib2 = ibest;
        smax = f32::NEG_INFINITY;
        for idt in -4..=4 {
            let s = self.sync8d(ibest + idt, None);
            if s > smax {
                smax = s;
                ib2 = ibest + idt;
            }
        }
        (ib2, f1)
    }

    /// Per-symbol complex tone amplitudes: `cs[sym][tone]` for the 8 FT8 tones.
    /// Port of the `four2a` symbol-FFT loop in `ft8b.f90` (lines 155–162).
    fn symbol_spectra(&mut self, ibest: i64) -> [[C; 8]; NN] {
        let mut cs = [[C::default(); 8]; NN];
        let mut buf = [C::default(); SPS2];
        for (k, sym) in cs.iter_mut().enumerate() {
            let i1 = ibest + k as i64 * SPS2 as i64;
            if i1 >= 0 && (i1 as usize + SPS2) <= NP2 {
                for (j, b) in buf.iter_mut().enumerate() {
                    *b = self.cd0[i1 as usize + j];
                }
            } else {
                buf = [C::default(); SPS2];
            }
            self.symfft.forward(&mut buf);
            sym.copy_from_slice(&buf[0..8]);
        }
        cs
    }

    /// Hard Costas-sync agreement (0..=21): for each of the 21 sync symbols, does
    /// the strongest tone match the expected Costas tone? (`ft8b.f90` 164–177.)
    fn hard_sync(cs: &[[C; 8]; NN]) -> u32 {
        let mut nsync = 0u32;
        for arr in [0usize, 36, 72] {
            for k in 0..7 {
                let s = &cs[arr + k];
                let mut best = 0usize;
                let mut bv = -1.0f32;
                for (t, c) in s.iter().enumerate() {
                    let m = c.norm();
                    if m > bv {
                        bv = m;
                        best = t;
                    }
                }
                if best == FT8_COSTAS[k] as usize {
                    nsync += 1;
                }
            }
        }
        nsync
    }

    /// Coherently analyze one candidate at audio frequency `f0` with downsampled
    /// start guess `i0_guess` (= (time_offset + time_sub/osr)·32). Returns the LLR
    /// variants and refined geometry, or `None` if the hard-sync gate fails.
    pub fn analyze(&mut self, f0: f32, i0_guess: f32) -> Option<Analysis> {
        let (ibest, f1) = self.fine_sync(f0, i0_guess);
        let cs = self.symbol_spectra(ibest);
        let nsync = Self::hard_sync(&cs);
        // The candidate already passed the magnitude Costas sync, so keep this gate
        // loose — CRC is the real filter; this only skips obviously-bad fits.
        const SYNC_GATE: u32 = 4;
        if nsync < SYNC_GATE {
            return None;
        }
        let llrs = coherent_llrs(&cs);
        Some(Analysis {
            llrs,
            freq_hz: f1,
            dt: ibest as f32 / FS2,
        })
    }
}

/// Bit `j` (0-based, LSB) of symbol value `i`.
#[inline]
fn bit(i: usize, j: usize) -> bool {
    (i >> j) & 1 == 1
}

/// Max of `s2[i]` over the values `i` whose bit `j` matches `want`.
#[inline]
fn max_over(s2: &[f32], j: usize, want: bool) -> f32 {
    let mut m = f32::NEG_INFINITY;
    for (i, &v) in s2.iter().enumerate() {
        if bit(i, j) == want && v > m {
            m = v;
        }
    }
    m
}

/// Build the five LLR variants from the coherent symbol spectra. Direct port of
/// the metric loop in `ft8b.f90` (lines 186–254): for nsym ∈ {1,2,3}, sum the
/// complex tone amplitudes over nsym adjacent symbols *before* magnitude, then form
/// max-log bit metrics. `a`=nsym1, `b`=nsym2, `c`=nsym3, `d`=nsym1 bit-normalized,
/// `e`=per-bit best of a/b/c. Each is variance-normalized for our BP.
fn coherent_llrs(cs: &[[C; 8]; NN]) -> Vec<[f32; 174]> {
    let g = |t: usize| FT8_GRAY[t] as usize; // gray value → tone
    let mut a = [0.0f32; 174];
    let mut b = [0.0f32; 174];
    let mut c = [0.0f32; 174];
    let mut d = [0.0f32; 174];

    for nsym in 1..=3usize {
        let nt = 1usize << (3 * nsym);
        let ibmax = 3 * nsym - 1; // bits produced per group (2, 5, 8)
        for ihalf in 0..2usize {
            let mut k = 1usize;
            while k <= 29 {
                // First data symbol of this group (0-based channel-symbol index).
                let ks = if ihalf == 0 { k - 1 + 7 } else { k - 1 + 43 };
                // Coherent magnitude for each of the nt tone-combination hypotheses.
                let mut s2 = vec![0.0f32; nt];
                for (i, s) in s2.iter_mut().enumerate() {
                    let i1 = i / 64;
                    let i2 = (i & 63) / 8;
                    let i3 = i & 7;
                    let z = match nsym {
                        1 => cs[ks][g(i3)],
                        2 => cs[ks][g(i2)] + cs[ks + 1][g(i3)],
                        _ => cs[ks][g(i1)] + cs[ks + 1][g(i2)] + cs[ks + 2][g(i3)],
                    };
                    *s = z.norm();
                }
                // Bit base index (0-based): WSJT-X i32 = 1+(k-1)*3+(ihalf-1)*87.
                let base = (k - 1) * 3 + ihalf * 87;
                for ib in 0..=ibmax {
                    let j = ibmax - ib; // bit position within the group
                    let bm = max_over(&s2, j, true) - max_over(&s2, j, false);
                    let idx = base + ib;
                    if idx >= 174 {
                        continue;
                    }
                    match nsym {
                        1 => {
                            a[idx] = bm;
                            let den = max_over(&s2, j, true).max(max_over(&s2, j, false));
                            d[idx] = if den > 0.0 { bm / den } else { 0.0 };
                        }
                        2 => b[idx] = bm,
                        _ => c[idx] = bm,
                    }
                }
                k += nsym;
            }
        }
    }

    // e[i] = whichever of a/b/c has the largest magnitude at bit i.
    let mut e = [0.0f32; 174];
    for i in 0..174 {
        let mut best = a[i];
        for &v in &[b[i], c[i]] {
            if v.abs() > best.abs() {
                best = v;
            }
        }
        e[i] = best;
    }

    let mut out = vec![a, b, c, d, e];
    for v in out.iter_mut() {
        crate::decode::normalize_llr(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::verify_codeword;
    use crate::encode::synth_ft8;
    use crate::ldpc::bp_decode;
    use crate::message::{CallHash, encode_std};
    use crate::waterfall::Protocol;

    #[test]
    fn coherent_decodes_clean_signal() {
        let mut h = CallHash::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let f0 = 1500.0;
        let sig = synth_ft8(&payload, f0, 12000);
        // synth centers the signal: lead-in silence = (180000 - 79*0.16*12000)/2.
        let n_data = (79.0 * 0.16 * 12000.0) as usize;
        let lead = (180000 - n_data) / 2;
        let i0_true = lead as f32 / NDOWN as f32; // downsampled samples

        let mut d = Demod::new();
        d.set_slot(&sig);
        let an = d.analyze(f0, i0_true).expect("analyze returns Some");

        let mut decoded = None;
        for llr in an.llrs.iter() {
            let (plain, _errors) = bp_decode(llr, 25);
            if let Some(p) = verify_codeword(Protocol::Ft8, &plain) {
                decoded = Some(p);
            }
        }
        // Compare decoded *text* (the trailing payload byte carries pad bits that
        // CRC doesn't constrain, so the raw 10-byte payload can differ harmlessly).
        let mut hh = CallHash::new();
        let got = decoded.and_then(|p| crate::message::decode(&p, &mut hh)).map(|(t, _)| t);
        assert_eq!(got.as_deref(), Some("CQ K1ABC FN42"), "coherent path should decode the clean signal");
    }
}
