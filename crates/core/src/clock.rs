//! Wall-clock slot timing — the authoritative slot identity for the whole app.
//!
//! Publishes `clock/status` on a **mode-aware** cadence: the slot period follows
//! the active mode (FT8 = 15 s, FT4 = 7.5 s), so the [`SlotId`] here stays
//! commensurate with the decode pipeline's numbering ([`crate::decode`]) and the
//! QSO sequencer's tick parity. It replaces the old mock clock, which was
//! hardcoded to 15 s yet silently wedged into the real path via
//! `mocks::spawn_support` — the root of the FT4 "armed but never transmits" bug
//! (an FT4 contact's TX parity, derived from 7.5 s decode slots, never matched a
//! 15 s clock tick).
//!
//! In live capture the period tracks the operator's selected mode through the
//! shared [`AudioControl`]; for WAV replay / no capture it uses the static
//! protocol the session was configured with.
//!
//! These are UTC-aligned wall-clock slots, matching the live decoder. The
//! WAV-replay decoder numbers slots by sample position instead, so clock ticks and
//! WAV decode slots don't line up — a known, pre-existing seam to reconcile when TX
//! lands on the replay path (`docs/live_pipeline_notes.md`); replay isn't used for
//! real QSOs.

use std::sync::Arc;
use std::time::Duration;

use bus::types::{ClockStatus, SlotId};
use bus::{BusHandle, Topic};
use modes::Protocol;

use crate::control::AudioControl;

/// How often to republish the slot phase. Small enough that the QSO shell detects
/// a slot boundary promptly — this interval bounds the worst-case lateness added
/// to every transmit, so it matches the old mock clock's 50 ms heartbeat.
const TICK_MS: u64 = 50;

/// Spawn the clock producer onto `bus`. When `audio` is present (live capture) the
/// slot period follows the live mode the operator selects; otherwise (WAV replay /
/// no capture) it uses the static `fallback` protocol. Must be called from within
/// a tokio runtime (like the rest of [`crate::spawn`]).
pub fn spawn(bus: &BusHandle, audio: Option<Arc<AudioControl>>, fallback: Protocol) {
    let bus = bus.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
        loop {
            tick.tick().await;
            let proto = audio.as_ref().map(|a| a.snapshot().1).unwrap_or(fallback);
            let slot_ms = (modes::slot_period(proto) * 1000.0) as i64;
            if slot_ms <= 0 {
                continue;
            }
            let ms = now_ms();
            // Identical formula to `decode::publish_one`, so clock slots and decode
            // slots share one numbering — the property the QSO parity check needs.
            let slot = SlotId(ms.div_euclid(slot_ms) as u64);
            let slot_phase = (ms.rem_euclid(slot_ms) as f32) / slot_ms as f32;
            let _ = bus.publish(
                &Topic::ClockStatus,
                ClockStatus {
                    // No NTP offset measurement yet; the UI sync indicator reads 0.
                    offset_ms: 0.0,
                    slot_phase,
                    slot,
                },
            );
        }
    });
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
