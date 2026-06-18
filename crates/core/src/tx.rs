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

use std::time::Duration;

use bus::types as t;
use bus::{BusHandle, Topic};

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
pub fn spawn(bus: &BusHandle, radio: t::RadioId, tx: std::sync::Arc<crate::control::TxControl>) {
    let mut server = match bus.serve::<t::TxRequest, t::TxAck>(&Topic::AudioTx(radio.clone())) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audio-tx: cannot serve {radio:?}: {e:?}");
            return;
        }
    };
    let bus = bus.clone();
    tokio::spawn(async move {
        tracing::info!("audio-tx: TX path armed");
        while let Some((req, responder)) = server.next().await {
            // Read the (live-editable) output device fresh for each over.
            let output = tx.snapshot();
            let (slot, outcome) = transmit(&bus, &radio, output, req).await;
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

/// Run one transmission end to end. Returns the slot it was for (for the report)
/// and the outcome.
async fn transmit(
    bus: &BusHandle,
    radio: &t::RadioId,
    output: Option<String>,
    req: t::TxRequest,
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

    // Key up — validated against the interlock grant by the rig adapter.
    if let Err(e) = key(bus, radio, token, true).await {
        let _ = key(bus, radio, token, false).await; // best-effort safety
        return (Some(slot), classify_key_error(e));
    }

    // Play to the rig's data-in, refreshing PTT inside the watchdog until done.
    let outcome = match audio::play(output, samples, TX_SAMPLE_RATE) {
        Ok(playback) => {
            wait_keyed(bus, radio, token, &playback).await;
            playback.stop();
            t::TxOutcome::Sent
        }
        Err(e) => t::TxOutcome::Failed(format!("audio output: {e}")),
    };

    // Always key down (key-down is never gated).
    let _ = key(bus, radio, token, false).await;
    (Some(slot), outcome)
}

/// Wait for playback to finish (or the safety cap), re-keying PTT every
/// [`PTT_REFRESH`] so the rig watchdog never drops the carrier mid-over.
async fn wait_keyed(
    bus: &BusHandle,
    radio: &t::RadioId,
    token: t::InterlockToken,
    playback: &audio::Playback,
) {
    let tick = Duration::from_millis(500);
    let mut elapsed = Duration::ZERO;
    let mut since_refresh = Duration::ZERO;
    while !playback.is_done() && elapsed < MAX_TX {
        tokio::time::sleep(tick).await;
        elapsed += tick;
        since_refresh += tick;
        if since_refresh >= PTT_REFRESH {
            // Refresh the watchdog; key-down on the next slot edge / the cap
            // recovers from an unexpected denial, so the result is best-effort.
            let _ = key(bus, radio, token, true).await;
            since_refresh = Duration::ZERO;
        }
    }
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
