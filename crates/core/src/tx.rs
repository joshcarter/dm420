//! Audio-TX service: turns a [`TxRequest`](bus::types::TxRequest) into a
//! slot-aligned on-air transmission.
//!
//! Serves `radio/{id}/audio_tx`. For each request it synthesizes the FT8 waveform
//! ([`modes::synth_message`]), keys the rig **once** over the rig command topic
//! (`PttRequest{token}` — validated by the interlock granter), plays the audio to
//! the configured output device (the rig's data-in), keys down at the end, and
//! reports the outcome on `radio/{id}/tx_report`. A whole over fits inside the
//! rig's PTT watchdog (`rig::actor::PTT_WATCHDOG`, sized to outlast one slot), so
//! we never re-key mid-over — real Kenwoods reject a `TX` command while already
//! transmitting, which would error the refresh and let the watchdog drop the
//! carrier.
//!
//! Spawned **only when `allow_transmit` is set** — this is the explicit, opt-in TX
//! path; nothing here runs in the default (RX-only) build.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bus::types as t;
use bus::{BusError, BusHandle, Topic, TopicSelector};

use crate::rig_adapter::CommandResult;

/// The modes synth produces audio at 12 kHz.
const TX_SAMPLE_RATE: u32 = 12_000;
/// Hard cap on a single transmission so key-down always lands before the next slot
/// even if playback never signals done. An FT8 over is ~12.6 s in a 15 s slot, and
/// this stays under the rig's PTT watchdog so key-down beats it.
const MAX_TX: Duration = Duration::from_secs(14);

/// Own-TX waterfall parameters — must mirror `core::decode` so our own-TX columns
/// share the RX axis: a 1024-pt FFT at 12 kHz (~11.7 Hz bins) kept to ~0..3000 Hz,
/// one column every 50 ms. The TX synth already runs at 12 kHz (`TX_SAMPLE_RATE`),
/// so a column FFT'd here drops straight onto the same offset axis the decoder uses.
const FFT_SIZE: usize = 1024;
const SPECTRUM_MAX_HZ: f32 = 3000.0;
const SPECTRUM_HOP_S: f64 = 0.05;

/// Serve `radio/{id}/audio_tx`: run each requested transmission to completion (one
/// at a time) and report its outcome on `radio/{id}/tx_report`.
pub fn spawn(bus: &BusHandle, radio: t::RadioId, tx: Arc<crate::control::TxControl>) {
    let mut server = match bus.serve::<t::TxRequest, t::TxAck>(&Topic::AudioTx(radio.clone())) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audio-tx: cannot serve {radio:?}: {e:?}");
            return;
        }
    };
    // Bumped whenever the QSO engine drops to Idle (the operator's Stop, seen via
    // the engine's published state). A transmit baselines it and aborts the over
    // the instant it changes — so Stop kills the carrier mid-message.
    let abort_gen = Arc::new(AtomicU64::new(0));
    spawn_abort_watcher(bus, radio.clone(), abort_gen.clone());

    let bus = bus.clone();
    tokio::spawn(async move {
        tracing::info!("audio-tx: TX path armed");
        while let Some((req, responder)) = server.next().await {
            // Read the (live-editable) output device fresh for each over.
            let output = tx.snapshot();
            let (slot, outcome) = transmit(&bus, &radio, output, req, &abort_gen).await;
            match &outcome {
                t::TxOutcome::Sent => tracing::info!(?slot, "audio-tx: over sent"),
                t::TxOutcome::Failed(e) => tracing::warn!(?slot, error = %e, "audio-tx: over failed"),
                t::TxOutcome::Denied(d) => {
                    tracing::warn!(?slot, denial = ?d, "audio-tx: over denied")
                }
            }
            let _ = bus.publish(
                &Topic::TxReport(radio.clone()),
                t::TxReport {
                    radio: radio.clone(),
                    slot,
                    outcome,
                },
            );
            // The ack is receipt only; the TxReport above is the source of truth.
            responder.reply(t::TxAck::Accepted);
        }
    });
}

