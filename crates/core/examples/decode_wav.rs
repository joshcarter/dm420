//! Decode a WAV recording from the command line and print the results — a quick
//! sanity check on the audio → resample → decode → parse path without the GUI.
//!
//! ```text
//! cargo run -p core --example decode_wav -- sample_data/ft8/rec_20260614_015943Z.wav
//! ```
//!
//! Defaults to the bundled FT8 sample if no path is given. Add `ft4` as a second
//! arg to decode FT4.

use std::path::PathBuf;

use core::{Protocol, parse_message};

const DECODE_RATE: u32 = 12_000;
const DEFAULT_WAV: &str = "sample_data/ft8/rec_20260614_015943Z.wav";

fn resample_to_12k(samples: Vec<f32>, rate: u32) -> Vec<f32> {
    if rate == DECODE_RATE {
        return samples;
    }
    let mut r = audio::Resampler::new(rate, DECODE_RATE);
    let mut out = r.push(&samples);
    out.extend(r.flush());
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map_or_else(|| PathBuf::from(DEFAULT_WAV), PathBuf::from);
    let proto = match args.next().as_deref() {
        Some("ft4") => Protocol::Ft4,
        _ => Protocol::Ft8,
    };

    let (samples, rate) = audio::load_wav_mono(&path).expect("load wav");
    let mono12 = resample_to_12k(samples, rate);
    let per = (modes::slot_period(proto) * DECODE_RATE as f64) as usize;
    let secs = mono12.len() as f64 / DECODE_RATE as f64;
    println!(
        "{}: {rate} Hz in, {} samples @ 12 kHz ({secs:.1}s, ~{} slots)",
        path.display(),
        mono12.len(),
        mono12.len() / per.max(1),
    );

    let mut total = 0usize;
    for (i, slot) in mono12.chunks(per).enumerate() {
        if slot.len() <= per / 2 {
            continue;
        }
        let decs = modes::decode(slot, DECODE_RATE, proto);
        if decs.is_empty() {
            continue;
        }
        println!("── slot {i} · {} decode(s) ──", decs.len());
        for d in &decs {
            total += 1;
            println!(
                "   {:>3} dB  {:>5.0} Hz  {:<20}  {:?}",
                d.snr_db.round(),
                d.freq_hz,
                d.message,
                parse_message(&d.message),
            );
        }
    }
    println!("total decodes: {total}");
}
