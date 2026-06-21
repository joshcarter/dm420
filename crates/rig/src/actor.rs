//! The rig actor: a thread that is the **sole owner** of the radio, serializing
//! all access through a channel. This is the invariant from ft8-app-design.md §4
//! — no other component touches the port, so CAT request/response framing can
//! never interleave. In the POC the transport is in-process channels; the request
//! and event types are `serde`-serializable so the future move onto a bus is a
//! transport swap, not a rewrite.
//!
//! The actor also enforces the PTT watchdog: a `SetPtt(true)` arms a deadline and
//! the loop auto-releases TX if it isn't refreshed, so a hung script or dropped
//! connection cannot leave the radio keyed. The deadline must outlast a full
//! single-slot over (a digital-mode TX keys once and stays keyed for the whole
//! ~13 s waveform); it is a backstop for a *stuck* over, not a mid-over heartbeat.
//! Re-keying mid-over to "refresh" it doesn't work on real Kenwoods — they reject
//! a `TX` command while already transmitting.

use crate::codec::{Mode, RigState, Vfo};
use crate::{Rig, RigError};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use serde::{Deserialize, Serialize};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Auto-release TX this long after the last `SetPtt(true)` unless refreshed.
/// Sized to outlast a full FT8 over (~13 s waveform, kept under `core::tx`'s
/// `max_tx_for` backstop) plus margin, so it never fires mid-over — the backstop for
/// a hung app or a missed key-down. NOTE: still FT8-sized (15 s = two FT4 slots); on
/// FT4 a stuck over only bleeds past its slot on a double failure (see
/// `docs/live_pipeline_notes.md` — make this slot-relative).
pub const PTT_WATCHDOG: Duration = Duration::from_secs(15);

/// A typed request to the rig. Mirrors the [`Rig`] surface; the Step 1.5 "full
/// control" commands (power, gains, …) ride on [`RigRequest::Raw`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RigRequest {
    SetFreq { vfo: Vfo, hz: u64 },
    GetFreq { vfo: Vfo },
    SetMode(Mode),
    GetMode,
    SetPtt(bool),
    GetState,
    Raw(String),
    SetAutoInfo(u8),
}

/// A typed response from the rig.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RigResponse {
    Ok,
    Freq(u64),
    Mode(Mode),
    State(RigState),
    Raw(Option<String>),
}

/// An unsolicited message from the radio (Auto-Information), forwarded to
/// subscribers (the `watch` command). Decoding for display happens at the edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigEvent {
    pub raw: String,
}

/// Internal actor mailbox message.
enum Msg {
    Request(RigRequest, Sender<Result<RigResponse, RigError>>),
    Subscribe(Sender<RigEvent>),
    Shutdown,
}

/// A cheap, cloneable handle to the rig actor. All methods are blocking
/// request/response round-trips.
#[derive(Clone)]
pub struct RigHandle {
    tx: Sender<Msg>,
}

impl RigHandle {
    fn request(&self, req: RigRequest) -> Result<RigResponse, RigError> {
        let (rtx, rrx) = bounded(1);
        self.tx
            .send(Msg::Request(req, rtx))
            .map_err(|_| RigError::ActorGone)?;
        rrx.recv().map_err(|_| RigError::ActorGone)?
    }

    pub fn set_freq(&self, vfo: Vfo, hz: u64) -> Result<(), RigError> {
        self.request(RigRequest::SetFreq { vfo, hz }).map(|_| ())
    }

    pub fn get_freq(&self, vfo: Vfo) -> Result<u64, RigError> {
        match self.request(RigRequest::GetFreq { vfo })? {
            RigResponse::Freq(f) => Ok(f),
            other => Err(RigError::Unexpected(format!("{other:?}"))),
        }
    }

    pub fn set_mode(&self, mode: Mode) -> Result<(), RigError> {
        self.request(RigRequest::SetMode(mode)).map(|_| ())
    }

    pub fn get_mode(&self) -> Result<Mode, RigError> {
        match self.request(RigRequest::GetMode)? {
            RigResponse::Mode(m) => Ok(m),
            other => Err(RigError::Unexpected(format!("{other:?}"))),
        }
    }

    pub fn set_ptt(&self, tx: bool) -> Result<(), RigError> {
        self.request(RigRequest::SetPtt(tx)).map(|_| ())
    }

