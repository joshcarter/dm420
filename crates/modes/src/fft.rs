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

pub use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use rustfft::{Fft as RustFft, FftPlanner};

/// In-place complex forward+inverse FFT (rustfft) for the coherent demod path —
/// baseband downconversion (one big inverse transform per candidate) and the
/// 32-point per-symbol transforms. Like [`Fft`], it caches its plan and scratch
/// so steady-state has no per-call allocation, and the transform is *unnormalized*
/// (rustfft convention: a forward+inverse round-trip scales by `n`).
pub struct Cfft {
    fwd: std::sync::Arc<dyn RustFft<f32>>,
    inv: std::sync::Arc<dyn RustFft<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Cfft {
    pub fn new(n: usize) -> Cfft {
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let scratch = vec![Complex::default(); fwd.get_inplace_scratch_len().max(inv.get_inplace_scratch_len())];
        Cfft { fwd, inv, scratch }
    }

    /// Forward DFT (sign −1), in place.
    pub fn forward(&mut self, buf: &mut [Complex<f32>]) {
        self.fwd.process_with_scratch(buf, &mut self.scratch);
    }

    /// Inverse DFT (sign +1), in place, unnormalized.
    pub fn inverse(&mut self, buf: &mut [Complex<f32>]) {
        self.inv.process_with_scratch(buf, &mut self.scratch);
    }
}

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

    /// A forward then inverse complex transform must reproduce the input scaled by
    /// `n` (rustfft's unnormalized convention), and a pure complex exponential must
    /// land all its energy in the expected bin.
    #[test]
    fn cfft_roundtrip_and_bin() {
        let n = 32;
        let mut c = Cfft::new(n);
        // Tone at bin 5 (a 4-GFSK-ish complex exponential).
        let orig: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let ph = 2.0 * std::f32::consts::PI * 5.0 * i as f32 / n as f32;
                Complex::new(ph.cos(), ph.sin())
            })
            .collect();
        let mut buf = orig.clone();
        c.forward(&mut buf);
        // All energy in bin 5.
        let peak = buf.iter().map(|z| z.norm()).enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert_eq!(peak.0, 5, "tone should land in bin 5");
        assert!((peak.1 - n as f32).abs() < 1e-3, "bin-5 magnitude ≈ n");
        // Inverse recovers the input × n.
        c.inverse(&mut buf);
        for (a, b) in buf.iter().zip(orig.iter()) {
            assert!((a - b * n as f32).norm() < 1e-2, "roundtrip scaled by n");
        }
    }
}
