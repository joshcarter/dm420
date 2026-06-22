//! Coherent per-candidate demodulation for FT4 — a port of WSJT-X's
//! `ft4_downsample.f90` + `sync4d.f90` + `get_ft4_bitmetrics.f90` (the fine-sync +
//! complex-symbol demod core), the FT4 sibling of [`crate::cohere`] (FT8).
//!
//! Same shape as the FT8 coherent path, with FT4 parameters and FT4's bit-metric
//! step (the one piece that differs structurally):
//!
//! 1. **downconvert** each candidate to a complex 666.67 Hz baseband centered on
//!    its frequency (`ft4_downsample`: one cached long FFT of the slot, a symmetric
//!    slice around the carrier through a **flat-top** window, inverse-transformed
//!    per candidate — note FT8 uses a one-sided slice through a cosine taper);
//! 2. **fine-syncs** using the coherent four-Costas metric (`sync4d`);
//! 3. **demodulates coherently** — a 32-point complex FFT per symbol keeps phase,
//!    so soft bits come from coherent 1-, 2-, and 4-symbol integration over FT4's
//!    4-tone symbols (`get_ft4_bitmetrics`: 256 four-symbol sequences).
//!
//! Output is five LLR variants per candidate; the caller runs each through BP +
//! OSD + the CRC gate, exactly as the magnitude path does. Coherent is tried
//! first, magnitude is the fallback, so this can only add decodes.

use crate::constants::{FT4_COSTAS, FT4_GRAY};
use crate::fft::{Cfft, Complex, Fft};

type C = Complex<f32>;

// FT4 geometry at 12 kHz (matches WSJT-X `ft4_params.f90` / `ft4_downsample.f90`).
const SR: usize = 12_000;
const NSPS: usize = 576; // samples/symbol at 12 kHz
const NDOWN: usize = 18; // downsample factor → 666.67 Hz
const NMAX: usize = 79_488; // long slot FFT length (= 23·3456, FT2-sized buffer)
const NFFT2: usize = NMAX / NDOWN; // 4416 downsampled length
const NN: usize = 103; // channel symbols (16 sync + 87 data; excludes 2 ramps)
const NSS: usize = NSPS / NDOWN; // 32 downsampled samples/symbol (same as FT8 SPS2)
const NSYNC2: usize = 2 * NSS; // 64 strided correlation points per Costas array
const FS2: f32 = (SR / NDOWN) as f32; // 666.67 Hz
const DF: f32 = SR as f32 / NMAX as f32; // 0.151 Hz/bin in the long FFT
const BAUD: f32 = SR as f32 / NSPS as f32; // 20.833 Hz tone spacing

/// Result of coherently analyzing one candidate.
pub struct Analysis {
    /// LLR variants in WSJT-X pass order: nsym=1, nsym=2, nsym=4, nsym=1
    /// bit-normalized, and per-bit best-of-(1,2,4). The caller decodes each.
    pub llrs: Vec<[f32; 174]>,
    /// Refined audio frequency (Hz) after the fine-frequency search.
    pub freq_hz: f32,
    /// Refined time offset (s) from the start of the analyzed audio.
    pub dt: f32,
}

/// Coherent FT4 demodulator: owns the FFT plans, the cached long-FFT spectrum of
/// the current slot, and reusable buffers. Build once, `set_slot` per slot,
/// `analyze` per candidate.
pub struct Demod {
    long: Fft,    // 79488-pt real FFT (slot → spectrum)
    inv: Cfft,    // 4416-pt inverse (baseband downconversion)
    symfft: Cfft, // 32-pt forward (per-symbol)
    spec: Vec<C>, // cached slot spectrum, NMAX/2 + 1 bins
    re: Vec<f32>, // long-FFT scratch
    im: Vec<f32>, // long-FFT scratch
    cd0: Vec<C>,  // downsampled baseband, NFFT2
    window: Vec<f32>, // flat-top downsample window, length NFFT2
    csync: [[C; NSYNC2]; 4], // four Costas reference waveforms
}