    pub fn get_state(&self) -> Result<RigState, RigError> {
        match self.request(RigRequest::GetState)? {
            RigResponse::State(s) => Ok(s),
            other => Err(RigError::Unexpected(format!("{other:?}"))),
        }
    }

    pub fn raw(&self, cmd: &str) -> Result<Option<String>, RigError> {
        match self.request(RigRequest::Raw(cmd.to_string()))? {
            RigResponse::Raw(r) => Ok(r),
            other => Err(RigError::Unexpected(format!("{other:?}"))),
        }
    }

    pub fn set_auto_info(&self, level: u8) -> Result<(), RigError> {
        self.request(RigRequest::SetAutoInfo(level)).map(|_| ())
    }

    /// Subscribe to unsolicited radio events (used by `watch`). The actor starts
    /// polling the port for Auto-Information traffic while any subscriber lives.
    pub fn subscribe(&self) -> Result<Receiver<RigEvent>, RigError> {
        let (tx, rx) = unbounded();
        self.tx
            .send(Msg::Subscribe(tx))
            .map_err(|_| RigError::ActorGone)?;
        Ok(rx)
    }

    /// Ask the actor thread to stop (best effort).
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

/// Spawn the actor thread that owns `rig`. When `allow_transmit` is false, the
/// actor hard-blocks every request that would key the transmitter (`ptt on` and
/// any raw `TX` command), so transmit is impossible unless explicitly enabled —
/// the single chokepoint no caller can bypass. Returns a handle plus the join
/// handle so callers can shut it down cleanly.
pub fn spawn(rig: Box<dyn Rig>, allow_transmit: bool) -> (RigHandle, JoinHandle<()>) {
    let (tx, rx) = unbounded::<Msg>();
    let join = thread::Builder::new()
        .name("rig-actor".into())
        .spawn(move || actor_loop(rig, rx, allow_transmit))
        .expect("spawn rig actor");
    (RigHandle { tx }, join)
}

/// Whether a request would key the transmitter.
fn is_transmit_request(req: &RigRequest) -> bool {
    match req {
        RigRequest::SetPtt(true) => true,
        // `TX`, `TX0/1/2` all key up; match the mnemonic case-insensitively.
        RigRequest::Raw(cmd) => cmd.trim_start().to_uppercase().starts_with("TX"),
        _ => false,
    }
}

fn handle_request(rig: &mut dyn Rig, req: RigRequest) -> Result<RigResponse, RigError> {
    match req {
        RigRequest::SetFreq { vfo, hz } => rig.set_freq(vfo, hz).map(|_| RigResponse::Ok),
        RigRequest::GetFreq { vfo } => rig.get_freq(vfo).map(RigResponse::Freq),
        RigRequest::SetMode(m) => rig.set_mode(m).map(|_| RigResponse::Ok),
        RigRequest::GetMode => rig.get_mode().map(RigResponse::Mode),
        RigRequest::SetPtt(tx) => rig.set_ptt(tx).map(|_| RigResponse::Ok),
        RigRequest::GetState => rig.get_state().map(RigResponse::State),
        RigRequest::Raw(cmd) => rig.raw(&cmd).map(RigResponse::Raw),
        RigRequest::SetAutoInfo(level) => rig.set_auto_info(level).map(|_| RigResponse::Ok),
    }
}

fn actor_loop(mut rig: Box<dyn Rig>, rx: Receiver<Msg>, allow_transmit: bool) {
    let mut subs: Vec<Sender<RigEvent>> = Vec::new();
    let mut tx_deadline: Option<Instant> = None;
    info!(allow_transmit, "rig actor started");

    loop {
        // PTT watchdog: release TX if the deadline lapsed without a refresh.
        if let Some(dl) = tx_deadline {
            if Instant::now() >= dl {
                warn!("PTT watchdog: auto-releasing TX (no refresh within timeout)");
                if let Err(e) = rig.set_ptt(false) {
                    warn!(error = %e, "watchdog failed to release TX");
                }
                tx_deadline = None;
            }
        }

        // Wake often enough to fire the watchdog and poll events promptly.
        let wait = if tx_deadline.is_some() {
            Duration::from_millis(100)
        } else if subs.is_empty() {
            Duration::from_millis(500)
        } else {
            Duration::from_millis(150)
        };

        match rx.recv_timeout(wait) {
            Ok(Msg::Shutdown) => {
                info!("rig actor shutting down");
                if tx_deadline.is_some() {
                    let _ = rig.set_ptt(false);
                }
                break;
            }
            Ok(Msg::Subscribe(s)) => {
                debug!("new event subscriber");
                subs.push(s);
            }
            Ok(Msg::Request(req, reply)) => {
                if !allow_transmit && is_transmit_request(&req) {
                    warn!(?req, "TRANSMIT BLOCKED: --allow-transmit not set");
                    let _ = reply.send(Err(RigError::TransmitBlocked));
                    continue;
                }
                let is_ptt_on = matches!(req, RigRequest::SetPtt(true));
                let is_ptt_off = matches!(req, RigRequest::SetPtt(false));
                let result = handle_request(&mut *rig, req);
                if result.is_ok() {
                    if is_ptt_on {
                        tx_deadline = Some(Instant::now() + PTT_WATCHDOG);
                        debug!(?PTT_WATCHDOG, "TX armed; watchdog set");
                    } else if is_ptt_off {
                        tx_deadline = None;
                    }
                }
                let _ = reply.send(result);
                // Surface any events that arrived interleaved with the reply.
                drain_and_publish(&mut *rig, &mut subs, Duration::ZERO);
            }
            Err(RecvTimeoutError::Timeout) => {
                if !subs.is_empty() {
                    drain_and_publish(&mut *rig, &mut subs, wait);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                info!("rig actor: all handles dropped, exiting");
                break;
            }
        }
    }
}

/// Poll unsolicited messages and fan them out to live subscribers, pruning any
/// whose receiver has been dropped.
fn drain_and_publish(rig: &mut dyn Rig, subs: &mut Vec<Sender<RigEvent>>, timeout: Duration) {
    let events = match rig.pump_events(timeout) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "failed to pump events");
            return;
        }
    };
    for raw in events {
        debug!(%raw, "rig event");
        let event = RigEvent { raw };
        subs.retain(|s| s.send(event.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CatRig;
    use crate::catrig::mock_rig;
    use crate::mock::MockChannel;

    #[test]
    fn actor_request_response() {
        let (h, join) = spawn(Box::new(mock_rig()), true);
        h.set_freq(Vfo::A, 10_136_000).unwrap();
        assert_eq!(h.get_freq(Vfo::A).unwrap(), 10_136_000);
        h.set_mode(Mode::Cw).unwrap();
        assert_eq!(h.get_mode().unwrap(), Mode::Cw);
        let st = h.get_state().unwrap();
        assert_eq!(st.freq_hz, 10_136_000);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn actor_raw_passthrough() {
        let (h, join) = spawn(Box::new(mock_rig()), true);
        assert_eq!(h.raw("ID").unwrap().as_deref(), Some("ID021"));
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn transmit_is_hard_blocked_without_permission() {
        let (h, join) = spawn(Box::new(mock_rig()), false);
        // Both the typed PTT path and a raw TX command are refused...
        assert!(matches!(h.set_ptt(true), Err(RigError::TransmitBlocked)));
        assert!(matches!(h.raw("TX"), Err(RigError::TransmitBlocked)));
        assert!(matches!(h.raw("TX1"), Err(RigError::TransmitBlocked)));
        // ...while the radio stays in RX and non-TX commands still work.
        h.set_freq(Vfo::A, 14_074_000).unwrap();
        assert!(!h.get_state().unwrap().tx);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn transmit_allowed_with_permission() {
        let (h, join) = spawn(Box::new(mock_rig()), true);
        h.set_ptt(true).unwrap();
        assert!(h.get_state().unwrap().tx);
        h.set_ptt(false).unwrap();
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn actor_publishes_events_to_subscriber() {
        // Seed the mock with a queued unsolicited message, then confirm a
        // subscriber receives it once auto-info polling kicks in.
        let mut chan = MockChannel::default();
        chan.push_event("FA00014074000");
        let rig: CatRig<MockChannel> = CatRig::new(chan);
        let (h, join) = spawn(Box::new(rig), false);
        let events = h.subscribe().unwrap();
        let ev = events
            .recv_timeout(Duration::from_secs(2))
            .expect("should receive the queued event");
        assert_eq!(ev.raw, "FA00014074000");
        h.shutdown();
        join.join().unwrap();
    }
}
