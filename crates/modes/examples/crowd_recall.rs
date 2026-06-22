//! `crowd_recall` — Half A of the decoder measurement harness.
//!
//! Plant *N* known FT8 signals at controlled SNRs into a single 15 s slot, decode
//! the slot, and report **recall** (decoded / planted) — overall and bucketed by
//! SNR and by spectral crowding. Because we author the scene, ground truth is
//! exact: no reference decoder is needed. This isolates the busy-band question —
//! turn the crowding knob (`--n`) and the SNR knob (`--snr-min/--snr-max`)
//! independently and watch where recall falls off, and whether the misses are
//! *weak* (→ OSD) or *masked by a louder neighbor* (→ subtraction).
//!
//! The planted SNR uses the WSJT-X 2500 Hz reference-bandwidth convention, so the
//! numbers line up roughly with what `jt9` reports — but Half B (A/B vs. `jt9` on
//! real recordings) is the absolute calibration; treat these dB as internally
//! consistent, not gospel.
//!
//! FT8 only by design: this harness plants FT8 scenes (it passes `Protocol::Ft8`
//! to `synth_message`/`decode`). FT4 synth now exists, so an FT4 variant is possible.
//!
//! Run:
//!   cargo run -p modes --example crowd_recall -- --n 40 --snr-min -18 --snr-max 0
//!   cargo run -p modes --example crowd_recall -- --wav captured_slot.wav   # just decode a real slot

use modes::{Protocol, decode, synth_message};
use std::collections::HashSet;

const SR: u32 = 12_000;
const SLOT_SECS: f32 = 15.0;
const BAND_LO: f32 = 300.0;
const BAND_HI: f32 = 2700.0;
/// FT8 SNR is referenced to a 2500 Hz noise bandwidth.
const REF_BW: f32 = 2500.0;

/// Deterministic splitmix64 PRNG — reproducible scenes from a seed, no `rand` dep.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Standard-normal sample (Box–Muller).
    fn gauss(&mut self) -> f32 {
        let u1 = self.unit().max(1e-9);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

/// One planted transmission and how it was placed.
struct Plant {
    msg: String,
    freq: f32,
    snr: f32,
    /// Hz to the nearest other plant (smaller = more crowded).
    nn_dist: f32,
    /// True if that nearest neighbor is louder than this one (masking risk).
    nn_louder: bool,
}

/// Mean square of the active (non-silent) region of a synthesized slot — its
/// average transmit power, the numerator of the SNR.
fn signal_power(sig: &[f32]) -> f32 {
    let (mut sum, mut cnt) = (0.0f64, 0u64);
    for &s in sig {
        if s.abs() > 1e-6 {
            sum += (s as f64) * (s as f64);
            cnt += 1;
        }
    }
    if cnt == 0 { 0.0 } else { (sum / cnt as f64) as f32 }
}

/// A guaranteed-valid, distinct standard callsign for scene index `i`:
/// `K` + digit + three suffix letters encoding `i` in base 26 (e.g. `K1ABC`).
fn gen_call(i: usize) -> String {
    let l = |x: usize| (b'A' + (x % 26) as u8) as char;
    let a = i % 26;
    let b = (i / 26) % 26;
    let c = (i / 676) % 26;
    format!("K1{}{}{}", l(a), l(b), l(c))
}

/// A valid 4-char Maidenhead grid derived deterministically from `i`.
fn gen_grid(i: usize) -> String {
    let fld = |x: usize| (b'A' + (x % 18) as u8) as char; // A..R
    let dig = |x: usize| (b'0' + (x % 10) as u8) as char;
    format!("{}{}{}{}", fld(i * 7 + 3), fld(i * 5 + 11), dig(i), dig(i / 10))
}

struct Args {
    n: usize,
    snr_min: f32,
    snr_max: f32,
    seed: u64,
    noise_var: f32,
    wav: Option<String>,
    write_wav: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        n: 30,
        snr_min: -18.0,
        snr_max: 0.0,
        seed: 0x0D_F420,
        noise_var: 1.0,
        wav: None,
        write_wav: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let val = || argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--n" => a.n = val().parse().unwrap_or(a.n),
            "--snr-min" => a.snr_min = val().parse().unwrap_or(a.snr_min),
            "--snr-max" => a.snr_max = val().parse().unwrap_or(a.snr_max),
            "--seed" => a.seed = val().parse().unwrap_or(a.seed),
            "--noise" => a.noise_var = val().parse().unwrap_or(a.noise_var),
            "--wav" => a.wav = Some(val()),
            "--write-wav" => a.write_wav = Some(val()),
            other => eprintln!("ignoring unknown arg: {other}"),
        }
        i += if argv[i].starts_with("--") { 2 } else { 1 };
    }
    a
}

