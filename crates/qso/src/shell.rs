//! The async bus shell around the pure [`Engine`].
//!
//! One tokio task per radio owns an [`Engine`] and feeds it events from the bus:
//! operator commands (`qso/{id}/command`), inbound decodes
//! (`radio/{id}/decodes`), the selection (`selection/{id}/active`), and the clock
//! (`clock/status`, whose `slot` id changes at each slot boundary). It publishes
//! the engine's [`QsoState`] on `qso/{id}/state` and logs completed contacts on
//! `logbook/entries`.
//!
//! The clock's [`ClockStatus::slot`] is the authoritative, mode-aware slot
//! identity — the shell ticks the engine with it directly rather than recomputing
//! one from the wall clock, so the engine's TX parity stays commensurate with the
//! decoder's slot numbering under FT4's 7.5 s slots (the old fixed 15 s recompute
//! was the FT4 "armed but never transmits" bug).
//!
//! Transmission is **gated behind `allow_transmit`** and, until the PTT interlock
//! granter + audio-TX codec exist, has nowhere to go — so the task computes and
//! publishes `QsoState.next_tx` (what it *would* send) but does not put anything
//! on the air. This mirrors `core`'s TX hard-block (`docs/qso_flow.md`, `TODO.md`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bus::types::{
    AbsHz, Band, ClockStatus, Decode, InterlockReply, InterlockRequest, LogEntry, OffsetHz,
    OverAirMode, QsoCommand, QsoId, RadioId, Selection, SlotId, StationId, Timestamp, TxAck,
    TxRequest,
};
use bus::{BusHandle, BusMessage, DeliveryClass, Topic, TopicSelector};
use serde::{Deserialize, Serialize};

use crate::engine::{CompletedQso, Engine, Event, Step, TxIntent};
use crate::message::StationConfig;

/// Reply to a `qso/{id}/command` request. The engine accepts every command (it
/// is the operator's single arm/disarm control); progress is reflected on the
/// state topic, so the ack is just receipt.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum QsoAck {
    Accepted,
}

impl BusMessage for QsoAck {
    const CLASS: DeliveryClass = DeliveryClass::Command;
}

/// Live-reconfiguration handle for the running engine — the GUI pushes the
/// current station identity / contest profile here on re-lock (mirrors the
/// rig/audio control handles, since nobody publishes `OperatingState` yet).
#[derive(Clone)]
pub struct QsoControl {
    station: Arc<Mutex<StationConfig>>,
}

impl QsoControl {
    /// Update the station identity / contest the engine builds messages from.
    pub fn set_station(&self, station: StationConfig) {
        *self.station.lock().unwrap() = station;
    }
}

/// Launch the QSO engine for `radio` onto `bus`. Must be called from within a
/// tokio runtime (like `core::spawn` / `mocks::spawn`).
pub fn spawn(
    bus: &BusHandle,
    radio: RadioId,
    station: StationConfig,
    allow_transmit: bool,
) -> QsoControl {
    let shared = Arc::new(Mutex::new(station.clone()));
    let control = QsoControl {
        station: shared.clone(),
    };
    tracing::info!(radio = ?radio, allow_transmit, "qso: engine spawned");
    tokio::spawn(run(bus.clone(), radio, station, shared, allow_transmit));
    control
}

async fn run(
    bus: BusHandle,
    radio: RadioId,
    station: StationConfig,
    shared: Arc<Mutex<StationConfig>>,
    allow_transmit: bool,
) {
    let mut engine = Engine::new(radio.clone(), station, OffsetHz(1500.0));

    let mut cmds = match bus.serve::<QsoCommand, QsoAck>(&Topic::QsoCommand(radio.clone())) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("qso: cannot serve commands for {radio:?}: {e:?}");
            return;
        }
    };
    let mut decodes =
        match bus.subscribe::<Decode>(TopicSelector::Exact(Topic::Decodes(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("qso: cannot subscribe decodes: {e:?}");
                return;
            }
        };
    let mut selection =
        match bus.subscribe::<Selection>(TopicSelector::Exact(Topic::Selection(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("qso: cannot subscribe selection: {e:?}");
                return;
            }
        };
    let mut clock = match bus.subscribe::<ClockStatus>(TopicSelector::Exact(Topic::ClockStatus)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("qso: cannot subscribe clock: {e:?}");
            return;
        }
    };

    // Publish an initial Idle state so late-joining UIs render immediately.
    let _ = bus.publish(&Topic::QsoState(radio.clone()), engine.state());

    // The clock's last-seen slot id; a change marks a T/R boundary (the tick).
    let mut prev_slot: Option<SlotId> = None;
    // The active on-air mode we hand the audio-TX codec so it never synthesizes the
    // wrong protocol into a slot. Sourced from the clock's `ClockStatus.mode` (the
    // live configured protocol), which arrives within one tick (~50 ms) of startup —
    // so it's correct even for an FT4 CQ-first over, before any decode is heard. This
    // FT8 initial value only stands until that first clock tick.
    let mut mode = OverAirMode::Ft8;
    // Seed the per-contact sequence from the wall clock so `QsoId { origin, seq }`
    // stays unique *across sessions*. A plain 0 restart reused 1, 2, 3… every run,
    // colliding with already-logged ids — the logbook dedups by `QsoId`, so those
    // post-restart contacts were silently dropped (never persisted). now_ms at
    // startup is strictly greater than any prior session's seqs, so no collision.
    let mut seq: u64 = now_ms();

    loop {
        // Pick up any live station-config change before handling the next event.
        engine.set_station(shared.lock().unwrap().clone());

        let step = tokio::select! {
            Some((cmd, responder)) = cmds.next() => {
                let step = engine.step(Event::Command(cmd));
                responder.reply(QsoAck::Accepted);
                step
            }
            r = decodes.recv() => match r {
                Ok(d) => engine.step(Event::Decode(d)),
                Err(_) => continue,
            },
            r = selection.recv() => match r {
                Ok(sel) => engine.step(Event::Select(sel)),
                Err(_) => continue,
            },
            r = clock.recv() => match r {
                Ok(cs) => {
                    // The clock is the authoritative mode source (derived from the
                    // live configured protocol), so `mode` is correct even before the
                    // first decode — an FT4 CQ-first over synthesizes as FT4, not FT8.
                    mode = cs.mode;
                    // A slot boundary is a change in the clock's authoritative slot
                    // id. Seed `prev_slot` on the first message without firing (we
                    // may be mid-slot), then tick on every change after.
                    let boundary = prev_slot.is_some_and(|p| p != cs.slot);
                    prev_slot = Some(cs.slot);
                    if !boundary {
                        continue;
                    }
                    engine.step(Event::Tick { slot: cs.slot })
                }
                Err(_) => continue,
            },
            else => break,
        };

        apply(
            &bus,
            &radio,
            &station_call(&shared),
            &mut seq,
            step,
            allow_transmit,
            mode,
        );
    }
}

