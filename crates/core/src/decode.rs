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
use modes::{CallHash, Decode as ModeDecode, Protocol, decode, decode_streaming};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::control::{AudioControl, StopReason, sleep_or_changed};
use crate::health;
use crate::parse::parse_message;

/// Decoder input rate (Joel's decoder is fixed at 12 kHz).
const DECODE_RATE: u32 = 12_000;

/// Seconds of post-signal silence at the end of a slot. The FT8/FT4 transmission
/// finishes this far before the boundary, so the early (signal-end) decode pass
/// fires `slot − this` into the slot — soon enough to answer in the next slot,
/// while the boundary pass still catches anything with a late DT. Tunable: smaller
/// = decodes appear later but the early pass catches more stations.
const DECODE_TAIL: f64 = 1.5;

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

/// How long the capture can deliver no samples before we treat the device as
/// gone and rebuild the stream. A live audio device delivers buffers
/// continuously (silence is still frames), so a gap this long means it died or
/// was unplugged. Generous enough to ride out device warm-up at session start.
const AUDIO_SILENCE_TIMEOUT: Duration = Duration::from_secs(3);

/// Reconnect backoff for the capture stream: quick first retry (a momentary
/// glitch recovers fast), capped so an absent device doesn't spin.
const AUDIO_BACKOFF_START: Duration = Duration::from_secs(1);
const AUDIO_BACKOFF_MAX: Duration = Duration::from_secs(15);

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn over_air(p: Protocol) -> t::OverAirMode {
    match p {
        Protocol::Ft8 => t::OverAirMode::Ft8,
        Protocol::Ft4 => t::OverAirMode::Ft4,
    }
}

/// Map an over-air mode to the modes-crate protocol, or `None` for a mode with no
/// waveform synthesizer / decoder (PSK31, RTTY). The inverse of [`over_air`].
pub(crate) fn protocol_of(mode: t::OverAirMode) -> Option<Protocol> {
    match mode {
        t::OverAirMode::Ft8 => Some(Protocol::Ft8),
        t::OverAirMode::Ft4 => Some(Protocol::Ft4),
        _ => None,
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
        t: t::Timestamp(types::now_ms()),
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
    for d in decs {
        publish_one(bus, radio, proto, slot_start_ms, d);
    }
}

/// Publish a single decode onto the lossless decodes stream. The live path calls
/// this per-decode as the decoder streams them, so each lands on the bus the
/// instant it's found (rather than waiting for the whole slot's batch).
fn publish_one(
    bus: &BusHandle,
    radio: &t::RadioId,
    proto: Protocol,
    slot_start_ms: i64,
    d: ModeDecode,
) {
    let ms = slot_start_ms;
    let slot_ms = (modes::slot_period(proto) * 1000.0) as i64;
    let slot = t::SlotId(ms.div_euclid(slot_ms.max(1)) as u64);
    // Audit trail symmetric with the TX log (`core::tx: audio-tx: begin over …`):
    // one line per decode that reaches the bus, so a QSO can be reconstructed end
    // to end from the log. Live decodes are deduped before they get here, so this
    // logs each distinct received message once per slot.
    tracing::info!(
        "decode: slot={} mode={:?} offset={:.1} snr={:+} dt={:+.1} {}",
        slot.0,
        over_air(proto),
        d.freq_hz,
        d.snr_db.round() as i64,
        d.dt,
        d.message,
    );
    let msg = t::Decode {
        radio: radio.clone(),
        mode: over_air(proto),
        t: t::Timestamp(ms),
        offset: t::OffsetHz(d.freq_hz),
        snr_db: Some(d.snr_db.round() as i8),
        source: t::SignalSource::Received,
        content: t::DecodeContent::Slotted {
            slot,
            dt: d.dt,
            message: parse_message(&d.message),
            raw: d.message,
        },
    };
    let _ = bus.publish(&Topic::Decodes(radio.clone()), msg);
}

/// Decode `audio` for the slot starting at `slot_start_ms` on its own thread,
/// streaming each *new* decode onto the bus. `published` dedups by message text
/// within a slot, so the boundary (full) pass never re-publishes what the
/// signal-end (early) pass already sent. `cleanup` drops the slot's dedup set when
/// the pass finishes (set on the final boundary pass).
#[allow(clippy::too_many_arguments)]
fn spawn_decode_pass(
    bus: BusHandle,
    radio: t::RadioId,
    proto: Protocol,
    audio: Vec<f32>,
    slot_start_ms: i64,
    published: Arc<Mutex<HashMap<i64, HashSet<String>>>>,
    call_hash: Arc<Mutex<CallHash>>,
    pass: &'static str,
    cleanup: bool,
) {
    std::thread::spawn(move || {
        let secs = audio.len() / DECODE_RATE as usize;
        let mut n = 0usize;
        // Decode against a snapshot of the session callsign table so the CPU-heavy
        // decode stays lock-free, then fold what this slot learned back in. The
        // snapshot lets a compound call heard earlier (e.g. `CQ W1AW/0`) resolve a
        // hashed `<...>` reply that lands in a later slot — a fresh-per-slot table
        // could only resolve calls that recur as a literal within the same slot.
        let mut hash = call_hash.lock().unwrap().clone();
        decode_streaming(&audio, DECODE_RATE, proto, &mut hash, |d| {
            // Insert under the lock, publish outside it (the guard drops at the `;`).
            let fresh = published
                .lock()
                .unwrap()
                .entry(slot_start_ms)
                .or_default()
                .insert(d.message.clone());
            if fresh {
                publish_one(&bus, &radio, proto, slot_start_ms, d);
                n += 1;
            }
        });
        call_hash.lock().unwrap().merge_from(&hash);
        if cleanup {
            published.lock().unwrap().remove(&slot_start_ms);
        }
        tracing::debug!("live decode [{pass}]: {secs} s of audio -> {n} new decode(s)");
    });
}

/// Replay a WAV recording onto the decodes topic.
pub fn spawn_wav(
    bus: &BusHandle,
    radio: t::RadioId,
    path: PathBuf,
    proto: Protocol,
    looping: bool,
) {
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
                let decs =
                    tokio::task::spawn_blocking(move || decode(&samples, DECODE_RATE, proto))
                        .await
                        .unwrap_or_default();
                publish_slot(&bus, &radio, proto, types::now_ms(), decs);
            }
            if !looping {
                break;
            }
        }
    });
}