fn main() {
    let args = parse_args();
    if let Some(path) = &args.wav {
        decode_real_wav(path);
        return;
    }
    run_synthetic(&args);
}

/// Convenience: decode a captured slot and dump what we get, so you can eyeball a
/// real WAV before wiring up the `jt9` A/B (Half B).
fn decode_real_wav(path: &str) {
    let (sig, sr) = read_wav_mono(path);
    let decodes = decode(&sig, sr, Protocol::Ft8);
    println!("{path}: {sr} Hz, {} decode(s)", decodes.len());
    let mut d: Vec<_> = decodes.iter().collect();
    d.sort_by(|a, b| a.freq_hz.partial_cmp(&b.freq_hz).unwrap());
    for x in d {
        println!("  {:>5.0} Hz  {:+3.0} dB  {}", x.freq_hz, x.snr_db, x.message);
    }
}

fn run_synthetic(args: &Args) {
    let n = args.n.max(1);
    let mut rng = Rng::new(args.seed);

    // Reference transmit power of a unit-amplitude synthesized signal.
    let p_unit = signal_power(&synth_message("CQ K1ABC FN42", Protocol::Ft8, 1500.0, SR).unwrap());
    // White-noise power that lands in the 2500 Hz FT8 reference band.
    let noise_2500 = args.noise_var * REF_BW / (SR as f32 / 2.0);

    // SNRs: loudest (index 0) → weakest, evenly spaced across the requested range.
    let snr_of = |i: usize| {
        if n == 1 {
            args.snr_max
        } else {
            args.snr_max - (args.snr_max - args.snr_min) * i as f32 / (n - 1) as f32
        }
    };

    // Frequencies: spread across the band with jitter; then deliberately crowd a
    // fraction of the *weaker* signals to within a few Hz of a *louder* one, so the
    // scene probes masking, not just weak-signal sensitivity.
    let mut freq = vec![0.0f32; n];
    for (i, f) in freq.iter_mut().enumerate() {
        let base = BAND_LO + (BAND_HI - BAND_LO) * (i as f32 + 0.5) / n as f32;
        *f = base + (rng.unit() - 0.5) * 12.0;
    }
    for i in (0..n).filter(|i| i % 5 == 4) {
        let louder = (i / 2).min(i.saturating_sub(1)); // some lower (louder) index
        let off = (2.0 + rng.unit() * 4.0) * if rng.unit() < 0.5 { -1.0 } else { 1.0 };
        freq[i] = (freq[louder] + off).clamp(BAND_LO, BAND_HI);
    }

    // Synthesize, scale to target SNR, and sum into one slot (plus shared noise).
    let n_samp = (SR as f32 * SLOT_SECS) as usize;
    let mut slot = vec![0.0f32; n_samp];
    let mut plants: Vec<Plant> = Vec::new();
    for (i, &f) in freq.iter().enumerate() {
        let msg = format!("CQ {} {}", gen_call(i), gen_grid(i));
        let Some(sig) = synth_message(&msg, Protocol::Ft8, f, SR) else {
            continue; // unencodable — skip; recall is scored against placed plants
        };
        let snr = snr_of(i);
        let amp = (10f32.powf(snr / 10.0) * noise_2500 / p_unit).sqrt();
        for (d, s) in slot.iter_mut().zip(&sig) {
            *d += amp * s;
        }
        plants.push(Plant { msg, freq: f, snr, nn_dist: f32::MAX, nn_louder: false });
    }
    for s in slot.iter_mut() {
        *s += args.noise_var.sqrt() * rng.gauss();
    }

    // Optionally dump the exact scene to a 12 kHz/16-bit/mono WAV so the *same*
    // crowded slot can be fed to both decoders via the `ab_jt9` example.
    if let Some(path) = &args.write_wav {
        write_wav_mono(path, &slot, SR);
        println!("wrote {} ({} planted signals) → feed to ab_jt9\n", path, plants.len());
    }

    // Annotate each plant with its nearest-neighbor distance + whether that
    // neighbor is louder (the masking condition).
    for a in 0..plants.len() {
        for b in 0..plants.len() {
            if a == b {
                continue;
            }
            let d = (plants[a].freq - plants[b].freq).abs();
            if d < plants[a].nn_dist {
                plants[a].nn_dist = d;
                plants[a].nn_louder = plants[b].snr > plants[a].snr;
            }
        }
    }

    // Decode and score.
    let decodes = decode(&slot, SR, Protocol::Ft8);
    let hit: HashSet<&str> = decodes.iter().map(|d| d.message.as_str()).collect();

    println!(
        "scene: {} planted  |  seed {}  noise σ²={:.2} (≈{:.1} dB in 2500 Hz ref)  |  SNR {:.0}..{:.0} dB",
        plants.len(),
        args.seed,
        args.noise_var,
        10.0 * noise_2500.log10(),
        args.snr_min,
        args.snr_max,
    );
    println!("decoded: {} total\n", decodes.len());

    // Per-signal table, weakest first (where the action is).
    let mut order: Vec<usize> = (0..plants.len()).collect();
    order.sort_by(|&a, &b| plants[a].snr.partial_cmp(&plants[b].snr).unwrap());
    println!("  {:<6} {:>7} {:>6} {:>8}  message", "hit", "freq", "snr", "nn");
    let mut recovered = 0usize;
    for &i in &order {
        let p = &plants[i];
        let got = hit.contains(p.msg.as_str());
        recovered += got as usize;
        let nn = if p.nn_dist == f32::MAX {
            "  —".to_string()
        } else {
            format!("{:.0}Hz{}", p.nn_dist, if p.nn_louder { "↑" } else { " " })
        };
        println!(
            "  {:<6} {:>5.0}Hz {:>+5.0} {:>8}  {}",
            if got { "✓" } else { "✗ MISS" },
            p.freq,
            p.snr,
            nn,
            p.msg,
        );
    }

    // Buckets: recall vs SNR, and vs crowding (and the masking sub-case).
    println!("\n== recall by SNR ==");
    for (lo, hi, label) in [
        (f32::NEG_INFINITY, -15.0, "       ≤ -15 dB"),
        (-15.0, -10.0, "  -15 .. -10 dB"),
        (-10.0, -5.0, "  -10 ..  -5 dB"),
        (-5.0, f32::INFINITY, "       > -5 dB "),
    ] {
        bucket(&plants, &hit, label, |p| p.snr >= lo && p.snr < hi);
    }

    println!("\n== recall by crowding ==");
    bucket(&plants, &hit, "  isolated >20Hz", |p| p.nn_dist > 20.0);
    bucket(&plants, &hit, "  near 5..20 Hz ", |p| p.nn_dist > 5.0 && p.nn_dist <= 20.0);
    bucket(&plants, &hit, "  overlap ≤5 Hz ", |p| p.nn_dist <= 5.0);
    bucket(&plants, &hit, "  masked (≤10Hz, louder nbr)", |p| {
        p.nn_dist <= 10.0 && p.nn_louder
    });

    let pct = 100.0 * recovered as f32 / plants.len().max(1) as f32;
    println!("\nrecall: {recovered}/{} = {pct:.0}%", plants.len());
}

