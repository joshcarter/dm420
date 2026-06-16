//! Rig adapter: Joel's rig actor ⇆ the bus, with a supervised connection.
//!
//! A single **supervisor** thread owns the rig's whole lifecycle: open the CAT
//! port (with autodetect fallback), publish `RigState` (latest-wins State) from
//! periodic `IF;` snapshots, and — crucially — never panic. A missing or
//! disconnected radio is reported as [`SubsystemHealth`] on `health/rig` and the
//! supervisor keeps retrying with backoff, so the app stays up and recovers when
//! the radio comes back.
//!
//! The `RigCommand` server is started once and forwards to whatever handle is
//! currently connected (a shared `Option<RigHandle>`); while the rig is down it
//! replies `Err("rig offline")` rather than blocking or failing the caller.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bus::types as t;
use bus::{BusHandle, BusMessage, DeliveryClass, Topic};
use rig::{RigHandle, Vfo};

use crate::SerialConfig;
use crate::health;
use crate::map;

/// Reply type for the rig command topic. Command reply types are chosen per call
/// site (the orphan rule lets us `impl BusMessage` here), so this lives with the
/// server rather than in the shared `types` crate.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum CommandResult {
    Ok,
    Err(String),
}

impl BusMessage for CommandResult {
    const CLASS: DeliveryClass = DeliveryClass::Command;
}

/// How often the poller republishes rig state (latest-wins, so this is just a
/// liveness heartbeat for meters/PTT that change outside our commands).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Consecutive failed `get_state()` polls before we declare the link lost and
/// reconnect. A single timeout is common on a busy radio; this rides those out.
const FAILS_BEFORE_RECONNECT: u32 = 3;

/// Reconnect backoff: start short (a quick replug recovers fast) and cap so a
/// truly absent radio doesn't busy-loop the port.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(15);

/// Per-attempt listen window during an autodetect sweep.
const AUTODETECT_WINDOW: Duration = Duration::from_millis(250);

/// The currently-connected rig handle, shared between the supervisor (which
/// swaps it on (re)connect) and the command server (which reads it per request).
/// `None` while disconnected.
type SharedHandle = Arc<Mutex<Option<RigHandle>>>;

pub fn spawn(bus: &BusHandle, radio: t::RadioId, allow_transmit: bool, serial: SerialConfig) {
    let shared: SharedHandle = Arc::new(Mutex::new(None));
    // The command server outlives any single connection; it reads `shared`.
    serve_commands(bus, radio.clone(), shared.clone());

    // The supervisor owns connect/poll/reconnect on its own std thread (serial
    // I/O is blocking). It never returns.
    let bus = bus.clone();
    std::thread::Builder::new()
        .name("rig-supervisor".into())
        .spawn(move || supervise(bus, radio, allow_transmit, serial, shared))
        .expect("spawn rig supervisor");
}

/// Publish a rig health transition (deduplicated; see [`health::set`]).
fn set_health(bus: &BusHandle, last: &mut Option<t::HealthState>, state: t::HealthState) {
    health::set(bus, t::SubsystemId::Rig, last, state);
}

