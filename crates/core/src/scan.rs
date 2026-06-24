//! The band-scanner I/O shell — drives the pure [`scanner::Scanner`] engine.
//!
//! Serves `scanner/command`; while a survey runs it time-slices the receiver across
//! the selected `(band, mode)` stops in **both FT8 and FT4**, dwelling 2 slots per
//! stop (even/odd parity) and looping until cancelled. It blocks TX for the whole
//! sweep, restores the operator's band+mode when cancelled, and supports changing
//! the stops live (`SetStops`) without resetting the counts.
//!
//! Why this lives in `core` and not the `scanner` crate: switching FT8↔FT4 means
//! reconfiguring capture through the in-process [`AudioControl`] handle (it restarts
//! the capture session — there is no bus command for it), and blocking TX means
//! holding the in-process [`Granter`] token. Both handles live here. The *pure*
//! sweep logic stays in the `scanner` crate so it's testable without hardware.
//!
//! **Decode attribution.** A decode arrives a slot or two after its audio (the
//! decoder runs once the slot ends), so by the time it lands the sweep may have
//! hopped. The decode carries the grid-aligned id of the slot it decoded, and the
//! shell records, per slot id, which band that slot was received on — crucially
//! **after** `on_slot` runs, so the slot that *starts at a hop boundary* is credited
//! to the band we just tuned to, not the one we left. The partial slot in progress
//! when a scan first starts is the resync seed and is never recorded, so pre-scan
//! traffic on the band the operator was on is dropped, not mis-credited. A decode
//! whose slot we never recorded is dropped.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bus::types as t;
use bus::{BusHandle, Topic, TopicSelector};
use modes::Protocol;
use scanner::{Phase, Scanner, Step};

use crate::control::AudioControl;
use crate::interlock::Granter;

/// How many recent slot→band entries to keep. Decodes lag their slot by only a slot
/// or two, so a small window is plenty.
const SLOT_BAND_KEEP: u64 = 16;

/// Launch the band-scanner service onto `bus`. `audio` is the live-capture mode
/// handle (`None` for WAV/no-capture: the survey still sweeps bands but can't switch
/// mode); `granter` is the PTT interlock authority it holds to block TX while
/// scanning. Spawns a detached tokio task, like the rest of [`crate::spawn`].
pub fn spawn(bus: &BusHandle, radio: t::RadioId, audio: Option<Arc<AudioControl>>, granter: Granter) {
    tokio::spawn(run(bus.clone(), radio, audio, granter));
}

