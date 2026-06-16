//! Pure DSP helpers: channel downmix, level math, and a streaming windowed-sinc
//! resampler. No I/O and no cpal types — everything here is unit-tested against
//! synthetic buffers.
//!
//! The resampler is the Phase 3 groundwork: ft8_lib wants 12 kHz mono, the
//! TS-590's USB codec delivers 48 kHz, and playback needs file-rate -> device-rate
//! conversion. One anti-aliased arbitrary-ratio resampler covers all of it.

/// Downmix interleaved multi-channel f32 samples to mono by averaging channels.
/// `channels == 1` is a cheap copy. Panics if `channels == 0`.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    assert!(channels > 0, "channels must be > 0");
    let ch = channels as usize;
    if ch == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Convert an f32 sample in [-1, 1] to i16 with clamping.
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Peak level (absolute value) of a buffer.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// RMS level of a buffer (0.0 for an empty buffer).
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Linear amplitude -> dBFS (1.0 = 0 dBFS). Floors at -120 dB for silence.
pub fn dbfs(amplitude: f32) -> f32 {
    if amplitude <= 1e-6 {
        -120.0
    } else {
        20.0 * amplitude.log10()
    }
}

/// Render a dBFS value as a meter bar from -60 dB to 0 dB, e.g.
/// `[##############......] -18.3 dBFS`.
pub fn level_bar(db: f32, width: usize) -> String {
    let clamped = db.clamp(-60.0, 0.0);
    let filled = (((clamped + 60.0) / 60.0) * width as f32).round() as usize;
    let filled = filled.min(width);
    format!(
        "[{}{}] {:>6.1} dBFS",
        "#".repeat(filled),
        ".".repeat(width - filled),
        db.max(-120.0)
    )
}

