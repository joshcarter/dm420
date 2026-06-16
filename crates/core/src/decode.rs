//! Decode pipeline: slot audio → `modes::decode` → `Decode` on the bus.
//!
//! Two sources feed the same publish path:
//! - [`spawn_wav`] replays a recording, chunked into slots, on a tokio task.
//! - [`spawn_live`] captures one slot at a time from cpal, aligned to UTC slot
//!   boundaries, on its own std thread (capture blocks for a slot duration).
//!
//! Joel's decoder wants 12 kHz mono; we downsample with his `audio::Resampler`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bus::types as t;
use bus::{BusHandle, Topic};
use modes::{Decode as ModeDecode, Protocol, decode};

use crate::parse::parse_message;

/// Decoder input rate (Joel's decoder is fixed at 12 kHz).
const DECODE_RATE: u32 = 12_000;

/// Replay pacing for WAV playback. Not real slot timing — a recording's slots are
/// emitted on this cadence so the GUI's decode rail populates promptly.
const REPLAY_INTERVAL: Duration = Duration::from_millis(1500);

/// Headroom left at the end of each slot for live capture. The FT8/FT4
/// transmission ends well before the slot boundary, so we capture `slot − this`
/// and use the remainder to re-align to the *next* boundary. Without it, a
/// full-slot capture overruns the boundary and only every other slot is grabbed.
/// Spectrum (waterfall) parameters. At 12 kHz a 1024-pt FFT gives ~11.7 Hz bins;
/// we keep the bins spanning ~0..3000 Hz to match the panel's audio-offset axis.
const FFT_SIZE: usize = 1024;
const SPECTRUM_MAX_HZ: f32 = 3000.0;
/// Seconds between spectrogram columns (a column every 50 ms ≈ 20/s — well above
/// the panel's pixel scroll rate, so the waterfall has no gaps).
const SPECTRUM_HOP_S: f64 = 0.05;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn over_air(p: Protocol) -> t::OverAirMode {
    match p {
        Protocol::Ft8 => t::OverAirMode::Ft8,
        Protocol::Ft4 => t::OverAirMode::Ft4,
    }
}

/// Resample arbitrary-rate mono audio to the decoder's 12 kHz using Joel's
/// windowed-sinc resampler.
fn resample_to_12k(samples: Vec<f32>, rate: u32) -> Vec<f32> {
    if rate == DECODE_RATE {
        return samples;
    }
    let mut r = audio::Resampler::new(rate, DECODE_RATE);
    let mut out = r.push(&samples);
    out.extend(r.flush());
    out
}

/// Load a WAV, resample to 12 kHz, and split into per-slot sample buffers.
/// Trailing partial slots shorter than half a slot are dropped.
fn load_slots(path: &Path, proto: Protocol) -> Result<Vec<Vec<f32>>, audio::AudioError> {
    let (samples, rate) = audio::load_wav_mono(path)?;
    let mono12 = resample_to_12k(samples, rate);
    let per = (modes::slot_period(proto) * DECODE_RATE as f64) as usize;
    if per == 0 {
        return Ok(Vec::new());
    }
    Ok(mono12
        .chunks(per)
        .filter(|c| c.len() > per / 2)
        .map(<[f32]>::to_vec)
        .collect())
}

/// Build and publish one scrolling-spectrogram column (a single-window FFT over
/// the latest `FFT_SIZE` samples of `win`) onto the spectrum topic.
fn publish_column(
    bus: &BusHandle,
    radio: &t::RadioId,
    proto: Protocol,
    win: &[f32],
    bin_hz: f32,
    max_bins: usize,
) {
    let row = t::SpectrumRow {
        radio: radio.clone(),
        mode: over_air(proto),
        t: t::Timestamp(now_ms()),
        bin0_offset: t::OffsetHz(0.0),
        bin_hz,
        mags: dsp::spectrum_column(win, FFT_SIZE, max_bins),
        source: t::SignalSource::Received,
    };
    let _ = bus.publish(&Topic::Spectrum(radio.clone()), row);
}

/// Build dm420 `Decode`s from one slot's `modes::decode` output and publish them
/// on the lossless decodes stream. `slot_start_ms` is when the slot's audio *began*
/// arriving (not when decoding finished), so the GUI can place each decode at the
/// horizontal position matching where its audio sits on the live spectrogram.
fn publish_slot(
    bus: &BusHandle,
    radio: &t::RadioId,
    proto: Protocol,
    slot_start_ms: i64,
    decs: Vec<ModeDecode>,
) {
    let ms = slot_start_ms;
    let slot_ms = (modes::slot_period(proto) * 1000.0) as i64;
    let slot = t::SlotId(ms.div_euclid(slot_ms.max(1)) as u64);
    let mode = over_air(proto);

    for d in decs {
        let msg = t::Decode {
            radio: radio.clone(),
            mode,
            t: t::Timestamp(ms),
            offset: t::OffsetHz(d.freq_hz),
            snr_db: Some(d.snr_db.round() as i8),
            source: t::SignalSource::Received,
            content: t::DecodeContent::Slotted {
                slot,
                dt: d.dt,
                message: parse_message(&d.message),
            },
        };
        let _ = bus.publish(&Topic::Decodes(radio.clone()), msg);
    }
}

