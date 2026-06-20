//! A small self-contained FFT — no external crates.
//!
//! The FT8 waterfall needs an N-point DFT where N = samples-per-symbol ×
//! freq_osr (e.g. 1920 × 2 = 3840 at 12 kHz), which is *not* a power of two. So
//! we provide a radix-2 transform for power-of-two sizes and wrap it in
//! [`Bluestein`]'s chirp-z algorithm to handle arbitrary N via a power-of-two
//! convolution. Inputs are real (audio); we return all N complex bins and let
//! the caller keep the first N/2+1.
//!
//! This is our own implementation; correctness is checked against a naive DFT in
//! the unit tests.
//!
//! [`Fft`] wraps this Bluestein path and a [`realfft`]/`rustfft` path behind one
//! interface, selected by [`FftBackend`] so the decoder's FFT can be swapped live
//! for A/B comparison (the magnitude pipeline and every downstream stage are
//! identical; only the transform differs). Both return the *unnormalized* forward
//! DFT, so they're drop-in: the `2/nfft` window gain lives in the analysis window,
//! not the FFT, and the decoder only reads bins well below Nyquist (inside the
//! `N/2+1` that the real-input transform returns).

use std::f64::consts::PI;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// In-place iterative radix-2 Cooley–Tukey FFT. `re`/`im` must have equal,
/// power-of-two length. Forward transform uses the e^{-2πi·nk/N} sign.
fn fft_radix2(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    debug_assert_eq!(im.len(), n);
    if n <= 1 {
        return;
    }

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Butterflies.
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wre_step, wim_step) = (ang.cos(), ang.sin());
        let half = len / 2;
        let mut i = 0;
        while i < n {
            let (mut wre, mut wim) = (1.0f64, 0.0f64);
            for k in 0..half {
                let a = i + k;
                let b = a + half;
                let tre = re[b] * wre - im[b] * wim;
                let tim = re[b] * wim + im[b] * wre;
                re[b] = re[a] - tre;
                im[b] = im[a] - tim;
                re[a] += tre;
                im[a] += tim;
                let nwre = wre * wre_step - wim * wim_step;
                wim = wre * wim_step + wim * wre_step;
                wre = nwre;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Inverse radix-2 FFT (unnormalized inverse: divides by N).
fn ifft_radix2(re: &mut [f64], im: &mut [f64]) {
    // ifft(x) = conj(fft(conj(x))) / N
    for v in im.iter_mut() {
        *v = -*v;
    }
    fft_radix2(re, im);
    let inv = 1.0 / re.len() as f64;
    for (r, i) in re.iter_mut().zip(im.iter_mut()) {
        *r *= inv;
        *i = -*i * inv;
    }
}

/// Arbitrary-N DFT via Bluestein's chirp-z algorithm. Construct once for a given
/// `n` and reuse — it precomputes the chirp and the transform of the filter.
pub struct Bluestein {
    n: usize,
    m: usize,
    // chirp w[k] = exp(-i·π·k²/n)
    w_re: Vec<f64>,
    w_im: Vec<f64>,
    // FFT of the convolution filter b[k] = conj(w[k]), arranged for linear conv
    bfft_re: Vec<f64>,
    bfft_im: Vec<f64>,
}

impl Bluestein {
    pub fn new(n: usize) -> Bluestein {
        assert!(n >= 1);
        let m = (2 * n - 1).next_power_of_two();
        let mut w_re = vec![0.0; n];
        let mut w_im = vec![0.0; n];
        for k in 0..n {
            // phase = -π k² / n, reduced via (k² mod 2n) to keep f64 precision.
            let k2 = ((k as u64) * (k as u64)) % (2 * n as u64);
            let phase = -PI * (k2 as f64) / (n as f64);
            w_re[k] = phase.cos();
            w_im[k] = phase.sin();
        }

        // Filter b[k] = conj(w[k]); b[0..n) and mirrored into b[m-k].
        let mut b_re = vec![0.0; m];
        let mut b_im = vec![0.0; m];
        b_re[0] = w_re[0];
        b_im[0] = -w_im[0];
        for k in 1..n {
            let (re, im) = (w_re[k], -w_im[k]);
            b_re[k] = re;
            b_im[k] = im;
            b_re[m - k] = re;
            b_im[m - k] = im;
        }
        fft_radix2(&mut b_re, &mut b_im);

        Bluestein {
            n,
            m,
            w_re,
            w_im,
            bfft_re: b_re,
            bfft_im: b_im,
        }
    }

    /// Forward DFT of a real input of length `n`. Writes `n` complex bins into
    /// `out_re`/`out_im` (each length >= n).
    pub fn forward_real(&self, x: &[f32], out_re: &mut [f32], out_im: &mut [f32]) {
        let n = self.n;
        debug_assert_eq!(x.len(), n);
        let mut a_re = vec![0.0f64; self.m];
        let mut a_im = vec![0.0f64; self.m];
        for k in 0..n {
            let xr = x[k] as f64;
            a_re[k] = xr * self.w_re[k];
            a_im[k] = xr * self.w_im[k];
        }
        fft_radix2(&mut a_re, &mut a_im);
        // Pointwise multiply by filter spectrum.
        for i in 0..self.m {
            let (ar, ai) = (a_re[i], a_im[i]);
            let (br, bi) = (self.bfft_re[i], self.bfft_im[i]);
            a_re[i] = ar * br - ai * bi;
            a_im[i] = ar * bi + ai * br;
        }
        ifft_radix2(&mut a_re, &mut a_im);
        // X[k] = w[k] · conv[k]
        for k in 0..n {
            let (cr, ci) = (a_re[k], a_im[k]);
            out_re[k] = (cr * self.w_re[k] - ci * self.w_im[k]) as f32;
            out_im[k] = (cr * self.w_im[k] + ci * self.w_re[k]) as f32;
        }
    }
}

/// Which FFT implementation the decoder's STFT front-end uses. Switchable live for
/// A/B comparison (the live toggle picks one per slot via `core::FftControl`).
/// [`Bluestein`] is the original, proven path and the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FftBackend {
    /// Our hand-rolled radix-2 + Bluestein chirp-z FFT (no external crates).
    #[default]
    Bluestein,
    /// `realfft` (real-input) on top of `rustfft` — SIMD, planner-cached twiddles.
    RealFft,
}

impl FftBackend {
    /// Short stable tag for logs/UI (`"bluestein"` / `"realfft"`).
    pub fn tag(self) -> &'static str {
        match self {
            FftBackend::Bluestein => "bluestein",
            FftBackend::RealFft => "realfft",
        }
    }
}