impl Default for Demod {
    fn default() -> Self {
        Self::new()
    }
}

impl Demod {
    pub fn new() -> Demod {
        let twopi = std::f32::consts::TAU;

        // Flat-top window (ft4_downsample): cosine ramps of width bw_transition
        // around a flat top of width bw_flat, then cyclically shifted by one baud
        // so the band sits centered after the carrier is rotated to DC.
        let mut window = vec![0.0f32; NFFT2];
        let iwt = (0.5 * BAUD / DF) as usize; // transition half-width in bins
        let iwf = (4.0 * BAUD / DF) as usize; // flat-top width in bins
        for (i, w) in window.iter_mut().take(iwt).enumerate() {
            *w = 0.5 * (1.0 + (std::f32::consts::PI * (iwt - 1 - i) as f32 / iwt as f32).cos());
        }
        for w in window.iter_mut().skip(iwt).take(iwf) {
            *w = 1.0;
        }
        for (j, w) in window.iter_mut().skip(iwt + iwf).take(iwt).enumerate() {
            *w = 0.5 * (1.0 + (std::f32::consts::PI * j as f32 / iwt as f32).cos());
        }
        let iws = (BAUD / DF) as usize; // cyclic shift = one baud
        window.rotate_left(iws);

        // Costas waveforms: a unit complex exponential at each sync tone, sampled
        // at the strided (decimate-by-2) rate sync4d correlates over — NSS/2 points
        // per symbol, four symbols, = NSYNC2 points per Costas array.
        let mut csync = [[C::default(); NSYNC2]; 4];
        for (row, costas) in csync.iter_mut().zip(FT4_COSTAS.iter()) {
            let mut k = 0usize;
            for &tone in costas.iter() {
                let dphi = 2.0 * twopi * tone as f32 / NSS as f32;
                let mut phi = 0.0f32;
                for _ in 0..NSS / 2 {
                    row[k] = C::new(phi.cos(), phi.sin());
                    phi = (phi + dphi) % twopi;
                    k += 1;
                }
            }
        }

        Demod {
            long: Fft::new(NMAX),
            inv: Cfft::new(NFFT2),
            symfft: Cfft::new(NSS),
            spec: vec![C::default(); NMAX / 2 + 1],
            re: vec![0.0; NMAX],
            im: vec![0.0; NMAX],
            cd0: vec![C::default(); NFFT2],
            window,
            csync,
        }
    }

    /// Compute and cache the long FFT of one slot's audio (≤ NMAX samples; the
    /// slot is zero-padded). Call once per slot before `analyze`.
    pub fn set_slot(&mut self, samples: &[f32]) {
        let mut x = vec![0.0f32; NMAX];
        let n = samples.len().min(NMAX);
        x[..n].copy_from_slice(&samples[..n]);
        self.long.forward_real(&x, &mut self.re, &mut self.im);
        for (k, c) in self.spec.iter_mut().enumerate() {
            *c = C::new(self.re[k], self.im[k]);
        }
    }

    /// Downconvert the cached slot to a complex 666.67 Hz baseband centered on
    /// `f0`, writing into `self.cd0` (normalized to unit average power). Port of
    /// `ft4_downsample`.
    fn downsample(&mut self, f0: f32) {
        let i0 = (f0 / DF).round() as i64;
        let nyq = (NMAX / 2) as i64;
        for c in self.cd0.iter_mut() {
            *c = C::default();
        }
        // Symmetric slice around the carrier: positive offsets to low indices,
        // negative offsets wrapped to high indices (so the carrier lands at DC).
        if i0 >= 0 && i0 <= nyq {
            self.cd0[0] = self.spec[i0 as usize];
        }
        for i in 1..=NFFT2 / 2 {
            let ip = i0 + i as i64;
            if ip >= 0 && ip <= nyq {
                self.cd0[i] = self.spec[ip as usize];
            }
            let im = i0 - i as i64;
            if im >= 0 && im <= nyq {
                self.cd0[NFFT2 - i] = self.spec[im as usize];
            }
        }
        for (c, &w) in self.cd0.iter_mut().zip(self.window.iter()) {
            *c *= w;
        }
        self.inv.inverse(&mut self.cd0);
        // Absolute scale is irrelevant downstream (sync power and bit metrics are
        // normalized), so normalize to unit average power and drop the FFT gains.
        let p: f32 = self.cd0.iter().map(|z| z.norm_sqr()).sum::<f32>() / NFFT2 as f32;
        if p > 0.0 {
            let s = 1.0 / p.sqrt();
            for c in self.cd0.iter_mut() {
                *c *= s;
            }
        }
    }

