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

use bus::types::{
    AbsHz, Band, ClockStatus, Decode, LogEntry, OffsetHz, OverAirMode, QsoCommand, QsoId, RadioId,
    Selection, StationId, Timestamp,
};
use bus::{BusHandle, BusMessage, DeliveryClass, Topic, TopicSelector};
use serde::{Deserialize, Serialize};

use crate::engine::{CompletedQso, Engine, Event, Step};
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
            // TODO(tx): request a PTT interlock token from the granter and place a
            // `TxRequest::SlottedMessage` on `radio/{id}/audio_tx`. The granter +
            // codec don't exist yet (TX hard-blocked), so this is unreachable today.
            tracing::warn!(
                "qso: allow_transmit set but no TX path exists; dropping {:?}",
                tx.message.text
            );
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