async fn run(bus: BusHandle, radio: t::RadioId, audio: Option<Arc<AudioControl>>, granter: Granter) {
    let mut cmds = match bus.serve::<t::ScannerCommand, t::ScannerAck>(&Topic::ScannerCommand) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("scanner: cannot serve commands: {e:?}");
            return;
        }
    };
    let mut clock = match bus.subscribe::<t::ClockStatus>(TopicSelector::Exact(Topic::ClockStatus)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("scanner: cannot subscribe clock: {e:?}");
            return;
        }
    };
    let mut decodes =
        match bus.subscribe::<t::Decode>(TopicSelector::Exact(Topic::Decodes(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("scanner: cannot subscribe decodes: {e:?}");
                return;
            }
        };
    let mut logs = match bus.subscribe::<t::LogEntry>(TopicSelector::Exact(Topic::LogbookEntries)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("scanner: cannot subscribe logbook: {e:?}");
            return;
        }
    };
    let mut rig = match bus.subscribe::<t::RigState>(TopicSelector::Exact(Topic::RigState(radio.clone()))) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("scanner: cannot subscribe rig_state: {e:?}");
            return;
        }
    };

    let mut engine = Scanner::new();
    // The PTT token held while scanning, and the operating state to restore on cancel.
    let mut token: Option<t::InterlockToken> = None;
    let mut saved: Option<(t::AbsHz, Protocol)> = None;
    let mut latest_vfo: Option<t::AbsHz> = None;
    // The band/mode currently tuned, and which band each recent slot was received on.
    let mut active_band: Option<t::Band> = None;
    let mut active_mode: Option<t::OverAirMode> = None;
    let mut slot_band: HashMap<t::SlotId, t::Band> = HashMap::new();
    // Slot resync: `None` seeds without counting/recording, so the partial slot in
    // progress right after a (re)tune isn't counted toward the dwell or attributed.
    let mut prev_slot: Option<t::SlotId> = None;

    publish_state(&bus, &engine, None);
    tracing::info!("scanner: service ready (idle)");

    loop {
        tokio::select! {
            m = cmds.next() => {
                let Some((cmd, responder)) = m else { break };
                match cmd {
                    t::ScannerCommand::StartSurvey { stops, dwell_slots } => {
                        // Remember where the operator was so we can restore it.
                        let cur_proto = audio.as_ref().map(|a| a.snapshot().1).unwrap_or(Protocol::Ft8);
                        saved = latest_vfo.map(|vfo| (vfo, cur_proto));
                        token = acquire(&granter);
                        if let Some((mode, band)) = engine.start(&stops, dwell_slots) {
                            active_band = Some(band);
                            active_mode = Some(mode);
                            slot_band.clear();
                            prev_slot = None; // skip the partial slot we tuned into
                            apply_stop(&bus, &radio, audio.as_deref(), mode, band).await;
                            publish_snapshot(&bus, &engine); // a fresh scan → zeroed counts
                            publish_state(&bus, &engine, None);
                            tracing::info!(stops = stops.len(), "scanner: survey started");
                        } else {
                            tracing::warn!("scanner: no scannable band/mode stops; not starting");
                            if let Some(tok) = token.take() { granter.release(tok); }
                        }
                    }
                    t::ScannerCommand::SetStops { stops } => {
                        if engine.phase() == Phase::Scanning {
                            let changed = engine.update_stops(&stops);
                            if engine.phase() != Phase::Scanning {
                                // Everything toggled off — wind down like Cancel.
                                restore(&bus, &radio, audio.as_deref(), saved.take()).await;
                                if let Some(tok) = token.take() { granter.release(tok); }
                                active_band = None;
                                active_mode = None;
                                slot_band.clear();
                                prev_slot = None;
                                publish_state(&bus, &engine, Some(types::now_ms()));
                            } else {
                                if changed {
                                    // The dwelled stop was removed — retune to the new one.
                                    if let Some((mode, band)) = engine.current() {
                                        if active_mode != Some(mode) { slot_band.clear(); }
                                        active_band = Some(band);
                                        active_mode = Some(mode);
                                        apply_stop(&bus, &radio, audio.as_deref(), mode, band).await;
                                        prev_slot = None;
                                    }
                                }
                                publish_snapshot(&bus, &engine);
                                publish_state(&bus, &engine, None);
                            }
                        }
                    }
                    t::ScannerCommand::Cancel => {
                        engine.cancel();
                        restore(&bus, &radio, audio.as_deref(), saved.take()).await;
                        if let Some(tok) = token.take() { granter.release(tok); }
                        active_band = None;
                        active_mode = None;
                        slot_band.clear();
                        prev_slot = None;
                        publish_state(&bus, &engine, Some(types::now_ms()));
                        tracing::info!("scanner: survey cancelled");
                    }
                }
                responder.reply(t::ScannerAck::Ok);
            }
            r = clock.recv() => {
                let Ok(cs) = r else { continue };
                if engine.phase() != Phase::Scanning {
                    prev_slot = Some(cs.slot);
                    continue;
                }
                // Seed `prev_slot` on the first message without firing (resync sets it
                // to `None`), then act on each slot change. The seed slot — the partial
                // one in progress right after a (re)tune — is never recorded.
                let boundary = prev_slot.is_some_and(|p| p != cs.slot);
                prev_slot = Some(cs.slot);
                if !boundary {
                    continue;
                }
                // Keep TX blocked: extend the grant before the ~20 s TTL lapses.
                if let Some(tok) = token { granter.refresh(tok); }
                if let Step::Hop { mode, band } = engine.on_slot() {
                    if active_mode != Some(mode) {
                        slot_band.clear(); // slot numbering changes with the mode
                    }
                    active_band = Some(band);
                    active_mode = Some(mode);
                    apply_stop(&bus, &radio, audio.as_deref(), mode, band).await;
                    prev_slot = None; // resync onto the new stop
                }
                // Record AFTER on_slot: the slot starting at a hop boundary is on the
                // band we just tuned to, so it must get `active_band`'s *new* value.
                if let Some(b) = active_band {
                    slot_band.insert(cs.slot, b);
                    slot_band.retain(|s, _| s.0 + SLOT_BAND_KEEP >= cs.slot.0);
                }
                publish_snapshot(&bus, &engine);
                publish_state(&bus, &engine, None);
            }
            r = decodes.recv() => {
                let Ok(d) = r else { continue };
                let mode = d.mode;
                if let t::DecodeContent::Slotted { slot, message, .. } = &d.content {
                    // Credit to the band recorded for this exact slot id. Slots we never
                    // recorded (the partial (re)tune slot, or pre-scan traffic on the
                    // band we tuned away from) are dropped, not mis-credited.
                    if let Some(band) = slot_band.get(slot).copied() {
                        engine.on_decode(band, mode, message);
                        publish_snapshot(&bus, &engine);
                    }
                }
            }
            r = logs.recv() => {
                let Ok(e) = r else { continue };
                engine.on_logged(e.call, e.band);
            }
            r = rig.recv() => {
                let Ok(s) = r else { continue };
                latest_vfo = Some(s.vfo);
            }
            else => break,
        }
    }
}