/// Forward real FFT via [`realfft`]/`rustfft`. Holds the plan plus reusable input/
/// output/scratch buffers, so steady-state has no per-call allocation (matching
/// [`Bluestein`]'s precompute-once design). Returns the `N/2 + 1` non-redundant
/// bins — all the decoder reads, since its used bins sit well below Nyquist.
pub struct RealFftEngine {
    n: usize,
    r2c: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl RealFftEngine {
    pub fn new(n: usize) -> RealFftEngine {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(n);
        let input = r2c.make_input_vec(); // len n
        let spectrum = r2c.make_output_vec(); // len n/2 + 1
        let scratch = r2c.make_scratch_vec();
        RealFftEngine {
            n,
            r2c,
            input,
            spectrum,
            scratch,
        }
    }

    /// Forward DFT of a real input of length `n`. Writes the first `n/2 + 1` complex
    /// bins into `out_re`/`out_im`; higher indices are left as-is — the caller only
    /// reads bins below `n/2` (real-transform conjugate symmetry covers the rest).
    pub fn forward_real(&mut self, x: &[f32], out_re: &mut [f32], out_im: &mut [f32]) {
        debug_assert_eq!(x.len(), self.n);
        self.input.copy_from_slice(x);
        // `process_with_scratch` overwrites `input`; that's why we copy in each call.
        self.r2c
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .expect("realfft forward transform (buffer lengths are fixed at construction)");
        for (k, c) in self.spectrum.iter().enumerate() {
            out_re[k] = c.re;
            out_im[k] = c.im;
        }
    }
}

/// The decoder's FFT, selectable between implementations ([`FftBackend`]). Built
/// once per [`crate::waterfall::Monitor`]; the live A/B toggle decides which
/// backend the next slot's Monitor is constructed with.
pub enum Fft {
    Bluestein(Bluestein),
    RealFft(RealFftEngine),
}

impl Fft {
    pub fn new(backend: FftBackend, n: usize) -> Fft {
        match backend {
            FftBackend::Bluestein => Fft::Bluestein(Bluestein::new(n)),
            FftBackend::RealFft => Fft::RealFft(RealFftEngine::new(n)),
        }
    }

