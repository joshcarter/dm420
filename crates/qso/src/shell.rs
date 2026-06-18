//! The async bus shell around the pure [`Engine`].
//!
//! One tokio task per radio owns an [`Engine`] and feeds it events from the bus:
//! operator commands (`qso/{id}/command`), inbound decodes
//! (`radio/{id}/decodes`), the selection (`selection/{id}/active`), and the clock
//! (`clock/status`, whose phase wrap marks a slot boundary). It publishes the
//! engine's [`QsoState`] on `qso/{id}/state` and logs completed contacts on
//! `logbook/entries`.
//!
//! Transmission is **gated behind `allow_transmit`** and, until the PTT interlock
//! granter + audio-TX codec exist, has nowhere to go — so the task computes and
//! publishes `QsoState.next_tx` (what it *would* send) but does not put anything
//! on the air. This mirrors `core`'s TX hard-block (`docs/qso_flow.md`, `TODO.md`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bus::types::{
    AbsHz, Band, ClockStatus, Decode, InterlockReply, InterlockRequest, LogEntry, OffsetHz,
    OverAirMode, QsoCommand, QsoId, RadioId, Selection, StationId, Timestamp, TxAck, TxRequest,
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

/// FT8 T/R period. TODO: derive from the active mode once `OperatingState` is
/// published (FT4 = 7.5 s).
const SLOT_PERIOD_MS: u64 = 15_000;

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

    let mut prev_phase = 1.0f32;
    let mut seq: u64 = 0;

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
                    let boundary = cs.slot_phase < prev_phase;
                    prev_phase = cs.slot_phase;
                    if !boundary {
                        continue;
                    }
                    engine.step(Event::Tick { slot: current_slot() })
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
) {
    let _ = bus.publish(&Topic::QsoState(radio.clone()), step.state);

    if let Some(tx) = step.tx {
        if allow_transmit {
            // Hand the over to the audio-TX codec on its own task (acquire token →
            // request TxRequest → release). The engine loop keeps running so it
            // still services decodes/clock during the ~13 s transmission.
            spawn_transmit(bus.clone(), radio.clone(), tx);
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
            build_log(done, radio.clone(), my_station.clone(), *seq),
        );
    }
}

/// Transmit one over on its own task: acquire the PTT token from the granter, hand
/// the message to the audio-TX codec (which keys, plays, and reports on
/// `tx_report`), then release the token so the next slot can acquire it. Spawned so
/// the engine loop keeps servicing decodes/clock through the ~13 s over.
fn spawn_transmit(bus: BusHandle, radio: RadioId, tx: TxIntent) {
    tokio::spawn(async move {
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

        let req = TxRequest::SlottedMessage {
            radio: radio.clone(),
            // TODO: derive the mode from OperatingState (FT4 = 7.5 s slots) once it
            // is published; the audio-TX codec only synthesizes FT8 today.
            mode: OverAirMode::Ft8,
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

/// Build a [`LogEntry`] from a completed contact. Band/freq/mode are placeholders
/// until the engine subscribes to `RigState`/`OperatingState`; logging is dormant
/// while TX is blocked (no QSO can complete on the air), so this is wired for
/// correctness, not yet exercised. TODO: stamp real band/freq/mode/time.
fn build_log(done: CompletedQso, radio: RadioId, origin: StationId, seq: u64) -> LogEntry {
    LogEntry {
        id: QsoId {
            origin: origin.clone(),
            seq,
        },
        origin,
        radio: Some(radio),
        call: done.call,
        mode: OverAirMode::Ft8,
        band: Band::B20m,
        freq: AbsHz(14_074_000),
        time: Timestamp(now_ms() as i64),
        exchange_sent: done.exchange_sent,
        exchange_rcvd: done.exchange_rcvd,
        grid: done.grid,
    }
}

fn station_call(shared: &Arc<Mutex<StationConfig>>) -> StationId {
    StationId(shared.lock().unwrap().call.0.clone())
}

/// The current slot id, derived from UTC. Consistent parity with UTC-aligned
/// decode slots; the WAV-replay path numbers slots differently (a known seam to
/// reconcile when TX lands — `docs/live_pipeline_notes.md`).
fn current_slot() -> types::SlotId {
    types::SlotId(now_ms() / SLOT_PERIOD_MS)
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