    /// Coherent four-Costas sync power at downsampled start sample `i0`, optionally
    /// with a per-sample frequency tweak `ctwk`. Port of `sync4d`. The four Costas
    /// arrays sit at symbol offsets 0, 33, 66, 99 from `i0`; each is correlated over
    /// `NSYNC2` samples decimated by 2 across its 4·NSS span.
    fn sync4d(&self, i0: i64, ctwk: Option<&[C; NSYNC2]>) -> f32 {
        let mut sync = 0.0f32;
        for (m, base) in self.csync.iter().enumerate() {
            let start = i0 + (m as i64) * 33 * NSS as i64;
            if start < 0 || start as usize + 4 * NSS > NFFT2 {
                continue;
            }
            let mut z = C::default();
            for j in 0..NSYNC2 {
                let mut cs = base[j];
                if let Some(tw) = ctwk {
                    cs *= tw[j];
                }
                z += self.cd0[start as usize + 2 * j] * cs.conj();
            }
            sync += z.norm();
        }
        sync
    }

    /// Fine-sync (coarse time → fine freq → fine time), returning the refined start
    /// sample and frequency, with `self.cd0` left holding the baseband at the
    /// refined frequency. Mirrors `cohere::Demod::fine_sync` with sync4d.
    fn fine_sync(&mut self, f0: f32, i0_guess: f32) -> (i64, f32) {
        self.downsample(f0);

        // Coarse time around the candidate's position. Wider than FT8's window
        // because FT4's start convention (ramp symbol, 0.5 s offset) is absorbed
        // here pending the empirical i0 correction at the call site.
        const COARSE_W: i64 = 24;
        let i0 = i0_guess.round() as i64;
        let mut ibest = i0;
        let mut smax = f32::NEG_INFINITY;
        for idt in (i0 - COARSE_W)..=(i0 + COARSE_W) {
            let s = self.sync4d(idt, None);
            if s > smax {
                smax = s;
                ibest = idt;
            }
        }

        // Fine frequency: ±10 Hz in 1 Hz steps via a per-(strided-)sample phase
        // ramp. The strided correlation samples are spaced 2/FS2 s apart.
        let twopi = std::f32::consts::TAU;
        let dt_str = 2.0 / FS2;
        let mut delf_best = 0.0f32;
        smax = f32::NEG_INFINITY;
        for ifr in -10..=10 {
            let delf = ifr as f32;
            let dphi = twopi * delf * dt_str;
            let mut ctwk = [C::default(); NSYNC2];
            let mut phi = 0.0f32;
            for c in ctwk.iter_mut() {
                *c = C::new(phi.cos(), phi.sin());
                phi = (phi + dphi) % twopi;
            }
            let s = self.sync4d(ibest, Some(&ctwk));
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
            let s = self.sync4d(ibest + idt, None);
            if s > smax {
                smax = s;
                ib2 = ibest + idt;
            }
        }
        (ib2, f1)
    }

