//! Encode a message to FT8/FT4 audio from the command line: write a WAV and/or
//! play it to the default output device. The encode-side counterpart to
//! `decode_wav` — together they make a full offline encode → decode loop.
//!
//! ```text
//! cargo run -p core --example encode_wav -- "CQ K1ABC FN42"
//! cargo run -p core --example encode_wav -- "CQ W4LL EM73" ft4 cq.wav
//! cargo run -p core --example encode_wav -- "CQ W4LL EM73" ft4 --play --freq=1200
//! ```
//!
//! Positional args: `<message> [ft8|ft4] [out.wav]`. Flags: `--play` (also play to
//! the system default output device), `--freq=<hz>` (audio tone offset, default
//! 1500). Pair it with `decode_wav` to confirm a message round-trips off the air.

use std::path::Path;
use std::time::Duration;

use core::Protocol;

const SAMPLE_RATE: u32 = 12_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let play = args.iter().any(|a| a == "--play");
    let freq: f32 = args
        .iter()
        .find_map(|a| a.strip_prefix("--freq="))
        .map_or(1500.0, |s| s.parse().expect("--freq=<hz> must be a number"));
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();

    let message = positional.first().copied().unwrap_or("CQ K1ABC FN42");
    let proto = match positional.get(1).copied() {
        Some("ft4") => Protocol::Ft4,
        _ => Protocol::Ft8,
    };
    let proto_str = match proto {
        Protocol::Ft8 => "ft8",
        Protocol::Ft4 => "ft4",
    };
    let default_out = format!("encoded_{proto_str}.wav");
    let out_path = positional.get(2).copied().unwrap_or(default_out.as_str());

    let samples = modes::synth_message(message, proto, freq, SAMPLE_RATE)
        .unwrap_or_else(|| panic!("cannot encode message: {message:?}"));
    let secs = samples.len() as f32 / SAMPLE_RATE as f32;
    println!(
        "{proto_str}: \"{message}\" @ {freq:.0} Hz → {} samples ({secs:.1}s slot)",
        samples.len()
    );

    audio::save_wav_mono(Path::new(out_path), &samples, SAMPLE_RATE)
        .unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    println!(
        "wrote {out_path}  (decode it: cargo run -p core --example decode_wav -- {out_path} {proto_str})"
    );

    if play {
        let out = audio::OutputStream::open(None).expect("open default output device");
        println!("playing on '{}'…", out.device_name());
        out.load(&samples, SAMPLE_RATE);
        while !out.is_done() && !out.is_dead() {
            std::thread::sleep(Duration::from_millis(100));
        }
        if out.is_dead() {
            eprintln!("warning: output device dropped out during playback");
        } else {
            // Brief grace so the device flushes its last buffer before we drop it.
            std::thread::sleep(Duration::from_millis(200));
            println!("done");
        }
    }
}