/// Replay a WAV recording onto the decodes topic.
pub fn spawn_wav(bus: &BusHandle, radio: t::RadioId, path: PathBuf, proto: Protocol, looping: bool) {
    let bus = bus.clone();
    tokio::spawn(async move {
        let slots = match tokio::task::spawn_blocking(move || load_slots(&path, proto)).await {
            Ok(Ok(s)) if !s.is_empty() => s,
            Ok(Ok(_)) => {
                tracing::warn!("decode wav: no full slots in recording");
                return;
            }
            Ok(Err(e)) => {
                tracing::error!("decode wav load failed: {e}");
                return;
            }
            Err(_) => return,
        };

        let mut tick = tokio::time::interval(REPLAY_INTERVAL);
        loop {
            for slot in &slots {
                tick.tick().await;
                let samples = slot.clone();
                let decs = tokio::task::spawn_blocking(move || decode(&samples, DECODE_RATE, proto))
                    .await
                    .unwrap_or_default();
                publish_slot(&bus, &radio, proto, now_ms(), decs);
            }
            if !looping {
                break;
            }
        }
    });
}

/// Live cpal capture, one slot at a time, aligned to UTC slot boundaries.
pub fn spawn_live(bus: &BusHandle, radio: t::RadioId, input: Option<String>, proto: Protocol) {
    let bus = bus.clone();
    std::thread::spawn(move || {
        // One persistent capture stream feeds two consumers: a steady spectrogram
        // column stream, and slot-aligned buffers for the decoder.
        let stream = match audio::capture_stream(input) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("decode live capture failed to start: {e}");
                return;
            }
        };
        let rate = stream.sample_rate;
        let mut resampler = audio::Resampler::new(rate, DECODE_RATE);

        let bin_hz = DECODE_RATE as f32 / FFT_SIZE as f32;
        let max_bins = (SPECTRUM_MAX_HZ / bin_hz).ceil() as usize;
        let hop = ((DECODE_RATE as f64) * SPECTRUM_HOP_S).max(1.0) as usize;

        // Rolling FFT window (latest FFT_SIZE samples) + a hop counter.
        let mut win: Vec<f32> = Vec::with_capacity(FFT_SIZE * 2);
        let mut hop_acc: usize = 0;
        // Audio for the slot currently in progress, plus the slot we're inside.
        let mut slot_buf: Vec<f32> = Vec::new();
        let mut slot_start = modes::current_slot_start(now_unix(), proto);

        loop {
            let chunk = match stream.recv_timeout(Duration::from_millis(500)) {
                Some(c) => c,
                None => continue, // no audio this tick; the device may be warming up
            };
            // Resample the device's rate to the decoder's fixed 12 kHz.
            let s12 = if rate == DECODE_RATE {
                chunk
            } else {
                resampler.push(&chunk)
            };
            if s12.is_empty() {
                continue;
            }

            // --- spectrogram columns: emit one every `hop` samples (~50 ms) ---
            win.extend_from_slice(&s12);
            if win.len() > FFT_SIZE {
                let drop = win.len() - FFT_SIZE;
                win.drain(0..drop);
            }
            hop_acc += s12.len();
            if win.len() == FFT_SIZE && hop_acc >= hop {
                hop_acc = 0;
                publish_column(&bus, &radio, proto, &win, bin_hz, max_bins);
            }

            // --- slot-aligned decode: when wall-clock crosses a boundary, decode
            // the slot that just ended (off-thread so it never stalls capture) ---
            slot_buf.extend_from_slice(&s12);
            let cur_slot = modes::current_slot_start(now_unix(), proto);
            if cur_slot != slot_start {
                // `slot_buf` holds the slot that just ended; stamp its decodes with
                // that slot's *start* time (when its audio began arriving), so the
                // text lands where its audio now sits on the scrolling spectrogram.
                let slot_start_ms = (slot_start * 1000.0) as i64;
                slot_start = cur_slot;
                let audio = std::mem::take(&mut slot_buf);
                let bus = bus.clone();
                let radio = radio.clone();
                std::thread::spawn(move || {
                    let decs = decode(&audio, DECODE_RATE, proto);
                    // Proof-of-life: real capture → real decoder → real bus publish.
                    let summary: Vec<String> = decs
                        .iter()
                        .map(|d| format!("{} ({:+} dB)", d.message.trim(), d.snr_db.round() as i32))
                        .collect();
                    eprintln!(
                        "[live decode] slot {} s of audio -> {} decode(s): {}",
                        audio.len() / DECODE_RATE as usize,
                        decs.len(),
                        summary.join(", ")
                    );
                    publish_slot(&bus, &radio, proto, slot_start_ms, decs);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end over the bundled decoder fixture: load → resample → slot →
    /// decode → build a `Decode`, and confirm the parse seam yields structure.
    #[test]
    fn decodes_fixture_wav() {
        // A known-good single-signal FT8 fixture vendored with the decoder.
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../modes/tests/fixtures/cq_k1abc_1000.wav");
        let slots = load_slots(&path, Protocol::Ft8).expect("load fixture");
        assert!(!slots.is_empty(), "fixture produced no slots");

        let decs: Vec<_> = slots
            .iter()
            .flat_map(|s| decode(s, DECODE_RATE, Protocol::Ft8))
            .collect();
        assert!(!decs.is_empty(), "decoder found nothing in fixture");

        // At least one decode parses into a structured (non-Raw) message.
        let structured = decs
            .iter()
            .any(|d| !matches!(parse_message(&d.message), t::ParsedMessage::Raw(_)));
        assert!(structured, "no decode produced a structured ParsedMessage");
    }
}