/// Live cpal capture, one slot at a time, aligned to UTC slot boundaries.
///
/// Supervised: if the device is absent at startup or disappears mid-session, the
/// capture stream is rebuilt with backoff and the fault is reported on
/// `health/audio`, so decoding resumes when the device returns without taking the
/// app down. Each session starts from a clean spectrogram/slot state.
pub fn spawn_live(bus: &BusHandle, radio: t::RadioId, control: Arc<AudioControl>) {
    let bus = bus.clone();
    std::thread::spawn(move || {
        let mut last_health: Option<t::HealthState> = None;
        let mut backoff = AUDIO_BACKOFF_START;

        loop {
            // Snapshot the live settings (and their generation) for this session.
            let cfg_gen = control.generation();
            let (input, proto) = control.snapshot();

            match audio::capture_stream(input) {
                Ok(stream) => {
                    // A session runs until the device stops delivering samples or
                    // the config changes.
                    let end = run_stream(
                        &bus,
                        &radio,
                        proto,
                        &stream,
                        &mut last_health,
                        &control,
                        cfg_gen,
                    );
                    drop(stream);
                    // A session that actually delivered audio earns a prompt retry;
                    // one that never started keeps backing off.
                    if end.ever_healthy {
                        backoff = AUDIO_BACKOFF_START;
                    }
                    match end.reason {
                        StopReason::LinkLost => set_audio_health(
                            &bus,
                            &mut last_health,
                            t::HealthState::Down("audio capture stopped — reconnecting".into()),
                        ),
                        StopReason::Reconfigured => set_audio_health(
                            &bus,
                            &mut last_health,
                            t::HealthState::Degraded("applying new settings…".into()),
                        ),
                    }
                }
                Err(e) => {
                    tracing::warn!("audio capture failed to start: {e}");
                    set_audio_health(
                        &bus,
                        &mut last_health,
                        t::HealthState::Down(format!("audio device unavailable: {e}")),
                    );
                }
            }

            // Back off before retrying, cut short if the operator changed settings.
            sleep_or_changed(backoff, || control.generation(), cfg_gen);
            backoff = (backoff * 2).min(AUDIO_BACKOFF_MAX);
        }
    });
}

/// Outcome of one capture session: whether it ever delivered audio (for backoff)
/// and why it ended (device lost vs. a config change).
struct SessionEnd {
    ever_healthy: bool,
    reason: StopReason,
}

/// Publish an audio health transition (deduplicated; see [`health::set`]).
fn set_audio_health(bus: &BusHandle, last: &mut Option<t::HealthState>, state: t::HealthState) {
    health::set(bus, t::SubsystemId::Audio, last, state);
}

