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

/// Build dm420 `Decode`s from one slot's `modes::decode` output and publish them
/// on the lossless decodes stream.
fn publish_slot(bus: &BusHandle, radio: &t::RadioId, proto: Protocol, decs: Vec<ModeDecode>) {
    let ms = now_ms();
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
                publish_slot(&bus, &radio, proto, decs);
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
        let slot_dur = Duration::from_secs_f64(modes::slot_period(proto));
        loop {
            // Sleep to just past the next slot boundary, then capture a full slot.
            let wait = modes::seconds_until_next_slot(now_unix(), proto);
            std::thread::sleep(Duration::from_secs_f64(wait));
            match audio::capture_window(input.clone(), slot_dur) {
                Ok((samples, rate)) => {
                    let mono12 = resample_to_12k(samples, rate);
                    let decs = decode(&mono12, DECODE_RATE, proto);
                    publish_slot(&bus, &radio, proto, decs);
                }
                Err(e) => {
                    tracing::error!("decode live capture failed: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                }
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
