//! Digital signal processing.
//!
//! Produces the `SpectrumRow` stream that drives the waterslide's rotated FFT.
//! Pure compute — no async, no I/O — so it stays trivially testable and free of
//! the bus/tokio dependency.
//!
//! Specs: `docs/waterslide_panel.md`, `docs/message-catalog.md` §2.

#![forbid(unsafe_code)]

use std::f32::consts::PI;

/// In-place iterative radix-2 Cooley–Tukey FFT. `re`/`im` must be equal length
/// and a power of two. Dependency-free (matching the `modes` decoder's ethos).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Danielson–Lanczos butterflies.
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wl_re, wl_im) = (ang.cos(), ang.sin());
        let half = len / 2;
        let mut base = 0;
        while base < n {
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..half {
                let a = base + k;
                let b = base + k + half;
                let t_re = re[b] * w_re - im[b] * w_im;
                let t_im = re[b] * w_im + im[b] * w_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let nw_re = w_re * wl_re - w_im * wl_im;
                w_im = w_re * wl_im + w_im * wl_re;
                w_re = nw_re;
            }
            base += len;
        }
        len <<= 1;
    }
}

/// A Hann window of length `n`.
fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (PI * i as f32 / (n as f32 - 1.0)).sin().powi(2))
        .collect()
}

/// Compute one log-scaled, peak-held magnitude spectrum from `samples`, covering
/// FFT bins `0..max_bins`. Windows of `fft_size` (a power of two) are Hann-weighted
/// and advanced by `hop`, taking the per-bin maximum across the whole buffer — so a
/// transient FT8 tone shows up even though it occupies only part of the slot.
/// Returns one normalized `u8` (0..=255) per bin; empty input yields all zeros.
pub fn waterfall_peak(samples: &[f32], fft_size: usize, hop: usize, max_bins: usize) -> Vec<u8> {
    let bins = max_bins.min(fft_size / 2);
    if bins == 0 || !fft_size.is_power_of_two() || samples.len() < fft_size {
        return vec![0; bins];
    }
    let hop = hop.max(1);
    let win = hann(fft_size);
    let mut peak = vec![0.0f32; bins];

    let mut start = 0;
    while start + fft_size <= samples.len() {
        let mut re: Vec<f32> = (0..fft_size).map(|i| samples[start + i] * win[i]).collect();
        let mut im = vec![0.0f32; fft_size];
        fft(&mut re, &mut im);
        for b in 0..bins {
            let mag = (re[b] * re[b] + im[b] * im[b]).sqrt();
            if mag > peak[b] {
                peak[b] = mag;
            }
        }
        start += hop;
    }

    // Log-compress then normalize to the strongest bin so the display auto-scales.
    let logs: Vec<f32> = peak.iter().map(|&m| (1.0 + m).ln()).collect();
    let max = logs.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    logs.iter().map(|&v| ((v / max) * 255.0) as u8).collect()
}

/// Brightness mapping range for a spectrogram column, in dB (FFT magnitude
/// normalized by window length). Bins at/below the floor read black; at/above the
/// ceiling, full brightness. Tune if the waterfall is too dark or washed out.
const COL_DB_FLOOR: f32 = -80.0;
const COL_DB_CEIL: f32 = -20.0;

/// Compute one spectrogram column from the *last* `fft_size` samples of `samples`
/// (a single Hann-windowed FFT), mapping bins `0..max_bins` to a fixed-scale
/// brightness `u8`. Unlike [`waterfall_peak`], the scale is absolute (not
/// per-column normalized), so column-to-column brightness is comparable — which a
/// scrolling spectrogram needs. Too-short input yields all zeros.
pub fn spectrum_column(samples: &[f32], fft_size: usize, max_bins: usize) -> Vec<u8> {
    let bins = max_bins.min(fft_size / 2);
    if bins == 0 || !fft_size.is_power_of_two() || samples.len() < fft_size {
        return vec![0; bins];
    }
    let win = hann(fft_size);
    let off = samples.len() - fft_size;
    let mut re: Vec<f32> = (0..fft_size).map(|i| samples[off + i] * win[i]).collect();
    let mut im = vec![0.0f32; fft_size];
    fft(&mut re, &mut im);

    let inv_n = 1.0 / fft_size as f32;
    (0..bins)
        .map(|b| {
            let power = (re[b] * re[b] + im[b] * im[b]) * inv_n * inv_n;
            let db = 10.0 * (power + 1e-12).log10();
            let t = ((db - COL_DB_FLOOR) / (COL_DB_CEIL - COL_DB_FLOOR)).clamp(0.0, 1.0);
            (t * 255.0) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pure tone should peak in the bin nearest its frequency.
    #[test]
    fn tone_peaks_in_its_bin() {
        let rate = 12_000.0f32;
        let fft_size = 1024;
        let freq = 1500.0f32;
        let samples: Vec<f32> = (0..fft_size * 4)
            .map(|i| (2.0 * PI * freq * i as f32 / rate).sin())
            .collect();
        let mags = waterfall_peak(&samples, fft_size, fft_size / 2, fft_size / 2);

        let bin_hz = rate / fft_size as f32;
        let expected = (freq / bin_hz).round() as usize;
        let argmax = mags
            .iter()
            .enumerate()
            .max_by_key(|&(_, &m)| m)
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            (argmax as i32 - expected as i32).abs() <= 1,
            "peak at bin {argmax}, expected ~{expected}"
        );
        assert_eq!(
            mags[argmax], 255,
            "strongest bin should normalize to full scale"
        );
    }

    #[test]
    fn short_input_is_safe() {
        assert_eq!(waterfall_peak(&[0.1, 0.2], 1024, 512, 256), vec![0u8; 256]);
        assert_eq!(spectrum_column(&[0.1, 0.2], 1024, 256), vec![0u8; 256]);
    }

    /// A strong tone lights its bin brighter than the noise floor, on the absolute
    /// scale (so a quiet column would stay dark — the property a spectrogram needs).
    #[test]
    fn column_tone_is_bright_in_its_bin() {
        let rate = 12_000.0f32;
        let fft_size = 1024;
        let freq = 1200.0f32;
        let samples: Vec<f32> = (0..fft_size)
            .map(|i| 0.5 * (2.0 * PI * freq * i as f32 / rate).sin())
            .collect();
        let col = spectrum_column(&samples, fft_size, fft_size / 2);
        let bin = (freq / (rate / fft_size as f32)).round() as usize;
        assert!(
            col[bin] > 200,
            "tone bin should be bright, got {}",
            col[bin]
        );
        assert!(
            col[bin / 4] < 64,
            "off-tone bin should be dim, got {}",
            col[bin / 4]
        );
    }
}
