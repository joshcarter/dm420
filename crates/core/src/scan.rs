//! The band-scanner I/O shell — drives the pure [`scanner::Scanner`] engine.
//!
//! Serves `scanner/command`; while a survey runs it time-slices the receiver across
//! the selected bands in **both FT8 and FT4**, dwelling 2 slots per stop (even/odd
//! parity) and looping until cancelled. It blocks TX for the whole sweep, and
//! restores the operator's band+mode when cancelled.
//!
//! Why this lives in `core` and not the `scanner` crate: switching FT8↔FT4 means
//! reconfiguring capture through the in-process [`AudioControl`] handle (it restarts
//! the capture session — there is no bus command for it), and blocking TX means
//! holding the in-process [`Granter`] token. Both handles live here. The *pure*
//! sweep logic stays in the `scanner` crate so it's testable without hardware.
//!
//! Hardware-timing notes (tune on a real rig): a mode hop restarts capture, so each
//! stop's 2-slot count begins only at the next clean slot boundary after the retune
//! (the in-progress slot is skipped via a `prev_slot` resync). A band-only hop is a
//! cheap CAT retune. A full FT8+FT4 × 4-band pass is therefore a few minutes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bus::types as t;
use bus::{BusHandle, Topic, TopicSelector};
use modes::Protocol;
use scanner::{Phase, Scanner, Step};

use crate::control::AudioControl;
use crate::interlock::Granter;

/// The modes every survey sweeps, in plan order (FT8 first; the engine is
/// mode-major so capture restarts only at the FT8→FT4 boundary).
const SURVEY_MODES: [t::OverAirMode; 2] = [t::OverAirMode::Ft8, t::OverAirMode::Ft4];

/// How many recent slots to keep in the slot→band map. Decodes lag their slot by
/// only a slot or two, so 16 is comfortably enough to credit a late decode right.
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
    // The PTT token held while scanning (blocks the QSO engine from keying), and the
    // operating state to put back on cancel.
    let mut token: Option<t::InterlockToken> = None;
    let mut saved: Option<(t::AbsHz, Protocol)> = None;
    let mut latest_vfo: Option<t::AbsHz> = None;
    // Last clock slot seen. `None` means "resync" — seed without ticking, so the
    // partial slot in progress right after a retune isn't counted.
    let mut prev_slot: Option<t::SlotId> = None;
    // The band/mode currently tuned, and the band each recent slot was received on —
    // so a decode that arrives a slot or two late (the decoder runs after the slot
    // ends) is credited to the band that was on the air then, not whatever we've
    // since hopped to.
    let mut active_band: Option<t::Band> = None;
    let mut active_mode: Option<t::OverAirMode> = None;
    let mut slot_band: HashMap<t::SlotId, t::Band> = HashMap::new();

    publish_state(&bus, &engine, None);
    tracing::info!("scanner: service ready (idle)");

    loop {
        tokio::select! {
            m = cmds.next() => {
                let Some((cmd, responder)) = m else { break };
                match cmd {
                    t::ScannerCommand::StartSurvey { bands, dwell_slots } => {
                        // Remember where the operator was so we can restore it.
                        let cur_proto = audio.as_ref().map(|a| a.snapshot().1).unwrap_or(Protocol::Ft8);
                        saved = latest_vfo.map(|vfo| (vfo, cur_proto));
                        token = acquire(&granter);
                        if let Some((mode, band)) = engine.start(&bands, &SURVEY_MODES, dwell_slots) {
                            active_band = Some(band);
                            active_mode = Some(mode);
                            slot_band.clear();
                            prev_slot = None; // resync onto the first stop
                            apply_stop(&bus, &radio, audio.as_deref(), mode, band).await;
                            publish_state(&bus, &engine, None);
                            tracing::info!(?bands, "scanner: survey started");
                        } else {
                            tracing::warn!("scanner: no scannable band/mode stops; not starting");
                            if let Some(tok) = token.take() { granter.release(tok); }
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
                        publish_state(&bus, &engine, Some(now_ms()));
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
                // Record which band this slot is being received on (to credit
                // late-arriving decodes), pruning the map to recent slots.
                if let Some(b) = active_band {
                    slot_band.insert(cs.slot, b);
                    slot_band.retain(|s, _| s.0 + SLOT_BAND_KEEP >= cs.slot.0);
                }
                // Seed `prev_slot` on the first message without firing (we may be
                // mid-slot, and a resync sets it to `None`), then tick on each change.
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
                    prev_slot = None; // skip the partial slot after the retune
                }
                if let Some((_, band)) = engine.current() {
                    publish_band(&bus, &engine, band);
                }
                publish_state(&bus, &engine, None);
            }
            r = decodes.recv() => {
                let Ok(d) = r else { continue };
                if let t::DecodeContent::Slotted { slot, message, .. } = &d.content {
                    // Credit the decode to the band tuned when its slot was on the air
                    // (it arrives a slot or two after), falling back to the current band.
                    if let Some(band) = slot_band.get(slot).copied().or(active_band) {
                        engine.on_decode(band, message);
                        publish_band(&bus, &engine, band);
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
            last_scan: last_scan_ms.map(t::Timestamp),
        },
    );
}

/// Publish `band`'s live tally on `scanner/candidates`. One band per publish (the GUI
/// accumulates by band). Called as decodes are credited and on each slot boundary, so
/// a band's count surfaces as soon as a station is heard on it — not a sweep later.
fn publish_band(bus: &BusHandle, engine: &Scanner, band: t::Band) {
    if let Some(tally) = engine.tallies().into_iter().find(|t| t.band == band) {
        let _ = bus.publish(
            &Topic::ScannerCandidates,
            t::BandActivity {
                band: tally.band,
                stations_seen: tally.seen,
                unworked: tally.unworked,
                t: t::Timestamp(now_ms()),
            },
        );
    }
}

/// Map an over-air mode to the capture protocol. FT8/FT4 are direct; the
/// architecture-only modes fall back to FT8 (the scanner only sweeps FT8/FT4).
fn proto_of(mode: t::OverAirMode) -> Protocol {
    match mode {
        t::OverAirMode::Ft4 => Protocol::Ft4,
        _ => Protocol::Ft8,
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