fn bucket(plants: &[Plant], hit: &HashSet<&str>, label: &str, pred: impl Fn(&Plant) -> bool) {
    let sel: Vec<&Plant> = plants.iter().filter(|p| pred(p)).collect();
    if sel.is_empty() {
        println!("  {label:<28} (none)");
        return;
    }
    let got = sel.iter().filter(|p| hit.contains(p.msg.as_str())).count();
    println!(
        "  {label:<28} {got:>3}/{:<3} = {:>3.0}%",
        sel.len(),
        100.0 * got as f32 / sel.len() as f32
    );
}

/// Write a canonical 16-bit PCM mono WAV. Peak-normalized to 0.9 full-scale so
/// summed signals don't clip — uniform scaling preserves every signal's SNR.
fn write_wav_mono(path: &str, samples: &[f32], sample_rate: u32) {
    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs())).max(1e-9);
    let gain = 0.9 / peak;
    let data_len = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_len);
    let mut tag = |b: &[u8]| buf.extend_from_slice(b);
    tag(b"RIFF");
    tag(&((36 + data_len) as u32).to_le_bytes());
    tag(b"WAVE");
    tag(b"fmt ");
    tag(&16u32.to_le_bytes()); // PCM fmt chunk size
    tag(&1u16.to_le_bytes()); // PCM
    tag(&1u16.to_le_bytes()); // mono
    tag(&sample_rate.to_le_bytes());
    tag(&(sample_rate * 2).to_le_bytes()); // byte rate
    tag(&2u16.to_le_bytes()); // block align
    tag(&16u16.to_le_bytes()); // bits/sample
    tag(b"data");
    tag(&(data_len as u32).to_le_bytes());
    for &s in samples {
        let q = (s * gain * 32767.0).clamp(-32768.0, 32767.0) as i16;
        buf.extend_from_slice(&q.to_le_bytes());
    }
    std::fs::write(path, buf).unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Minimal canonical-WAV reader (16-bit PCM mono), mirroring the one in
/// `tests/fixtures_decode.rs` — no external crates.
fn read_wav_mono(path: &str) -> (Vec<f32>, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let mut i = 12;
    let (data_off, data_len) = loop {
        let id = &bytes[i..i + 4];
        let sz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if id == b"data" {
            break (i + 8, sz);
        }
        i += 8 + sz;
    };
    let samples = bytes[data_off..data_off + data_len]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    (samples, sample_rate)
}