/// Publish the new state and act on the engine's TX / log outputs.
fn apply(
    bus: &BusHandle,
    radio: &RadioId,
    my_station: &StationId,
    seq: &mut u64,
    step: Step,
    allow_transmit: bool,
    mode: OverAirMode,
) {
    let _ = bus.publish(&Topic::QsoState(radio.clone()), step.state);

    if let Some(tx) = step.tx {
        if allow_transmit {
            // Hand the over to the audio-TX codec on its own task (acquire token →
            // request TxRequest → release). The engine loop keeps running so it
            // still services decodes/clock during the ~13 s transmission.
            spawn_transmit(bus.clone(), radio.clone(), tx, mode);
        } else {
            tracing::debug!(
                "qso: TX gated off; would send {:?} @ {:?}",
                tx.message.text,
                tx.offset
            );
        }
    }

    if let Some(done) = step.log {
        *seq += 1;
        let _ = bus.publish(
            &Topic::LogbookEntries,
            build_log(done, radio.clone(), my_station.clone(), *seq, mode),
        );
    }
}

/// Transmit one over on its own task: acquire the PTT token from the granter, hand
/// the message to the audio-TX codec (which keys, plays, and reports on
/// `tx_report`), then release the token so the next slot can acquire it. Spawned so
/// the engine loop keeps servicing decodes/clock through the ~13 s over.
fn spawn_transmit(bus: BusHandle, radio: RadioId, tx: TxIntent, mode: OverAirMode) {
    tokio::spawn(async move {
        tracing::debug!(
            offset = ?tx.offset, slot = ?tx.slot, message = %tx.message.text,
            "qso: starting over (acquiring PTT token)",
        );
        let token = match bus
            .request::<InterlockRequest, InterlockReply>(
                &Topic::Interlock(radio.clone()),
                InterlockRequest::Acquire,
                Duration::from_secs(2),
            )
            .await
        {
            Ok(InterlockReply::Granted { token, .. }) => token,
            Ok(other) => {
                tracing::warn!("qso: PTT interlock not granted: {other:?}");
                return;
            }
            Err(e) => {
                tracing::warn!("qso: interlock request failed: {e:?}");
                return;
            }
        };

        tracing::debug!(?token, "qso: PTT token acquired; handing over to audio-tx");
        let req = TxRequest::SlottedMessage {
            radio: radio.clone(),
            // The mode the partner was heard on. The audio-TX codec only
            // synthesizes FT8 today, so an FT4 over is rejected there with a clear
            // "not implemented" — better than keying FT8 tones into an FT4 slot.
            mode,
            offset: tx.offset,
            slot: tx.slot,
            message: tx.message,
            token,
        };
        // The codec replies once the over finishes (~13 s).
        if let Err(e) = bus
            .request::<TxRequest, TxAck>(&Topic::AudioTx(radio.clone()), req, Duration::from_secs(30))
            .await
        {
            tracing::warn!("qso: audio-tx request failed: {e:?}");
        }
        let _ = bus
            .request::<InterlockRequest, InterlockReply>(
                &Topic::Interlock(radio.clone()),
                InterlockRequest::Release(token),
                Duration::from_secs(2),
            )
            .await;
    });
}

/// Build a [`LogEntry`] from a completed contact. `mode` is the real on-air mode
/// (tracked from the partner's decodes); band/freq remain placeholders until the
/// engine subscribes to `RigState`. Logging is dormant while TX is blocked (no QSO
/// can complete on the air), so this is wired for correctness, not yet exercised.
/// TODO: stamp real band/freq/time.
fn build_log(
    done: CompletedQso,
    radio: RadioId,
    origin: StationId,
    seq: u64,
    mode: OverAirMode,
) -> LogEntry {
    LogEntry {
        id: QsoId {
            origin: origin.clone(),
            seq,
        },
        origin,
        radio: Some(radio),
        call: done.call,
        mode,
        band: Band::B20m,
        freq: AbsHz(14_074_000),
        time: Timestamp(now_ms() as i64),
        exchange_sent: done.exchange_sent,
        exchange_rcvd: done.exchange_rcvd,
        grid: done.grid,
        section: done.section,
    }
}

fn station_call(shared: &Arc<Mutex<StationConfig>>) -> StationId {
    StationId(shared.lock().unwrap().call.0.clone())
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