/// The supervisor loop: (re)connect, poll until the link is lost, back off, retry.
fn supervise(
    bus: BusHandle,
    radio: t::RadioId,
    allow_transmit: bool,
    serial: SerialConfig,
    shared: SharedHandle,
) {
    let mut last_health: Option<t::HealthState> = None;
    let mut backoff = BACKOFF_START;

    loop {
        match open(&serial) {
            Ok((desc, dev)) => {
                tracing::info!("rig connected: {desc}");
                let (handle, _join) = rig::spawn(Box::new(dev), allow_transmit);
                *shared.lock().unwrap() = Some(handle.clone());
                set_health(&bus, &mut last_health, t::HealthState::Healthy);
                publish_state(&bus, &radio, &handle);
                backoff = BACKOFF_START; // a good connection resets the backoff

                // Block here polling state until the link drops.
                run_until_lost(&bus, &radio, &handle, &mut last_health);

                // Release the device before retrying: clear the shared handle and
                // drop ours so the actor thread (and its serial port) shut down.
                *shared.lock().unwrap() = None;
                drop(handle);
                set_health(
                    &bus,
                    &mut last_health,
                    t::HealthState::Down("rig link lost — reconnecting".into()),
                );
            }
            Err(e) => {
                tracing::warn!("rig connect failed: {e}");
                set_health(&bus, &mut last_health, t::HealthState::Down(e));
            }
        }

        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Poll `IF;` snapshots until `FAILS_BEFORE_RECONNECT` consecutive failures, then
/// return so the supervisor can reconnect. A first failure flips health to
/// `Degraded`; a recovered poll restores `Healthy`.
fn run_until_lost(
    bus: &BusHandle,
    radio: &t::RadioId,
    handle: &RigHandle,
    last_health: &mut Option<t::HealthState>,
) {
    let mut fails: u32 = 0;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        match handle.get_state() {
            Ok(s) => {
                fails = 0;
                set_health(bus, last_health, t::HealthState::Healthy);
                let _ = bus.publish(
                    &Topic::RigState(radio.clone()),
                    map::to_bus_rig_state(radio.clone(), &s),
                );
            }
            Err(e) => {
                fails += 1;
                tracing::warn!("rig poll failed ({fails}/{FAILS_BEFORE_RECONNECT}): {e}");
                if fails >= FAILS_BEFORE_RECONNECT {
                    return;
                }
                set_health(
                    bus,
                    last_health,
                    t::HealthState::Degraded("rig not responding".into()),
                );
            }
        }
    }
}

/// Open the rig per `serial`: try the explicit port first, then (if enabled) an
/// autodetect sweep. Returns a human description plus the opened device, or a
/// short error string for the health message.
fn open(serial: &SerialConfig) -> Result<(String, rig::KenwoodRig), String> {
    if let Some(port) = &serial.port {
        match rig::open_serial(port, serial.baud, serial.profile) {
            Ok(d) => return Ok((format!("{port} @ {} baud", serial.baud), d)),
            Err(e) if serial.autodetect => {
                tracing::warn!(
                    "rig open on {port} @ {} baud failed ({e}); falling back to autodetect",
                    serial.baud
                );
            }
            Err(e) => return Err(format!("open {port} @ {} baud: {e}", serial.baud)),
        }
    }

    if serial.autodetect {
        autodetect_open()
    } else {
        Err("no serial port configured and autodetect disabled".into())
    }
}

/// Sweep the likely-radio ports × standard Kenwood bauds × line profiles for a
/// responding radio, then open the winner.
fn autodetect_open() -> Result<(String, rig::KenwoodRig), String> {
    let ports = rig::probe::candidate_ports(false).map_err(|e| format!("listing serial ports: {e}"))?;
    if ports.is_empty() {
        return Err("no serial ports found to autodetect".into());
    }
    let report = rig::probe::autodetect(
        &ports,
        rig::probe::KENWOOD_BAUDS,
        rig::probe::PROBE_PROFILES,
        AUTODETECT_WINDOW,
        |line| tracing::info!("rig autodetect: {line}"),
    );
    let w = report
        .winner
        .ok_or_else(|| "autodetect found no radio on any port/baud".to_string())?;
    let dev = rig::open_serial(&w.port, w.baud, w.profile)
        .map_err(|e| format!("open {} @ {} baud: {e}", w.port, w.baud))?;
    Ok((
        format!("{} @ {} baud [{}] (autodetected)", w.port, w.baud, w.profile.label()),
        dev,
    ))
}

/// Read one `IF;` snapshot and publish it as dm420 `RigState`.
fn publish_state(bus: &BusHandle, radio: &t::RadioId, handle: &RigHandle) {
    if let Ok(s) = handle.get_state() {
        let _ = bus.publish(
            &Topic::RigState(radio.clone()),
            map::to_bus_rig_state(radio.clone(), &s),
        );
    }
}

fn serve_commands(bus: &BusHandle, radio: t::RadioId, shared: SharedHandle) {
    let mut server = match bus.serve::<t::RigCommand, CommandResult>(&Topic::RigCommand(radio.clone()))
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("rig command server not started: {e}");
            return;
        }
    };
    let bus = bus.clone();
    tokio::spawn(async move {
        while let Some((cmd, responder)) = server.next().await {
            let shared = shared.clone();
            let radio = radio.clone();
            let bus = bus.clone();
            // The rig call blocks; keep it off the async executor.
            let reply = tokio::task::spawn_blocking(move || {
                // Snapshot the live handle; if the rig is down, fail fast rather
                // than block the caller.
                let handle = match shared.lock().unwrap().clone() {
                    Some(h) => h,
                    None => return CommandResult::Err("rig offline".into()),
                };
                apply(&handle, &cmd, &radio, &bus)
            })
            .await
            .unwrap_or_else(|_| CommandResult::Err("rig task panicked".into()));
            responder.reply(reply);
        }
    });
}

/// Apply one command to the rig and, on success, publish fresh state so the UI
/// reflects the change without waiting for the next poll.
fn apply(handle: &RigHandle, cmd: &t::RigCommand, radio: &t::RadioId, bus: &BusHandle) -> CommandResult {
    let res = match cmd {
        t::RigCommand::SetFreq(t::AbsHz(hz)) => handle.set_freq(Vfo::A, *hz),
        t::RigCommand::SetRigMode(m) => {
            let (mode, _data) = map::to_rig_mode(*m);
            handle.set_mode(mode)
        }
        // Interlock-token validation belongs to the (future) core granter; for now
        // Joel's actor enforces the TX gate + watchdog, so we forward the request.
        t::RigCommand::PttRequest { on, token: _ } => handle.set_ptt(*on),
    };
    match res {
        Ok(()) => {
            publish_state(bus, radio, handle);
            CommandResult::Ok
        }
        Err(e) => CommandResult::Err(e.to_string()),
    }
}