    /// Per-symbol complex tone amplitudes: `cs[sym][tone]` for the 4 FT4 tones.
    /// Port of the symbol-FFT loop in `get_ft4_bitmetrics` (`four2a(csymb,NSS,…)`,
    /// keeping the first 4 bins).
    fn symbol_spectra(&mut self, ibest: i64) -> [[C; 4]; NN] {
        let mut cs = [[C::default(); 4]; NN];
        let mut buf = [C::default(); NSS];
        for (k, sym) in cs.iter_mut().enumerate() {
            let i1 = ibest + k as i64 * NSS as i64;
            if i1 >= 0 && (i1 as usize + NSS) <= NFFT2 {
                for (j, b) in buf.iter_mut().enumerate() {
                    *b = self.cd0[i1 as usize + j];
                }
            } else {
                buf = [C::default(); NSS];
            }
            self.symfft.forward(&mut buf);
            sym.copy_from_slice(&buf[0..4]);
        }
        cs
    }

    /// Hard Costas-sync agreement (0..=16): for each of the 16 sync symbols (four
    /// 4-symbol Costas arrays at symbol 0, 33, 66, 99), does the strongest tone
    /// match the expected Costas tone? (`get_ft4_bitmetrics` sync check.)
    fn hard_sync(cs: &[[C; 4]; NN]) -> u32 {
        let mut nsync = 0u32;
        for (m, costas) in FT4_COSTAS.iter().enumerate() {
            for k in 0..4usize {
                let s = &cs[m * 33 + k];
                let mut best = 0usize;
                let mut bv = -1.0f32;
                for (t, c) in s.iter().enumerate() {
                    let mag = c.norm();
                    if mag > bv {
                        bv = mag;
                        best = t;
                    }
                }
                if best == costas[k] as usize {
                    nsync += 1;
                }
            }
        }
        nsync
    }