/// Streaming arbitrary-ratio resampler using a windowed-sinc (Blackman) kernel.
///
/// Anti-aliasing is built in: the kernel cutoff tracks the lower of the two
/// rates, so downsampling (48k -> 12k) filters out everything above the new
/// Nyquist instead of folding it into the passband.
///
/// Usage: feed mono input with [`Resampler::push`], collect output; call
/// [`Resampler::flush`] at end-of-stream to drain the tail.
pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    /// Input samples not yet fully consumed. `buf[0]` is absolute input index
    /// `buf_start`.
    buf: Vec<f32>,
    buf_start: u64,
    /// Index of the next output sample to produce.
    next_out: u64,
    /// Kernel half-width, in input samples.
    half_width: usize,
    /// Normalized cutoff in cycles per input sample.
    cutoff: f64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(in_rate > 0 && out_rate > 0, "rates must be > 0");
        // Cutoff at 45% of the lower Nyquist, expressed in cycles per *input*
        // sample. 8 sinc lobes per side, stretched by the cutoff, gives strong
        // stopband rejection at modest cost.
        let cutoff = 0.45 * (in_rate.min(out_rate) as f64) / in_rate as f64;
        let half_width = (8.0 / (2.0 * cutoff)).ceil() as usize;
        Resampler {
            in_rate,
            out_rate,
            buf: Vec::new(),
            buf_start: 0,
            next_out: 0,
            half_width,
            cutoff,
        }
    }

    /// Identity check: no resampling needed.
    pub fn is_identity(&self) -> bool {
        self.in_rate == self.out_rate
    }

    /// Feed mono input samples; returns whatever output samples are now ready.
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_identity() {
            return input.to_vec();
        }
        self.buf.extend_from_slice(input);
        self.produce()
    }

    /// Signal end-of-stream: pads with silence to flush the kernel tail and
    /// returns the remaining output.
    pub fn flush(&mut self) -> Vec<f32> {
        if self.is_identity() {
            return Vec::new();
        }
        self.buf
            .extend(std::iter::repeat(0.0).take(self.half_width + 1));
        self.produce()
    }

    fn produce(&mut self) -> Vec<f32> {
        let step = self.in_rate as f64 / self.out_rate as f64;
        let mut out = Vec::new();
        loop {
            let center = self.next_out as f64 * step;
            // Need input samples up to center + half_width.
            let needed_end = (center.ceil() as u64) + self.half_width as u64;
            let available_end = self.buf_start + self.buf.len() as u64;
            if needed_end >= available_end {
                break;
            }
            out.push(self.sample_at(center));
            self.next_out += 1;
        }
        // Drop input that can no longer influence future outputs.
        let next_center = self.next_out as f64 * step;
        let keep_from = (next_center.floor() as i64 - self.half_width as i64).max(0) as u64;
        if keep_from > self.buf_start {
            let drop = (keep_from - self.buf_start) as usize;
            let drop = drop.min(self.buf.len());
            self.buf.drain(..drop);
            self.buf_start += drop as u64;
        }
        out
    }

    /// Evaluate the windowed-sinc interpolation at fractional input position
    /// `center` (absolute index).
    fn sample_at(&self, center: f64) -> f32 {
        let lo = (center.floor() as i64 - self.half_width as i64).max(self.buf_start as i64);
        let hi = (center.floor() as i64 + self.half_width as i64 + 1)
            .min((self.buf_start + self.buf.len() as u64) as i64);
        let mut acc = 0.0f64;
        for i in lo..hi {
            let x = i as f64 - center; // distance in input samples
            let w = self.kernel(x);
            let s = self.buf[(i as u64 - self.buf_start) as usize] as f64;
            acc += s * w;
        }
        acc as f32
    }

    /// Blackman-windowed sinc, normalized so the passband gain is ~1.
    fn kernel(&self, x: f64) -> f64 {
        let a = self.half_width as f64;
        if x.abs() >= a {
            return 0.0;
        }
        // sinc at the cutoff frequency
        let t = 2.0 * self.cutoff * x;
        let sinc = if t.abs() < 1e-12 {
            1.0
        } else {
            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
        };
        // Blackman window over [-a, a]
        let u = (x + a) / (2.0 * a);
        let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * u).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * u).cos();
        2.0 * self.cutoff * sinc * window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goertzel power of `freq` Hz in `samples` at `rate` Hz.
    fn goertzel(samples: &[f32], rate: f32, freq: f32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * (freq / rate) as f64;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in samples {
            let s0 = x as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2) / samples.len() as f64
    }

    fn sine(freq: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn run_resampler(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
        let mut r = Resampler::new(in_rate, out_rate);
        let mut out = Vec::new();
        // Feed in uneven chunks to exercise the streaming path.
        for chunk in input.chunks(777) {
            out.extend(r.push(chunk));
        }
        out.extend(r.flush());
        out
    }

    #[test]
    fn downmix_stereo_averages() {
        let stereo = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix_to_mono(&stereo, 2), vec![0.5, 0.5, 0.0]);
        let mono = [0.1, 0.2];
        assert_eq!(downmix_to_mono(&mono, 1), vec![0.1, 0.2]);
    }

    #[test]
    fn f32_i16_conversion_clamps() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(2.0), i16::MAX); // clamped
        assert_eq!(f32_to_i16(-2.0), -i16::MAX); // clamped
    }

    #[test]
    fn level_math() {
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
        assert!((dbfs(1.0) - 0.0).abs() < 1e-3);
        assert!((dbfs(0.5) + 6.02).abs() < 0.1);
        assert_eq!(dbfs(0.0), -120.0);
    }

    #[test]
    fn level_bar_renders() {
        let full = level_bar(0.0, 20);
        assert!(full.starts_with("[####################]"), "{full}");
        let empty = level_bar(-60.0, 20);
        assert!(empty.starts_with("[....................]"), "{empty}");
        let mid = level_bar(-30.0, 20);
        assert!(mid.contains("##########.........."), "{mid}");
    }

    #[test]
    fn downsample_48k_to_12k_preserves_tone() {
        // 1 kHz at 48 kHz -> should come out as a clean 1 kHz at 12 kHz.
        let input = sine(1000.0, 48_000, 48_000);
        let out = run_resampler(&input, 48_000, 12_000);
        // Length ~ 12000 (within kernel-tail slack).
        assert!(
            (out.len() as i64 - 12_000).unsigned_abs() < 100,
            "got {} samples",
            out.len()
        );
        // Skip the kernel warm-up tail at both ends for analysis.
        let body = &out[500..out.len() - 500];
        let p_tone = goertzel(body, 12_000.0, 1000.0);
        let p_off = goertzel(body, 12_000.0, 3500.0);
        assert!(p_tone > 0.1, "tone power too low: {p_tone}");
        assert!(p_off < p_tone * 0.01, "off-tone power: {p_off} vs {p_tone}");
    }

    #[test]
    fn downsample_rejects_aliasing() {
        // 20 kHz at 48 kHz would alias to 4 kHz at 12 kHz without filtering.
        let input = sine(20_000.0, 48_000, 48_000);
        let out = run_resampler(&input, 48_000, 12_000);
        let body = &out[500..out.len() - 500];
        let p_alias = goertzel(body, 12_000.0, 4000.0);
        // Compare against what an in-band tone delivers.
        let inband = run_resampler(&sine(4000.0, 48_000, 48_000), 48_000, 12_000);
        let p_inband = goertzel(&inband[500..inband.len() - 500], 12_000.0, 4000.0);
        assert!(
            p_alias < p_inband * 0.001,
            "alias not rejected: {p_alias} vs in-band {p_inband}"
        );
    }

    #[test]
    fn upsample_12k_to_48k_preserves_tone() {
        let input = sine(1000.0, 12_000, 12_000);
        let out = run_resampler(&input, 12_000, 48_000);
        assert!(
            (out.len() as i64 - 48_000).unsigned_abs() < 400,
            "got {} samples",
            out.len()
        );
        let body = &out[2000..out.len() - 2000];
        let p_tone = goertzel(body, 48_000.0, 1000.0);
        let p_off = goertzel(body, 48_000.0, 2500.0);
        assert!(p_tone > 0.1, "tone power too low: {p_tone}");
        assert!(p_off < p_tone * 0.01, "off-tone power: {p_off}");
    }

    #[test]
    fn non_integer_ratio_44100_to_12000() {
        let input = sine(1000.0, 44_100, 44_100);
        let out = run_resampler(&input, 44_100, 12_000);
        assert!(
            (out.len() as i64 - 12_000).unsigned_abs() < 100,
            "got {} samples",
            out.len()
        );
        let body = &out[500..out.len() - 500];
        assert!(goertzel(body, 12_000.0, 1000.0) > 0.1);
    }

    #[test]
    fn identity_passthrough() {
        let mut r = Resampler::new(48_000, 48_000);
        let input = [0.1f32, 0.2, 0.3];
        assert_eq!(r.push(&input), input.to_vec());
        assert!(r.flush().is_empty());
    }

    #[test]
    fn streaming_matches_oneshot() {
        // Chunk size must not affect output.
        let input = sine(700.0, 48_000, 24_000);
        let mut a = Resampler::new(48_000, 12_000);
        let mut one = a.push(&input);
        one.extend(a.flush());

        let mut b = Resampler::new(48_000, 12_000);
        let mut chunked = Vec::new();
        for c in input.chunks(123) {
            chunked.extend(b.push(c));
        }
        chunked.extend(b.flush());

        assert_eq!(one.len(), chunked.len());
        for (x, y) in one.iter().zip(chunked.iter()) {
            assert!((x - y).abs() < 1e-6);
        }
    }
}