/// Acquire the PTT token so the QSO engine can't key while we sweep. Best-effort: a
/// denial (someone mid-over) is logged, and the survey proceeds with TX-block not
/// guaranteed — scanning is meant to start from an idle radio.
fn acquire(granter: &Granter) -> Option<t::InterlockToken> {
    match granter.acquire() {
        t::InterlockReply::Granted { token, .. } => Some(token),
        other => {
            tracing::warn!(?other, "scanner: could not hold TX interlock; TX block is best-effort");
            None
        }
    }
}

/// Tune the rig for a stop: switch mode (only when it changed — a mode switch
/// restarts capture) and retune the dial to the band/mode calling frequency.
async fn apply_stop(
    bus: &BusHandle,
    radio: &t::RadioId,
    audio: Option<&AudioControl>,
    mode: t::OverAirMode,
    band: t::Band,
) {
    if let Some(a) = audio {
        let (input, cur) = a.snapshot();
        let want = proto_of(mode);
        if cur != want {
            a.set(input, want);
        }
    }
    if let Some(freq) = t::calling_freq(band, mode) {
        set_freq(bus, radio, freq).await;
    }
    tracing::info!(?mode, ?band, "scanner: dwelling");
}

/// Put the operator's pre-scan mode + dial frequency back (the "return to normal
/// operating state" of the spec). No-op if we never captured a starting state.
async fn restore(
    bus: &BusHandle,
    radio: &t::RadioId,
    audio: Option<&AudioControl>,
    saved: Option<(t::AbsHz, Protocol)>,
) {
    let Some((vfo, proto)) = saved else { return };
    if let Some(a) = audio {
        let input = a.snapshot().0;
        a.set(input, proto);
    }
    set_freq(bus, radio, vfo).await;
}

/// Command the rig to a dial frequency (fire-and-forget; logs on failure).
async fn set_freq(bus: &BusHandle, radio: &t::RadioId, freq: t::AbsHz) {
    if let Err(e) = bus
        .request::<t::RigCommand, crate::CommandResult>(
            &Topic::RigCommand(radio.clone()),
            t::RigCommand::SetFreq(freq),
            Duration::from_secs(2),
        )
        .await
    {
        tracing::warn!(error = ?e, "scanner: set-frequency command failed");
    }
}

/// Publish the scanner run state. `last_scan` is `Some` only when a scan just
/// stopped (drives the panel's "Last scan: N min ago").
fn publish_state(bus: &BusHandle, engine: &Scanner, last_scan_ms: Option<i64>) {
    let status = match engine.phase() {
        Phase::Scanning => t::ScanStatus::Scanning,
        Phase::Idle => t::ScanStatus::Idle,
    };
    let _ = bus.publish(
        &Topic::ScannerState,
        t::ScannerState {
            status,
            current: engine.current().map(|(_, band)| band),
            current_mode: engine.current().map(|(mode, _)| mode),
            last_scan: last_scan_ms.map(t::Timestamp),
        },
    );
}

/// Publish the full per-scan snapshot on `scanner/candidates`: one [`t::BandActivity`]
/// per scanned `(band, mode)`, as a single State value. Sent on each decode and slot
/// boundary (and zeroed on Start), so the panel always sees a complete, current set —
/// no accumulation, no coalescing, and counts reset cleanly when a scan begins.
fn publish_snapshot(bus: &BusHandle, engine: &Scanner) {
    let now = types::now_ms();
    let snapshot: Vec<t::BandActivity> = engine
        .tallies()
        .into_iter()
        .map(|tl| t::BandActivity {
            band: tl.band,
            mode: tl.mode,
            heard: tl.heard,
            cq: tl.cq,
            unworked: tl.unworked,
            t: t::Timestamp(now),
        })
        .collect();
    let _ = bus.publish(&Topic::ScannerCandidates, snapshot);
}

/// Map an over-air mode to the capture protocol. FT8/FT4 are direct; the
/// architecture-only modes fall back to FT8 (the scanner only sweeps FT8/FT4).
fn proto_of(mode: t::OverAirMode) -> Protocol {
    match mode {
        t::OverAirMode::Ft4 => Protocol::Ft4,
        _ => Protocol::Ft8,
    }
}