    /// Forward real FFT into `out_re`/`out_im` (see each backend's `forward_real`).
    /// `&mut self` because `realfft` writes through reusable scratch; the Bluestein
    /// arm simply ignores the mutability.
    pub fn forward_real(&mut self, x: &[f32], out_re: &mut [f32], out_im: &mut [f32]) {
        match self {
            Fft::Bluestein(b) => b.forward_real(x, out_re, out_im),
            Fft::RealFft(r) => r.forward_real(x, out_re, out_im),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(x: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let n = x.len();
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        for k in 0..n {
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for (t, &xt) in x.iter().enumerate() {
                let ang = -2.0 * PI * (k as f64) * (t as f64) / (n as f64);
                sr += xt as f64 * ang.cos();
                si += xt as f64 * ang.sin();
            }
            re[k] = sr as f32;
            im[k] = si as f32;
        }
        (re, im)
    }

    fn check(n: usize) {
        // Deterministic pseudo-random-ish signal.
        let x: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.3).sin() + (i as f32 * 0.07).cos() * 0.5)
            .collect();
        let bl = Bluestein::new(n);
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        bl.forward_real(&x, &mut re, &mut im);
        let (rr, ri) = naive_dft(&x);
        for k in 0..n {
            assert!(
                (re[k] - rr[k]).abs() < 1e-2 * (n as f32),
                "re[{k}] N={n}: {} vs {}",
                re[k],
                rr[k]
            );
            assert!((im[k] - ri[k]).abs() < 1e-2 * (n as f32), "im[{k}] N={n}");
        }
    }

    #[test]
    fn matches_naive_dft_pow2() {
        check(16);
        check(64);
    }

    #[test]
    fn matches_naive_dft_arbitrary() {
        check(10); // FT4-ish factor
        check(96);
        check(3840); // the actual FT8 nfft at 12 kHz, freq_osr=2
    }

    /// The `realfft` engine must match the naive DFT over the `N/2+1` bins it
    /// returns — the decoder reads only this lower half, so this pins exactly what
    /// the A/B alternate backend feeds the sync/demod stages.
    fn check_realfft(n: usize) {
        let x: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.3).sin() + (i as f32 * 0.07).cos() * 0.5)
            .collect();
        let mut eng = RealFftEngine::new(n);
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        eng.forward_real(&x, &mut re, &mut im);
        let (rr, ri) = naive_dft(&x);
        for k in 0..=n / 2 {
            assert!(
                (re[k] - rr[k]).abs() < 1e-2 * (n as f32),
                "re[{k}] N={n}: {} vs {}",
                re[k],
                rr[k]
            );
            assert!((im[k] - ri[k]).abs() < 1e-2 * (n as f32), "im[{k}] N={n}");
        }
    }

    #[test]
    fn realfft_matches_naive_dft() {
        check_realfft(16);
        check_realfft(1152); // FT4 nfft at 12 kHz
        check_realfft(3840); // FT8 nfft at 12 kHz, freq_osr=2
    }
}
