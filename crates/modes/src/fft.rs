//! The decoder's FFT — a forward real-input transform via the [`realfft`] crate
//! (built on `rustfft`).
//!
//! The FT8/FT4 waterfall needs an N-point DFT where N = samples-per-symbol ×
//! freq_osr (e.g. 1920 × 2 = 3840 at 12 kHz), which is *not* a power of two.
//! `rustfft` handles arbitrary N (mixed-radix plus its own internal Bluestein),
//! SIMD-accelerated with planner-cached twiddles, and `realfft` exploits the
//! real-input symmetry to return just the `N/2 + 1` non-redundant bins — all the
//! decoder reads, since its used bins sit well below Nyquist.
//!
//! The transform is *unnormalized*: the `2/nfft` gain lives in the analysis window
//! (see `waterfall.rs`), not here, which is what the sync/demod stages expect.
//! Correctness is checked against a naive DFT in the unit tests.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Forward real FFT. Holds the plan plus reusable input/output/scratch buffers, so
/// steady-state has no per-call allocation. Built once per
/// [`crate::waterfall::Monitor`] for its fixed transform size.
pub struct Fft {
    n: usize,
    r2c: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(n);
        let input = r2c.make_input_vec(); // len n
        let spectrum = r2c.make_output_vec(); // len n/2 + 1
        let scratch = r2c.make_scratch_vec();
        Fft {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

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

    /// The FFT must match the naive DFT over the `N/2+1` bins it returns — the
    /// decoder reads only this lower half, so this pins exactly what feeds the
    /// sync/demod stages, including the actual FT8/FT4 transform sizes.
    fn check(n: usize) {
        // Deterministic pseudo-random-ish signal.
        let x: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.3).sin() + (i as f32 * 0.07).cos() * 0.5)
            .collect();
        let mut fft = Fft::new(n);
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        fft.forward_real(&x, &mut re, &mut im);
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
    fn matches_naive_dft() {
        check(16);
        check(96);
        check(1152); // FT4 nfft at 12 kHz
        check(3840); // FT8 nfft at 12 kHz, freq_osr=2
    }
}