    /// Coherently analyze one candidate at audio frequency `f0` with downsampled
    /// start guess `i0_guess`. Returns the LLR variants and refined geometry, or
    /// `None` if the hard-sync gate fails.
    pub fn analyze(&mut self, f0: f32, i0_guess: f32) -> Option<Analysis> {
        let (ibest, f1) = self.fine_sync(f0, i0_guess);
        let cs = self.symbol_spectra(ibest);
        let nsync = Self::hard_sync(&cs);
        // The candidate already passed the magnitude Costas sync; keep this gate
        // loose (CRC is the real filter, this only skips obviously-bad fits).
        const SYNC_GATE: u32 = 6;
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

/// Bit `j` (0-based, LSB) of value `i`.
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

/// Build the five LLR variants from the coherent symbol spectra. Port of
/// `get_ft4_bitmetrics`: for nsym ∈ {1,2,4}, sum the complex tone amplitudes over
/// nsym adjacent symbols *before* magnitude (over 4^nsym tone-combinations), then
/// form max-log bit metrics over all 2·NN symbol-bit positions. Columns: nsym1,
/// nsym2, nsym4, nsym1 bit-normalized, and per-bit cherry-pick of the first three.
/// Then the 174 data bits are extracted by skipping the four Costas blocks.
fn coherent_llrs(cs: &[[C; 4]; NN]) -> Vec<[f32; 174]> {
    let g = |t: usize| FT4_GRAY[t] as usize; // gray value → tone
    const NB: usize = 2 * NN; // 206 raw symbol-bit positions
    let mut bm = vec![[0.0f32; 5]; NB];

    for (col, &nsym) in [1usize, 2, 4].iter().enumerate() {
        let nt = 1usize << (2 * nsym); // 4, 16, 256
        let ibmax = 2 * nsym - 1; // bits produced per group: 1, 3, 7
        let mut ks = 0usize; // first data symbol of this group (0-based)
        while ks + nsym <= NN {
            let mut s2 = vec![0.0f32; nt];
            for (i, sv) in s2.iter_mut().enumerate() {
                let z = match nsym {
                    1 => cs[ks][g(i & 3)],
                    2 => cs[ks][g((i >> 2) & 3)] + cs[ks + 1][g(i & 3)],
                    _ => {
                        cs[ks][g((i >> 6) & 3)]
                            + cs[ks + 1][g((i >> 4) & 3)]
                            + cs[ks + 2][g((i >> 2) & 3)]
                            + cs[ks + 3][g(i & 3)]
                    }
                };
                *sv = z.norm();
            }
            let ipt = ks * 2; // 0-based bit pointer (Fortran 1+(ks-1)*2)
            for ib in 0..=ibmax {
                let j = ibmax - ib; // bit position within the group
                let v = max_over(&s2, j, true) - max_over(&s2, j, false);
                let idx = ipt + ib;
                if idx >= NB {
                    continue;
                }
                bm[idx][col] = v;
                if nsym == 1 {
                    let den = max_over(&s2, j, true).max(max_over(&s2, j, false));
                    bm[idx][3] = if den > 0.0 { v / den } else { 0.0 };
                }
            }
            ks += nsym;
        }
    }

    // Edge patches: the nsym=2 and nsym=4 groups don't tile the 206 positions
    // evenly, so the tail bits borrow from the shorter sequences (Fortran
    // bitmetrics(205:206,2)=...(,1); (201:204,3)=...(,2); (205:206,3)=...(,1)).
    bm[204][1] = bm[204][0];
    bm[205][1] = bm[205][0];
    for row in bm[200..=203].iter_mut() {
        row[2] = row[1];
    }
    bm[204][2] = bm[204][0];
    bm[205][2] = bm[205][0];

    // Cherry-pick (column 5): for each bit, whichever of columns 1-3 has the
    // largest absolute value (the most-confident coherent estimate).
    for row in bm.iter_mut() {
        let mut best = row[0];
        for &v in &[row[1], row[2]] {
            if v.abs() > best.abs() {
                best = v;
            }
        }
        row[4] = best;
    }

    // Extract the 174 data-bit LLRs from each column, skipping the four 4-symbol
    // Costas blocks (bit positions 1-8, 67-74, 133-140, 199-206; 0-based: 0-7,
    // 66-73, 132-139, 198-205). Three runs of 58 bits = 174.
    // Source positions of the 174 data bits (skipping the four Costas blocks).
    let src_idx: Vec<usize> = (8..66).chain(74..132).chain(140..198).collect();
    (0..5)
        .map(|col| {
            let mut llr = [0.0f32; 174];
            for (dst, &s) in src_idx.iter().enumerate() {
                llr[dst] = bm[s][col];
            }
            crate::decode::normalize_llr(&mut llr);
            llr
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::verify_codeword;
    use crate::encode::synth_ft4;
    use crate::ldpc::bp_decode;
    use crate::message::{CallHash, encode_std};
    use crate::waterfall::Protocol;

    #[test]
    fn coherent_decodes_clean_signal() {
        let mut h = CallHash::new();
        let payload = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let f0 = 1500.0;
        let sig = synth_ft4(&payload, f0, 12000);

        // synth_ft4 centers the 105-symbol (incl. 2 ramp) waveform; the first
        // Costas symbol starts one symbol (NSPS) past the lead-in silence.
        let n_total = (105.0 * NSPS as f32) as usize;
        let lead = (sig.len() - n_total) / 2;
        let i0_true = (lead + NSPS) as f32 / NDOWN as f32; // first Costas, downsampled

        let mut d = Demod::new();
        d.set_slot(&sig);
        let an = d.analyze(f0, i0_true).expect("analyze returns Some");

        let mut decoded = None;
        for llr in an.llrs.iter() {
            let (plain, _errors) = bp_decode(llr, 25);
            if let Some(p) = verify_codeword(Protocol::Ft4, &plain) {
                decoded = Some(p);
            }
        }
        let mut hh = CallHash::new();
        let got = decoded
            .and_then(|p| crate::message::decode(&p, &mut hh))
            .map(|(t, _)| t);
        assert_eq!(
            got.as_deref(),
            Some("CQ K1ABC FN42"),
            "coherent FT4 path should decode the clean signal"
        );
    }
}