/// Watch the QSO engine's published state and bump `abort_gen` each time it drops
/// into Idle (the operator's Stop, or a finished QSO). A transmit baselines the
/// counter at the start of an over, so an Idle that *precedes* the over (e.g. the
/// engine going Idle as it queues a courtesy 73) is captured in the baseline and
/// won't abort it — only an Idle that lands *during* the over does.
fn spawn_abort_watcher(bus: &BusHandle, radio: t::RadioId, abort_gen: Arc<AtomicU64>) {
    let mut sub = match bus.subscribe::<t::QsoState>(TopicSelector::Exact(Topic::QsoState(radio))) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audio-tx: cannot watch QsoState for aborts: {e:?}");
            return;
        }
    };
    tokio::spawn(async move {
        let mut prev_idle = true; // the engine starts Idle
        loop {
            match sub.recv().await {
                Ok(state) => {
                    let idle = matches!(state.phase, t::QsoPhase::Idle);
                    if idle && !prev_idle {
                        abort_gen.fetch_add(1, Ordering::Release);
                    }
                    prev_idle = idle;
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Run one transmission end to end. Returns the slot it was for (for the report)
/// and the outcome.
async fn transmit(
    bus: &BusHandle,
    radio: &t::RadioId,
    output: Option<String>,
    req: t::TxRequest,
    abort_gen: &AtomicU64,
) -> (Option<t::SlotId>, t::TxOutcome) {
    let t::TxRequest::SlottedMessage {
        mode,
        offset,
        slot,
        message,
        token,
        ..
    } = req
    else {
        return (
            None,
            t::TxOutcome::Failed("unsupported TxRequest variant".into()),
        );
    };

    tracing::debug!(?slot, ?mode, offset = offset.0, message = %message.text, "audio-tx: begin over");

    // FT8 only for now: the encoder has no FT4 synth yet.
    if mode != t::OverAirMode::Ft8 {
        return (
            Some(slot),
            t::TxOutcome::Failed(format!("{mode:?} TX synthesis not implemented yet")),
        );
    }

    // Synthesize the slot waveform, then trim the silence off both ends: trailing
    // entirely (so key-down lands right after the signal), and leading down to a
    // short T/R-settle lead (so the tones start near the top of the slot, not the
    // synth's centered ~1.18 s in — a late start otherwise pushes our DT out of the
    // far station's decode window).
    let Some(mut samples) = modes::synth_message(&message.text, offset.0, TX_SAMPLE_RATE) else {
        return (
            Some(slot),
            t::TxOutcome::Failed(format!("cannot encode {:?}", message.text)),
        );
    };
    while samples.last().is_some_and(|&s| s.abs() < 1e-4) {
        samples.pop();
    }
    // ~0.5 s of lead is ample for the rig's (millisecond) T/R switch; drop the rest.
    const TX_LEAD_SAMPLES: usize = TX_SAMPLE_RATE as usize / 2;
    let lead = samples.iter().take_while(|&&s| s.abs() < 1e-4).count();
    if lead > TX_LEAD_SAMPLES {
        samples.drain(..lead - TX_LEAD_SAMPLES);
    }
    tracing::debug!(
        samples = samples.len(),
        secs = samples.len() as f32 / TX_SAMPLE_RATE as f32,
        "audio-tx: synthesized FT8 waveform",
    );

    // Baseline the abort counter before keying up, so only a Stop that lands
    // during *this* over aborts it.
    let base = abort_gen.load(Ordering::Acquire);

    // Key up — validated against the interlock grant by the rig adapter.
    tracing::debug!(?token, "audio-tx: keying up (TX1 / data route)");
    if let Err(e) = key(bus, radio, token, true).await {
        let _ = key(bus, radio, token, false).await; // best-effort safety
        return (Some(slot), classify_key_error(e));
    }

    // Pre-FFT the synthesized waveform into own-TX waterfall columns so we can
    // stream them, paced to playback, onto the spectrum topic — the operator sees
    // their outgoing signal scroll by at its true offset (the RX capture is
    // meaningless while keyed). Columns share the RX axis (same FFT size + rate).
    let bin_hz = TX_SAMPLE_RATE as f32 / FFT_SIZE as f32;
    let max_bins = (SPECTRUM_MAX_HZ / bin_hz).ceil() as usize;
    let tx_cols = tx_spectrum_columns(&samples, max_bins);

    // Play to the rig's data-in, staying keyed until playback finishes (or the
    // operator hits Stop, which cuts the over short). No mid-over re-keying: the
    // watchdog covers a full over, and a Kenwood rejects `TX` while transmitting.
    let outcome = match audio::play(output, samples, TX_SAMPLE_RATE) {
        Ok(playback) => {
            // Stream the own-TX columns while it plays; `stop` ends the streamer the
            // instant the over does (normal finish or operator Stop).
            let stop = Arc::new(AtomicBool::new(false));
            spawn_tx_spectrum(
                bus.clone(),
                radio.clone(),
                mode,
                playback.progress.clone(),
                tx_cols,
                bin_hz,
                stop.clone(),
            );
            let aborted = wait_done(&playback, abort_gen, base).await;
            stop.store(true, Ordering::Release);
            playback.stop();
            if aborted {
                t::TxOutcome::Failed("aborted by operator".into())
            } else {
                t::TxOutcome::Sent
            }
        }
        Err(e) => t::TxOutcome::Failed(format!("audio output: {e}")),
    };

    // Always key down (key-down is never gated).
    let _ = key(bus, radio, token, false).await;
    tracing::debug!("audio-tx: keyed down");
    (Some(slot), outcome)
}

/// Wait for playback to finish (or the safety cap / an operator Stop). The carrier
/// stays keyed from the single key-up in [`transmit`] — a full over fits inside the
/// rig's PTT watchdog, so there is no mid-over re-keying. Returns `true` if the
/// operator aborted (Stop) before playback finished.
async fn wait_done(playback: &audio::Playback, abort_gen: &AtomicU64, base: u64) -> bool {
    let tick = Duration::from_millis(200);
    let mut elapsed = Duration::ZERO;
    while !playback.is_done() && elapsed < MAX_TX {
        if abort_gen.load(Ordering::Acquire) != base {
            tracing::debug!("audio-tx: Stop detected mid-over; aborting carrier");
            return true; // operator hit Stop — abort the over now
        }
        tokio::time::sleep(tick).await;
        elapsed += tick;
    }
    false
}

/// Issue one `PttRequest` over the rig command topic.
async fn key(
    bus: &BusHandle,
    radio: &t::RadioId,
    token: t::InterlockToken,
    on: bool,
) -> Result<(), String> {
    match bus
        .request::<t::RigCommand, CommandResult>(
            &Topic::RigCommand(radio.clone()),
            t::RigCommand::PttRequest { on, token },
            Duration::from_secs(2),
        )
        .await
    {
        Ok(CommandResult::Ok) => Ok(()),
        Ok(CommandResult::Err(e)) => Err(e),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// A key-up rejection mentioning the interlock is a token denial; anything else
/// (PTT/CAT failure, rig offline) is a hardware failure.
fn classify_key_error(e: String) -> t::TxOutcome {
    if e.contains("interlock") {
        t::TxOutcome::Denied(t::InterlockError::Denied)
    } else {
        t::TxOutcome::Failed(e)
    }
}

/// FFT the synthesized waveform into scrolling-waterfall columns, one every
/// [`SPECTRUM_HOP_S`]. Each entry is `(sample index at the window's right edge,
/// magnitudes)` so the streamer can release a column once playback has reached it.
fn tx_spectrum_columns(samples: &[f32], max_bins: usize) -> Vec<(usize, Vec<u8>)> {
    let hop = (TX_SAMPLE_RATE as f64 * SPECTRUM_HOP_S).max(1.0) as usize;
    let mut cols = Vec::new();
    let mut end = FFT_SIZE;
    while end <= samples.len() {
        cols.push((
            end,
            dsp::spectrum_column(&samples[end - FFT_SIZE..end], FFT_SIZE, max_bins),
        ));
        end += hop;
    }
    cols
}

/// Stream the pre-computed own-TX waterfall columns onto the spectrum topic, paced
/// to playback `progress` (file frames at 12 kHz, so it indexes `samples` directly)
/// and tagged [`SignalSource::OwnTx`](t::SignalSource::OwnTx) so the GUI shows them
/// in place of the RX waterfall while keyed. Ends when every column is out or
/// `stop` is set (the over finished or the operator aborted).
fn spawn_tx_spectrum(
    bus: BusHandle,
    radio: t::RadioId,
    mode: t::OverAirMode,
    progress: Arc<AtomicU64>,
    cols: Vec<(usize, Vec<u8>)>,
    bin_hz: f32,
    stop: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut i = 0;
        while i < cols.len() {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let played = progress.load(Ordering::Relaxed) as usize;
            while i < cols.len() && cols[i].0 <= played {
                let _ = bus.publish(
                    &Topic::Spectrum(radio.clone()),
                    t::SpectrumRow {
                        radio: radio.clone(),
                        mode,
                        t: t::Timestamp(now_ms()),
                        bin0_offset: t::OffsetHz(0.0),
                        bin_hz,
                        mags: cols[i].1.clone(),
                        source: t::SignalSource::OwnTx,
                    },
                );
                i += 1;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
}

/// Milliseconds since the Unix epoch (wall clock), for stamping spectrum columns.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
