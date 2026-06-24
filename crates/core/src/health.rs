//! Publishing subsystem-health transitions onto `health/{id}`.
//!
//! Producers call [`set`] as their liveness changes. The topic is latest-wins
//! State, so we deduplicate against the last value the caller published — only
//! emitting on a real transition keeps the UI from repainting on every heartbeat.

use bus::types as t;
use bus::{BusHandle, Topic};

/// Publish `state` for subsystem `id`, skipping it if equal to `*last`. Updates
/// `*last` to the new state when it does publish.
pub(crate) fn set(
    bus: &BusHandle,
    id: t::SubsystemId,
    last: &mut Option<t::HealthState>,
    state: t::HealthState,
) {
    if last.as_ref() == Some(&state) {
        return;
    }
    let _ = bus.publish(
        &Topic::Health(id),
        t::SubsystemHealth {
            id,
            state: state.clone(),
            since: t::Timestamp(types::now_ms()),
        },
    );
    *last = Some(state);
}
