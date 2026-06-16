//! Rig adapter: Joel's rig actor ⇆ the bus.
//!
//! Starts the rig actor (in-memory mock by default), publishes `RigState`
//! (latest-wins State) from periodic `IF;` snapshots, and serves `RigCommand`
//! (Command) by forwarding to the actor. The actor's `RigHandle` methods are
//! **blocking** (crossbeam round-trips), so the state poller runs on its own std
//! thread and each served command is applied via `spawn_blocking`.

use std::time::Duration;

use bus::types as t;
use bus::{BusHandle, BusMessage, DeliveryClass, Topic};
use rig::{RigHandle, Vfo};

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

pub fn spawn(bus: &BusHandle, radio: t::RadioId, allow_transmit: bool) {
    // The in-memory mock radio — swap for `rig::open_serial(..)` behind config to
    // drive real hardware. The JoinHandle is detached; cloned handles keep the
    // actor thread alive.
    let (handle, _join) = rig::spawn(Box::new(rig::mock_rig()), allow_transmit);

    // Seed the mock so the readout shows a sensible FT8 watering hole rather than
    // the cold-start default.
    let _ = handle.set_freq(Vfo::A, 14_074_000);
    let _ = handle.set_mode(rig::Mode::Usb);

    publish_state(bus, &radio, &handle);
    spawn_poller(bus.clone(), radio.clone(), handle.clone());
    serve_commands(bus, radio, handle);
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

fn spawn_poller(bus: BusHandle, radio: t::RadioId, handle: RigHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(POLL_INTERVAL);
            publish_state(&bus, &radio, &handle);
        }
    });
}

fn serve_commands(bus: &BusHandle, radio: t::RadioId, handle: RigHandle) {
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
            let handle = handle.clone();
            let radio = radio.clone();
            let bus = bus.clone();
            // The rig call blocks; keep it off the async executor.
            let reply = tokio::task::spawn_blocking(move || apply(&handle, &cmd, &radio, &bus))
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
