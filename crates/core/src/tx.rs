//! Audio-TX service: turns a [`TxRequest`](bus::types::TxRequest) into a
//! slot-aligned on-air transmission.
//!
//! Serves `radio/{id}/audio_tx`. For each request it synthesizes the FT8/FT4 waveform
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bus::types as t;
use bus::{BusError, BusHandle, Topic, TopicSelector};

use crate::rig_adapter::CommandResult;

/// The modes synth produces audio at 12 kHz.
const TX_SAMPLE_RATE: u32 = 12_000;
/// Safety cap on a single transmission so key-down always lands before the next
/// slot even if playback never signals done — a backstop only (normal key-down is
/// playback-driven). Sized just under the mode's slot and the rig's PTT watchdog:
/// FT8 ≈ 14 s in a 15 s slot, FT4 ≈ 6.5 s in a 7.5 s slot.
fn max_tx_for(mode: t::OverAirMode) -> Duration {
    Duration::from_millis((slot_period_ms(mode) - 1000).max(2000) as u64)
}

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
        // The warm output stream: opened once and kept alive across overs so a
        // transmission starts on the next audio callback, not after a cold device
        // open. Re-opened only when the selected device changes or it drops out.
        // Open it up front so even the first over is immediate; if the device isn't
        // ready yet the loop opens it on the first transmit instead.
        let mut out: Option<audio::OutputStream> = match audio::OutputStream::open(tx.snapshot()) {
            Ok(o) => Some(o),
            Err(e) => {
                tracing::warn!(error = %e, "audio-tx: output not ready at startup; opening on first over");
                None
            }
        };
        while let Some((req, responder)) = server.next().await {
            // Snapshot the loggable shape of this over before `req` is consumed by
            // `transmit`, to mirror it (with the outcome) onto `tx_log` for the raw
            // diagnostic archive. Only slotted FT8/FT4 overs are recorded; the PSK
            // stream variants are architecture-only in v1.
            let tx_log = if let t::TxRequest::SlottedMessage {
                mode,
                offset,
                slot,
                message,
                ..
            } = &req
            {
                Some((*mode, *offset, *slot, message.clone(), t::Timestamp(now_ms())))
            } else {
                None
            };
            let want = tx.snapshot(); // live-editable output device (None = default)
            let reopen = match &out {
                Some(o) => o.requested() != want.as_deref() || o.is_dead(),
                None => true,
            };
            if reopen {
                drop(out.take()); // drop any existing stream first to free the device
                out = match audio::OutputStream::open(want.clone()) {
                    Ok(o) => Some(o),
                    Err(e) => {
                        tracing::warn!(error = %e, "audio-tx: cannot open output device");
                        None
                    }
                };
            }
                    let gain = tx.gain(); // live-editable TX audio gain, snapshot per over
            let (slot, outcome) = transmit(&bus, &radio, out.as_ref(), req, gain, &abort_gen).await;
            match &outcome {
                t::TxOutcome::Sent => tracing::info!(?slot, "audio-tx: over sent"),
                t::TxOutcome::Failed(e) => tracing::warn!(?slot, error = %e, "audio-tx: over failed"),
                t::TxOutcome::Denied(d) => {
                    tracing::warn!(?slot, denial = ?d, "audio-tx: over denied")
                }
            }
            // Mirror the over (with its real outcome) onto `tx_log` for the archive.
            // Off the operating path — nothing in the QSO/UI flow consumes this topic;
            // it exists only for the diagnostic decode/transmit archive.
            if let Some((mode, offset, tx_slot, message, t)) = tx_log {
                let _ = bus.publish(
                    &Topic::TxLog(radio.clone()),
                    t::TxLogEntry {
                        radio: radio.clone(),
                        mode,
                        slot: tx_slot,
                        offset,
                        message,
                        outcome: outcome.clone(),
                        t,
                    },
                );
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

/// Run one transmission end to end on the warm `out` stream. Returns the slot it
/// was for (for the report) and the outcome. `out` is `None` only when the output
/// device couldn't be opened — the over then fails cleanly.
async fn transmit(
    bus: &BusHandle,
    radio: &t::RadioId,
    out: Option<&audio::OutputStream>,
    req: t::TxRequest,
    gain: f32,
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

    let Some(out) = out else {
        return (
            Some(slot),
            t::TxOutcome::Failed("audio output: device unavailable".into()),
        );
    };

    // `into_slot` (ms past the mode's slot edge — 15 s FT8 / 7.5 s FT4) is the
    // timing-critical metric: the tones must reach the air near the top of the slot
    // or the far end's DT goes out of range. Logged at each step so the lateness
    // budget is visible on real hw.
    let slot_ms = slot_period_ms(mode);
    tracing::info!(
        ?slot, ?mode, offset = offset.0, into_slot_ms = now_ms().rem_euclid(slot_ms),
        message = %message.text, "audio-tx: begin over",
    );

    // Map the on-air mode to a synthesizable protocol. FT8 and FT4 both have
    // encoders; anything else (PSK31/RTTY) has no waveform synth, so fail cleanly.
    let Some(protocol) = crate::decode::protocol_of(mode) else {
        return (
            Some(slot),
            t::TxOutcome::Failed(format!("{mode:?} has no waveform synthesizer")),
        );
    };

    // Synthesize the slot waveform, then trim the silence off both ends: trailing
    // entirely (so key-down lands right after the signal), and leading down to a
    // short T/R-settle lead (so the tones start near the top of the slot, not the
    // synth's centered ~1.18 s in — a late start otherwise pushes our DT out of the
    // far station's decode window).
    let t_synth = Instant::now();
    let Some(mut samples) = modes::synth_message(&message.text, protocol, offset.0, TX_SAMPLE_RATE)
    else {
        return (
            Some(slot),
            t::TxOutcome::Failed(format!("cannot encode {:?}", message.text)),
        );
    };
    while samples.last().is_some_and(|&s| s.abs() < 1e-4) {
        samples.pop();
    }
    // Leave only a short T/R-settle lead (~0.2 s). We key up *before* playing (see
    // below), so the rig is already transmitting by the time audio flows; a long
    // lead just pushes our DT later. Small margin guards the rig's ALC attack.
    const TX_LEAD_SAMPLES: usize = TX_SAMPLE_RATE as usize / 5;
    let lead = samples.iter().take_while(|&&s| s.abs() < 1e-4).count();
    if lead > TX_LEAD_SAMPLES {
        samples.drain(..lead - TX_LEAD_SAMPLES);
    }
    let synth_ms = t_synth.elapsed().as_millis();

    // Apply the TX audio gain. The synth emits at 0 dBFS; scaling here (after the
    // silence trim, which keys off the full-scale envelope) backs the level off so
    // the rig's ALC stays at/under threshold instead of splattering. `gain` is
    // pre-clamped to [0.0, 1.0] by `TxControl`, so this can only attenuate.
    if gain != 1.0 {
        for s in &mut samples {
            *s *= gain;
        }
    }

    // Baseline the abort counter before keying up, so only a Stop that lands
    // during *this* over aborts it.
    let base = abort_gen.load(Ordering::Acquire);

    // Key up — validated against the interlock grant by the rig adapter.
    let t_key = Instant::now();
    if let Err(e) = key(bus, radio, token, true).await {
        let _ = key(bus, radio, token, false).await; // best-effort safety
        return (Some(slot), classify_key_error(e));
    }
    let key_ms = t_key.elapsed().as_millis();

    // Play to the rig's data-in on the warm stream — starts on the next audio
    // callback (no device open here), staying keyed until playback finishes (or the
    // operator hits Stop, which cuts the over short). No mid-over re-keying: the
    // watchdog covers a full over, and a Kenwood rejects `TX` while transmitting.
    let t_load = Instant::now();
    out.load(&samples, TX_SAMPLE_RATE);
    let load_ms = t_load.elapsed().as_millis();
    // How late the first audio sample reaches the air, relative to the slot edge.
    // `synth/key/load_ms` break down the gap from "begin over" so we can see which
    // step dominates. Plus the ~0.2 s lead trimmed above, this is our effective DT.
    tracing::info!(
        into_slot_ms = now_ms().rem_euclid(slot_ms),
        synth_ms,
        key_ms,
        load_ms,
        "audio-tx: playback started (tones reach air ~0.2 s later)",
    );
    // Stream the own-TX waterfall columns while it plays, FFT'd lazily as playback
    // reaches each one — kept *off* the pre-playback critical path. `stop` ends the
    // streamer the instant the over does (normal finish or operator Stop).
    let bin_hz = TX_SAMPLE_RATE as f32 / FFT_SIZE as f32;
    let max_bins = (SPECTRUM_MAX_HZ / bin_hz).ceil() as usize;
    let stop = Arc::new(AtomicBool::new(false));
    spawn_tx_spectrum(
        bus.clone(),
        radio.clone(),
        mode,
        out.progress(),
        Arc::new(samples),
        bin_hz,
        max_bins,
        stop.clone(),
    );
    let aborted = wait_done(out, abort_gen, base, max_tx_for(mode)).await;
    stop.store(true, Ordering::Release);
    if aborted {
        out.silence(); // cut the carrier audio now
    }
    let outcome = if aborted {
        t::TxOutcome::Failed("aborted by operator".into())
    } else {
        t::TxOutcome::Sent
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
async fn wait_done(
    out: &audio::OutputStream,
    abort_gen: &AtomicU64,
    base: u64,
    max_tx: Duration,
) -> bool {
    let tick = Duration::from_millis(200);
    let mut elapsed = Duration::ZERO;
    while !out.is_done() && elapsed < max_tx {
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

/// Stream the own-TX waterfall onto the spectrum topic, tagged
/// [`SignalSource::OwnTx`](t::SignalSource::OwnTx) so the GUI shows it in place of
/// the RX waterfall while keyed. Each column is FFT'd **lazily**, only once playback
/// `progress` (file frames at 12 kHz, so it indexes `samples` directly) reaches it —
/// spreading the FFT work across the over instead of front-loading it onto the
/// pre-playback critical path. Ends when the waveform is exhausted or `stop` is set
/// (the over finished or the operator aborted).
#[allow(clippy::too_many_arguments)]
fn spawn_tx_spectrum(
    bus: BusHandle,
    radio: t::RadioId,
    mode: t::OverAirMode,
    progress: Arc<AtomicU64>,
    samples: Arc<Vec<f32>>,
    bin_hz: f32,
    max_bins: usize,
    stop: Arc<AtomicBool>,
) {
    let hop = (TX_SAMPLE_RATE as f64 * SPECTRUM_HOP_S).max(1.0) as usize;
    tokio::spawn(async move {
        let mut end = FFT_SIZE; // right edge of the next column's FFT window
        while end <= samples.len() {
            if stop.load(Ordering::Acquire) {
                break;
            }
            // Sample the playback clock once per poll: `played` audio frames have
            // played as of wall-clock `now`. Each column below is then stamped by
            // its *own* audio position (`tx_col_time_ms`), not by publish time — so
            // a burst published in one wake (after a scheduler hiccup) keeps its real
            // ~50 ms column spacing instead of collapsing onto a single timestamp.
            let now = now_ms();
            let played = progress.load(Ordering::Relaxed) as usize;
            while end <= played && end <= samples.len() {
                let mags = dsp::spectrum_column(&samples[end - FFT_SIZE..end], FFT_SIZE, max_bins);
                let _ = bus.publish(
                    &Topic::Spectrum(radio.clone()),
                    t::SpectrumRow {
                        radio: radio.clone(),
                        mode,
                        t: t::Timestamp(tx_col_time_ms(now, played, end, TX_SAMPLE_RATE)),
                        bin0_offset: t::OffsetHz(0.0),
                        bin_hz,
                        mags,
                        source: t::SignalSource::OwnTx,
                    },
                );
                end += hop;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
}

/// Wall-clock ms at which the audio sample at index `end` reached the stream, given
/// that `played` samples have played as of `now_ms`. Stamps a TX spectrogram column
/// by its true audio position (not publish time), so burst-published columns keep
/// their real spacing instead of collapsing onto one timestamp.
fn tx_col_time_ms(now_ms: i64, played: usize, end: usize, sample_rate: u32) -> i64 {
    now_ms - (played as i64 - end as i64) * 1000 / sample_rate as i64
}

/// Milliseconds since the Unix epoch (wall clock), for stamping spectrum columns.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Slot length in ms for the `into_slot_ms` audit metric (mode-aware: FT4 = 7.5 s).
/// Streaming modes have no slot and don't reach the slotted-message path, so they
/// fall through to the FT8 period.
fn slot_period_ms(mode: t::OverAirMode) -> i64 {
    match mode {
        t::OverAirMode::Ft4 => 7_500,
        _ => 15_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tx_col_time_ms` stamps each TX spectrogram column by its audio position, so a
    /// burst published in one poll keeps its real column spacing instead of collapsing.
    #[test]
    fn tx_col_time_stamps_by_audio_position() {
        let now = 1_000_000_i64;
        let hop = (TX_SAMPLE_RATE as f64 * SPECTRUM_HOP_S) as usize;

        // 1. A column at the play head (`end == played`) is stamped `now` exactly.
        let played = 50_000_usize;
        assert_eq!(tx_col_time_ms(now, played, played, TX_SAMPLE_RATE), now);

        // 2. Two columns from the *same* (now, played) burst with right edges `end`
        //    and `end + hop` are spaced `hop*1000/rate` ms apart, with the older
        //    (smaller-`end`) column earlier — proving a burst no longer collapses.
        let end = 20_000_usize;
        let t0 = tx_col_time_ms(now, played, end, TX_SAMPLE_RATE);
        let t1 = tx_col_time_ms(now, played, end + hop, TX_SAMPLE_RATE);
        let expected_delta = hop as i64 * 1000 / TX_SAMPLE_RATE as i64;
        assert_eq!(t1 - t0, expected_delta);
        assert!(t0 < t1, "older (smaller-end) column must be earlier");

        // 3. A column whose audio played 1 s ago is stamped `now - 1000`.
        let end_1s_ago = played - TX_SAMPLE_RATE as usize;
        assert_eq!(
            tx_col_time_ms(now, played, end_1s_ago, TX_SAMPLE_RATE),
            now - 1000
        );
    }
}