/// Run one capture session: pump samples into the spectrogram + slot decoder
/// until the device delivers nothing for [`AUDIO_SILENCE_TIMEOUT`] (device lost)
/// or the config generation moves off `start_gen` (reconfigured). Returns whether
/// the device was ever healthy and why it stopped. All per-session state is
/// local, so a reconnect starts the spectrogram and slot alignment fresh.
fn run_stream(
    bus: &BusHandle,
    radio: &t::RadioId,
    proto: Protocol,
    stream: &audio::CaptureStream,
    last_health: &mut Option<t::HealthState>,
    control: &AudioControl,
    start_gen: u64,
) -> SessionEnd {
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
    // Two-pass decode: a speculative early pass at signal-end + the full pass at the
    // boundary. `early_trigger` is seconds-into-slot when the transmission is done
    // (slot minus the trailing silence); `published` dedups the two passes by message
    // text, per slot. (Adding more trigger points here scales to N passes for free —
    // the dedup makes extra passes only publish what's newly decodable.)
    let early_trigger = modes::slot_period(proto) - DECODE_TAIL;
    let mut early_decoded = false;
    let published: Arc<Mutex<HashMap<i64, HashSet<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    // Session-lived callsign hash table, shared across this session's slots so a
    // hashed `<...>` reference resolves to a call heard in an earlier slot. Like
    // the spectrogram and slot alignment, it's per-session state — a reconnect
    // starts fresh.
    let call_hash: Arc<Mutex<CallHash>> = Arc::new(Mutex::new(CallHash::new()));

    let mut ever_healthy = false;
    let mut silent_for = Duration::ZERO;
    let tick = Duration::from_millis(500);

    loop {
        // A config edit ends the session so the next one opens the new device/mode.
        if control.generation() != start_gen {
            return SessionEnd {
                ever_healthy,
                reason: StopReason::Reconfigured,
            };
        }
        let chunk = match stream.recv_timeout(tick) {
            Some(c) => c,
            None => {
                // No audio this tick. A live device always delivers buffers, so a
                // long gap means it's gone — end the session so we reconnect.
                silent_for += tick;
                if silent_for >= AUDIO_SILENCE_TIMEOUT {
                    tracing::warn!(
                        "audio: no samples for {:?}; treating device as lost",
                        AUDIO_SILENCE_TIMEOUT
                    );
                    return SessionEnd {
                        ever_healthy,
                        reason: StopReason::LinkLost,
                    };
                }
                continue;
            }
        };
        silent_for = Duration::ZERO;
        ever_healthy = true;
        set_audio_health(bus, last_health, t::HealthState::Healthy);

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
            publish_column(bus, radio, proto, &win, bin_hz, max_bins);
        }

        // --- slot-aligned decode (two-pass, off-thread so it never stalls capture).
        // EARLY pass at signal-end (~13.5 s into an FT8 slot) gets on-time stations —
        // incl. a CQ being worked — to the UI + engine *before* the boundary; the FULL
        // pass at the boundary re-decodes the whole slot and adds any late-DT / weak
        // signals the early pass missed (deduped against it). Degrades to today's
        // behavior if the early pass finds nothing. Decodes are stamped with the slot's
        // *start* time so text lands where its audio sits on the scrolling spectrogram.
        slot_buf.extend_from_slice(&s12);
        let now = now_unix();
        let cur_slot = modes::current_slot_start(now, proto);
        if cur_slot != slot_start {
            let slot_start_ms = (slot_start * 1000.0) as i64;
            slot_start = cur_slot;
            early_decoded = false;
            let audio = std::mem::take(&mut slot_buf);
            spawn_decode_pass(
                bus.clone(),
                radio.clone(),
                proto,
                audio,
                slot_start_ms,
                published.clone(),
                call_hash.clone(),
                "full",
                true,
            );
        } else if !early_decoded && modes::time_into_slot(now, proto) >= early_trigger {
            early_decoded = true;
            let slot_start_ms = (slot_start * 1000.0) as i64;
            let audio = slot_buf.clone();
            spawn_decode_pass(
                bus.clone(),
                radio.clone(),
                proto,
                audio,
                slot_start_ms,
                published.clone(),
                call_hash.clone(),
                "early",
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_over_air_mapping_round_trips() {
        // FT8/FT4 map both ways; modes with no synth/decoder map to None.
        assert_eq!(protocol_of(t::OverAirMode::Ft8), Some(Protocol::Ft8));
        assert_eq!(protocol_of(t::OverAirMode::Ft4), Some(Protocol::Ft4));
        assert_eq!(protocol_of(t::OverAirMode::Psk31), None);
        assert_eq!(protocol_of(t::OverAirMode::Rtty), None);
        assert_eq!(over_air(Protocol::Ft8), t::OverAirMode::Ft8);
        assert_eq!(over_air(Protocol::Ft4), t::OverAirMode::Ft4);
    }

    /// The `encode_wav` → `decode_wav` CLI loop: synthesize a message to a WAV,
    /// load it back, and decode it — exercising modes::synth_message + audio WAV
    /// I/O + the decoder together (FT4 here; the in-memory FT8/FT4 round-trips live
    /// in the modes crate).
    #[test]
    fn encode_to_wav_then_decode_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cq.wav");
        let samples = modes::synth_message("CQ K1ABC FN42", Protocol::Ft4, 1200.0, DECODE_RATE)
            .expect("encode");
        audio::save_wav_mono(&path, &samples, DECODE_RATE).expect("write wav");
        let (loaded, rate) = audio::load_wav_mono(&path).expect("load wav");
        let decs = decode(&loaded, rate, Protocol::Ft4);
        assert!(
            decs.iter().any(|d| d.message == "CQ K1ABC FN42"),
            "got {:?}",
            decs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// End-to-end over the bundled decoder fixture: load → resample → slot →
    /// decode → build a `Decode`, and confirm the parse seam yields structure.
    #[test]
    fn decodes_fixture_wav() {
        // A known-good single-signal FT8 fixture vendored with the decoder.
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../modes/tests/fixtures/cq_k1abc_1000.wav");
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
