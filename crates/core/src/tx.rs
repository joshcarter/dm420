//! Audio-TX service: turns a [`TxRequest`](bus::types::TxRequest) into a
//! slot-aligned on-air transmission.
//!
//! Serves `radio/{id}/audio_tx`. For each request it synthesizes the FT8 waveform
//! ([`modes::synth_message`]), keys the rig over the rig command topic
//! (`PttRequest{token}` — validated by the interlock granter), plays the audio to
//! the configured output device (the rig's data-in), **re-keys PTT inside the
//! rig's 10 s watchdog** so the carrier never drops mid-over, keys down, and
//! reports the outcome on `radio/{id}/tx_report`.
//!
//! Spawned **only when `allow_transmit` is set** — this is the explicit, opt-in TX
//! path; nothing here runs in the default (RX-only) build.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bus::types as t;
use bus::{BusError, BusHandle, Topic, TopicSelector};

use crate::rig_adapter::CommandResult;

/// The modes synth produces audio at 12 kHz.
const TX_SAMPLE_RATE: u32 = 12_000;
/// Re-key this often during a transmission to stay inside the rig's 10 s PTT
/// watchdog (`rig::actor::PTT_WATCHDOG`).
const PTT_REFRESH: Duration = Duration::from_secs(5);
/// Hard cap on a single transmission so key-down always lands before the next slot
/// even if playback never signals done. An FT8 over is ~12.6 s in a 15 s slot.
const MAX_TX: Duration = Duration::from_secs(14);

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

    // Synthesize the slot waveform, then trim the trailing silence so key-down
    // lands shortly after the signal, not at the slot edge (the leading silence is
    // kept so the rig has time to switch to TX before the signal).
    let Some(mut samples) = modes::synth_message(&message.text, offset.0, TX_SAMPLE_RATE) else {
        return (
            Some(slot),
            t::TxOutcome::Failed(format!("cannot encode {:?}", message.text)),
        );
    };
    while samples.last().is_some_and(|&s| s.abs() < 1e-4) {
        samples.pop();
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

    // Play to the rig's data-in, refreshing PTT inside the watchdog until done (or
    // the operator hits Stop, which cuts the over short).
    let outcome = match audio::play(output, samples, TX_SAMPLE_RATE) {
        Ok(playback) => {
            let aborted = wait_keyed(bus, radio, token, &playback, abort_gen, base).await;
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

/// Wait for playback to finish (or the safety cap / an operator Stop), re-keying
/// PTT every [`PTT_REFRESH`] so the rig watchdog never drops the carrier mid-over.
/// Returns `true` if the operator aborted (Stop) before playback finished.
async fn wait_keyed(
    bus: &BusHandle,
    radio: &t::RadioId,
    token: t::InterlockToken,
    playback: &audio::Playback,
    abort_gen: &AtomicU64,
    base: u64,
) -> bool {
    let tick = Duration::from_millis(200);
    let mut elapsed = Duration::ZERO;
    let mut since_refresh = Duration::ZERO;
    while !playback.is_done() && elapsed < MAX_TX {
        if abort_gen.load(Ordering::Acquire) != base {
            tracing::debug!("audio-tx: Stop detected mid-over; aborting carrier");
            return true; // operator hit Stop — abort the over now
        }
        tokio::time::sleep(tick).await;
        elapsed += tick;
        since_refresh += tick;
        if since_refresh >= PTT_REFRESH {
            // Refresh the watchdog; key-down on the next slot edge / the cap
            // recovers from an unexpected denial, so the result is best-effort.
            tracing::debug!("audio-tx: PTT refresh (staying inside rig watchdog)");
            let _ = key(bus, radio, token, true).await;
            since_refresh = Duration::ZERO;
        }
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
