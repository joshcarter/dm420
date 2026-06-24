//! The QSO state machine — pure, synchronous, and content-driven.
//!
//! [`Engine::step`] takes one [`Event`] (an operator command, an inbound decode,
//! or a slot boundary) and returns a [`Step`]: the new [`QsoState`] to publish,
//! an optional transmission to make this slot, and an optional completed contact
//! to log. No I/O, no clock, no bus — the async shell ([`crate::spawn`]) feeds it
//! events and acts on the outputs. That keeps the whole sequencer unit-testable.
//!
//! State is **re-derived from received content**, not an internal step counter
//! (WSJT-X interop rule #4 — `docs/wsjtx_qso_sequencing.md` §7). The two role
//! flows and the Field Day variant (grid step skipped, `RR73`/`73` roles and the
//! logging trigger reversed) follow `docs/qso_flow.md` §3 and §7.
//!
//! Not yet covered (deferred, see `docs/qso_flow.md` §6/§8): multi-caller
//! selection + number-key override, dupe/peer exclusion (needs logbook
//! enrichment + gossip), tail-ending, compound/hashed callsigns, and AP decoding.

use types::{
    Callsign, Decode, DecodeContent, DecodeRef, ExchangePayload, GridSquare, OffsetHz,
    OutgoingMessage, ParsedMessage, QsoCommand, QsoPhase, QsoState, RadioId, Section, Selection,
    Signoff, SlotId,
};

use crate::message::{self, StationConfig};

/// An input to the state machine.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Operator command from `qso/{id}/command`.
    Command(QsoCommand),
    /// The current selection from `selection/{id}/active` (carries our outgoing
    /// offset; the target is conveyed separately by [`QsoCommand::Start`]).
    Select(Selection),
    /// An inbound decode from `radio/{id}/decodes`. The engine filters relevance.
    Decode(Decode),
    /// A T/R slot boundary. `slot` parity (`slot.0 % 2`) decides which slots are
    /// ours to transmit in — we answer in the opposite slot from the station we
    /// heard.
    Tick { slot: SlotId },
}

/// What the engine wants done as a result of one [`Event`].
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The new state to publish on `qso/{id}/state`.
    pub state: QsoState,
    /// Transmit this on the air *this* slot (the shell gates on `allow_transmit`).
    pub tx: Option<TxIntent>,
    /// A completed contact to log.
    pub log: Option<CompletedQso>,
}

/// A message the engine wants transmitted this slot.
#[derive(Clone, Debug, PartialEq)]
pub struct TxIntent {
    pub offset: OffsetHz,
    pub slot: SlotId,
    pub message: OutgoingMessage,
}

/// A finished contact, for the logbook. Band/freq/time are stamped by the shell
/// from the rig + clock; the engine owns only the on-air facts.
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedQso {
    pub call: Callsign,
    pub grid: Option<GridSquare>,
    /// The partner's ARRL/RAC section, from a Field Day exchange. Carried for the
    /// map: a Field Day responder sends only a section, never a grid.
    pub section: Option<Section>,
    pub exchange_sent: String,
    pub exchange_rcvd: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// We called CQ; the partner answered us.
    CallingCq,
    /// We answered the partner's CQ.
    Answering,
}

/// What to do once the engine finishes its current outgoing message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Finish {
    /// Answering side: drop to idle after the final message.
    Idle,
    /// CQ side: go back to calling CQ on the same offset.
    ResumeCq,
}

/// Top-level engine state.
enum State {
    Idle,
    /// DM420's wait-for-CQ: armed to a target, receive-only until it calls CQ.
    /// (We snap our Tx offset to the target's own offset on commit, so the
    /// pre-commit offset isn't retained here.)
    Armed {
        target: DecodeRef,
    },
    /// Calling CQ; no committed partner yet. `tx_parity` is adopted on the first
    /// slot tick so we transmit every other slot. `cq_count` is the number of CQs
    /// sent at the current `offset` with no reply — drives auto-QSY.
    Calling {
        offset: OffsetHz,
        tx_parity: Option<u8>,
        cq_count: u8,
    },
    /// A committed contact in progress (boxed — much larger than the other
    /// variants).
    Active(Box<Active>),
}

/// A committed contact. `next` is the message we repeat each TX slot until
/// received content advances it (the repeat policy — `docs/qso_flow.md` §7).
struct Active {
    role: Role,
    partner: Callsign,
    /// Kept for re-arming if we lose the race (answering side).
    target: Option<DecodeRef>,
    offset: OffsetHz,
    tx_parity: u8,
    next: Option<OutgoingMessage>,
    /// Apply this transition after the next transmission goes out.
    finish_after_tx: Option<Finish>,
    /// When set, emit the completed-QSO log the next time we transmit `next`
    /// (the "log on `RR73` *sent*" trigger). Cleared after it fires once.
    log_on_tx: bool,
    logged: bool,
    /// Display-only over counter for `QsoPhase::InExchange { step }`.
    step: u8,
    // --- captured facts for the log ---
    partner_grid: Option<GridSquare>,
    /// Our report of the partner's signal (Standard), used for the report we send.
    partner_snr: i8,
    /// Their report of us (Standard).
    rcvd_report: Option<i8>,
    /// Their Field Day exchange.
    rcvd_fd: Option<(String, Section)>,
    /// TX slots we've sent the current `next` with no received content advancing
    /// the contact. Reset to 0 on every advance; when it reaches the give-up cap
    /// (`TX_CAP_DEFAULT`, or `TX_CAP_AFTER_LOG` once logged) we stop and fall back
    /// instead of repeating forever (P1 — `docs/qso_engine_improvements.md`).
    overs_since_progress: u8,
}

/// Unanswered CQs at one offset before auto-QSY moves us to a clearer one. After
/// this many CQs with no reply, the next CQ goes out on `Engine::next_cq_offset`.
const CQ_HOP_AFTER: u8 = 3;

/// Consecutive no-progress overs we send the *same* in-exchange message before
/// giving up on a committed contact (P1). One over ≈ two T/R slots (~30 s FT8 /
/// ~15 s FT4); the counter resets on any received content that advances the QSO,
/// so this is a run of *unanswered* overs. 3 trades a marginal late-completing
/// contact for getting back on CQ — the right call for Field Day rate.
const TX_CAP_DEFAULT: u8 = 3;

/// Tighter cap once the contact is already logged — the Standard CQ-side terminal
/// `RR73`, re-sent while waiting for a courtesy `73` that usually never comes. The
/// QSO already counts for us, so extra `RR73`s only help *them* log us: send a
/// couple for insurance, then resume CQ.
const TX_CAP_AFTER_LOG: u8 = 2;

/// The QSO engine for one radio.
pub struct Engine {
    radio: RadioId,
    me: StationConfig,
    /// Latest outgoing offset from the selection (where a CQ would transmit).
    outgoing: OffsetHz,
    state: State,
    /// Auto-QSY: after `CQ_HOP_AFTER` unanswered CQs, move to `next_cq_offset`
    /// before the next CQ. Toggled from the UI via `QsoControl`.
    auto_hop: bool,
    /// The clearest CQ offset the UI's lane finder last suggested — the hop target.
    next_cq_offset: Option<OffsetHz>,
    /// One-shot: set to the partner we just gave up on, so the give-up slot
    /// publishes `QsoPhase::TimedOut` once (then `step` clears it and we're already
    /// back in `Calling`/`Idle`). Only set on a genuine timeout, not a clean finish.
    timed_out: Option<Callsign>,
    /// TX offset frozen by the operator. The engine is the **sole owner** of the
    /// offset; this is the **one place** the lock is enforced — while set, the engine
    /// ignores every offset write (operator [`Event::Select`] / [`QsoCommand::SetTxOffset`])
    /// *and* its own auto-QSY hop, so a locked offset never moves. "Freeze everything."
    offset_locked: bool,
}

impl Engine {
    /// Create an idle engine. `outgoing` is the initial Tx offset until a
    /// [`Selection`] updates it.
    pub fn new(radio: RadioId, me: StationConfig, outgoing: OffsetHz) -> Self {
        Self {
            radio,
            me,
            outgoing,
            state: State::Idle,
            auto_hop: false,
            next_cq_offset: None,
            timed_out: None,
            offset_locked: false,
        }
    }

    /// Replace the station identity / contest profile (live reconfig from the UI).
    pub fn set_station(&mut self, me: StationConfig) {
        self.me = me;
    }

    /// Enable/disable auto-QSY after unanswered CQs (the UI's AUTO QSY toggle).
    pub fn set_auto_hop(&mut self, on: bool) {
        self.auto_hop = on;
    }

    /// Update the hop target — the lane finder's current best CQ offset.
    pub fn set_next_cq_offset(&mut self, offset: Option<OffsetHz>) {
        self.next_cq_offset = offset;
    }

    /// The current published state, without stepping (for an initial snapshot).
    pub fn state(&self) -> QsoState {
        self.snapshot()
    }

    /// Advance the machine by one event.
    pub fn step(&mut self, event: Event) -> Step {
        let mut tx = None;
        let mut log = None;
        match event {
            Event::Command(cmd) => log = self.on_command(cmd),
            // The Selection topic is also an offset write, so it obeys the same lock:
            // the engine is the sole enforcer, so no offset path (selection gesture or
            // command) can move a frozen offset.
            Event::Select(sel) => {
                if !self.offset_locked {
                    self.outgoing = sel.outgoing;
                }
            }
            Event::Decode(d) => log = self.on_decode(d),
            Event::Tick { slot } => {
                let out = self.on_tick(slot);
                tx = out.0;
                log = out.1;
            }
        }
        let state = self.snapshot();
        // The `TimedOut` phase is a one-shot: it's been published in `state`, so
        // clear it — the next snapshot shows the real fall-back state we're now in.
        self.timed_out = None;
        Step { state, tx, log }
    }

    // ----------------------------------------------------------------- commands

    /// Apply an operator command. Returns a completed-QSO log only for
    /// [`QsoCommand::Resume`], whose clicked line may itself be a logging trigger
    /// (their `RR73`); the other commands never log.
    fn on_command(&mut self, cmd: QsoCommand) -> Option<CompletedQso> {
        match cmd {
            QsoCommand::CallCq => {
                tracing::info!(offset = ?self.outgoing, "qso engine: calling CQ");
                self.state = State::Calling {
                    offset: self.outgoing,
                    tx_parity: None,
                    cq_count: 0,
                };
                None
            }
            QsoCommand::Start { target } => {
                tracing::info!(
                    target = ?target.call,
                    "qso engine: armed — will answer when the target next calls CQ"
                );
                self.state = State::Armed { target };
                None
            }
            QsoCommand::Resume {
                target,
                message,
                snr,
                offset,
            } => self.resume_from(target, message, snr, offset),
            QsoCommand::Abort => {
                tracing::info!("qso engine: abort → idle");
                self.state = State::Idle;
                None
            }
            // The engine owns the TX offset; this is the one place the lock is
            // enforced for an operator write. While locked we ignore it entirely
            // ("freeze everything") — operator-facing behaviour is identical to a
            // click while locked today: nothing moves. When unlocked, update
            // `outgoing` (where the next CQ/answer transmits) and, if we're already
            // calling CQ, the live `Calling.offset` too, so the move takes effect on
            // the very next CQ rather than waiting for a re-arm.
            QsoCommand::SetTxOffset(hz) => {
                if !self.offset_locked {
                    self.outgoing = hz;
                    if let State::Calling { offset, .. } = &mut self.state {
                        *offset = hz;
                    }
                }
                None
            }
            QsoCommand::SetOffsetLock(locked) => {
                self.offset_locked = locked;
                None
            }
        }
    }

    // ------------------------------------------------------------------ decodes

    /// Returns a completed-QSO log if this decode triggers a "log on receive".
    fn on_decode(&mut self, d: Decode) -> Option<CompletedQso> {
        let DecodeContent::Slotted { slot, message, .. } = d.content else {
            return None; // streaming (PSK31/RTTY) — not sequenced in v1
        };
        let snr = d.snr_db.unwrap_or(0);
        // Read the discriminant (copying out what each arm needs) so the borrow
        // of `self.state` ends before we call the `&mut self` handlers.
        enum Dispatch {
            Armed,
            Calling(OffsetHz, Option<u8>),
            Active,
            Idle,
        }
        let dispatch = match &self.state {
            State::Armed { .. } => Dispatch::Armed,
            State::Calling { offset, tx_parity, .. } => Dispatch::Calling(*offset, *tx_parity),
            State::Active(_) => Dispatch::Active,
            State::Idle => Dispatch::Idle,
        };
        match dispatch {
            Dispatch::Armed => {
                self.commit_from_armed(&message, &d.offset, slot, snr);
                None
            }
            Dispatch::Calling(offset, parity) => self.commit_from_cq(&message, snr, offset, parity),
            Dispatch::Active => self.advance_active(&message, snr),
            Dispatch::Idle => None,
        }
    }

    /// Armed → answering: commit when the target calls CQ.
    fn commit_from_armed(&mut self, msg: &ParsedMessage, _their_offset: &OffsetHz, slot: SlotId, snr: i8) {
        let State::Armed { target, .. } = &self.state else {
            return;
        };
        let target = target.clone();
        match msg {
            // The target called CQ — answer in the opposite slot at our chosen TX
            // offset (self.outgoing), which the UI set via the Selection topic when
            // the operator armed. Using the target's decoded offset here would
            // ignore a locked audio offset and transmit on the wrong frequency.
            ParsedMessage::Cq { caller, grid, .. } if Some(caller) == target.call.as_ref() => {
                tracing::info!(target = ?caller, tx_offset = ?self.outgoing, "qso engine: target called CQ → answering");
                let opener = self.opener(caller);
                self.state = State::Active(Box::new(Active {
                    role: Role::Answering,
                    partner: caller.clone(),
                    target: Some(target),
                    offset: self.outgoing,
                    tx_parity: parity_after(slot),
                    next: Some(opener),
                    finish_after_tx: None,
                    log_on_tx: false,
                    logged: false,
                    step: 1,
                    partner_grid: grid.clone(),
                    partner_snr: snr,
                    rcvd_report: None,
                    rcvd_fd: None,
                    overs_since_progress: 0,
                }));
            }
            // The target answered someone else — we lost the race; stay armed and
            // wait for its next CQ (we never transmitted, so nothing to stop).
            _ => {}
        }
    }

    /// Calling CQ → CQ side: commit when a station answers our CQ.
    fn commit_from_cq(
        &mut self,
        msg: &ParsedMessage,
        snr: i8,
        offset: OffsetHz,
        tx_parity: Option<u8>,
    ) -> Option<CompletedQso> {
        let parity = tx_parity.unwrap_or(0);
        match msg {
            // Standard: a caller answered with their grid (Tx1).
            ParsedMessage::Exchange {
                to,
                from,
                payload: ExchangePayload::Grid(grid),
            } if to == &self.me.call && !self.me.is_field_day() => {
                let reply = message::report(&self.me, from, snr);
                self.state = State::Active(Box::new(Active {
                    role: Role::CallingCq,
                    partner: from.clone(),
                    target: None,
                    offset,
                    tx_parity: parity,
                    next: Some(reply),
                    finish_after_tx: None,
                    log_on_tx: false,
                    logged: false,
                    step: 1,
                    partner_grid: Some(grid.clone()),
                    partner_snr: snr,
                    rcvd_report: None,
                    rcvd_fd: None,
                    overs_since_progress: 0,
                }));
                None
            }
            // Standard (P3): a caller skipped the grid and answered with a bare
            // signal report (the Tx2-style "skip-Tx1" opening, common in pile-ups
            // and POTA). Roger it and send our report (Tx3) — WSJT-X's jump-ahead.
            // We complete when they roger us (their `RR73`, via `advance_active`).
            // Field Day stays exclusive (no report openers — see A3 of the doc).
            ParsedMessage::Exchange {
                to,
                from,
                payload: ExchangePayload::Report(r),
            } if to == &self.me.call && !self.me.is_field_day() => {
                let reply = message::roger_report(&self.me, from, snr);
                self.state = State::Active(Box::new(Active {
                    role: Role::CallingCq,
                    partner: from.clone(),
                    target: None,
                    offset,
                    tx_parity: parity,
                    next: Some(reply),
                    finish_after_tx: None,
                    log_on_tx: false,
                    logged: false,
                    step: 2,
                    partner_grid: None,
                    partner_snr: snr,
                    rcvd_report: Some(*r),
                    rcvd_fd: None,
                    overs_since_progress: 0,
                }));
                None
            }
            // Field Day: a caller answered with their bare exchange (Tx2). We
            // reply with the combined roger+exchange (Tx3).
            ParsedMessage::Exchange {
                to,
                from,
                payload:
                    ExchangePayload::FieldDay {
                        class,
                        section,
                        rogered: false,
                    },
            } if to == &self.me.call && self.me.is_field_day() => {
                let reply = message::fd_roger_exchange(&self.me, from);
                self.state = State::Active(Box::new(Active {
                    role: Role::CallingCq,
                    partner: from.clone(),
                    target: None,
                    offset,
                    tx_parity: parity,
                    next: Some(reply),
                    finish_after_tx: None,
                    log_on_tx: false,
                    logged: false,
                    step: 1,
                    partner_grid: None,
                    partner_snr: snr,
                    rcvd_report: None,
                    rcvd_fd: Some((class.clone(), section.clone())),
                    overs_since_progress: 0,
                }));
                None
            }
            _ => None,
        }
    }

    /// Pick up a contact mid-stream from a decode the operator clicked, when the
    /// engine wasn't armed for it — e.g. we armed, the target didn't answer, we
    /// disarmed to look elsewhere, and *then* it answered our earlier call. Unlike
    /// [`Self::commit_from_armed`] (which waits for the target's next CQ), this
    /// commits at once, deriving our role and reply from the clicked line's
    /// content. Returns a log if that line is itself a logging trigger (their
    /// `RR73`).
    ///
    /// Only a line addressed *to us* resumes a contact: a `Cq` is the armed path
    /// ([`QsoCommand::Start`]), and free/unaddressed text carries no contact. A
    /// payload that doesn't fit the current contest mode, or a bare `73` (nothing
    /// left to send), is ignored.
    fn resume_from(
        &mut self,
        target: DecodeRef,
        msg: ParsedMessage,
        snr: i8,
        offset: OffsetHz,
    ) -> Option<CompletedQso> {
        let Some((to, from)) = addressed(&msg) else {
            tracing::info!("qso engine: resume ignored — not a directed message");
            return None;
        };
        if to != &self.me.call {
            tracing::info!(?to, "qso engine: resume ignored — not addressed to us");
            return None;
        }
        let from = from.clone();
        let me_fd = self.me.is_field_day();

        // Infer our role from who is answering whom — the same content→role mapping
        // the live commit paths use. Standard and Field Day reverse the side that
        // receives `RR73` (CQ side in FD, answering side in Standard).
        let role = match (&msg, me_fd) {
            (
                ParsedMessage::Exchange {
                    payload: ExchangePayload::Grid(_) | ExchangePayload::RogerReport(_),
                    ..
                },
                false,
            )
            | (
                ParsedMessage::Exchange {
                    payload: ExchangePayload::FieldDay { rogered: false, .. },
                    ..
                },
                true,
            ) => Role::CallingCq,
            (
                ParsedMessage::Exchange {
                    payload: ExchangePayload::Report(_),
                    ..
                },
                false,
            )
            | (
                ParsedMessage::Exchange {
                    payload: ExchangePayload::FieldDay { rogered: true, .. },
                    ..
                },
                true,
            ) => Role::Answering,
            (ParsedMessage::Signoff { kind, .. }, false) if is_roger(*kind) => Role::Answering,
            (ParsedMessage::Signoff { kind, .. }, true) if is_roger(*kind) => Role::CallingCq,
            _ => {
                tracing::info!("qso engine: resume ignored — nothing to send from this line");
                return None;
            }
        };

        tracing::info!(partner = ?from, ?role, "qso engine: resume — picking up mid-contact");

        // A provisional contact; `next`, the logging trigger, and the captured
        // exchange facts are filled by the same content-driven transitions the live
        // paths use (the openers inline below, everything else via `advance_active`).
        // We never saw the earlier overs, so the log can only carry what's on this
        // line plus our own report — partial, but truthful for a late pick-up.
        self.state = State::Active(Box::new(Active {
            role,
            partner: from.clone(),
            target: Some(target.clone()),
            offset,
            tx_parity: parity_after(target.slot),
            next: None,
            finish_after_tx: None,
            log_on_tx: false,
            logged: false,
            step: 0,
            partner_grid: None,
            partner_snr: snr,
            rcvd_report: None,
            rcvd_fd: None,
            overs_since_progress: 0,
        }));

        match &msg {
            // Openers — a station answering our CQ. `advance_active` only handles
            // mid/late exchanges, so seed the reply (and captured fact) here.
            ParsedMessage::Exchange {
                payload: ExchangePayload::Grid(grid),
                ..
            } => {
                let reply = message::report(&self.me, &from, snr);
                if let State::Active(a) = &mut self.state {
                    a.partner_grid = Some(grid.clone());
                    a.next = Some(reply);
                    a.step = 1;
                }
                None
            }
            ParsedMessage::Exchange {
                payload:
                    ExchangePayload::FieldDay {
                        class,
                        section,
                        rogered: false,
                    },
                ..
            } => {
                let reply = message::fd_roger_exchange(&self.me, &from);
                if let State::Active(a) = &mut self.state {
                    a.rcvd_fd = Some((class.clone(), section.clone()));
                    a.next = Some(reply);
                    a.step = 1;
                }
                None
            }
            // Report / R-report / sign-off: reuse the in-QSO transition, which sets
            // `next`, the logging trigger, and returns any log (their `RR73`).
            _ => self.advance_active(&msg, snr),
        }
    }

    /// Advance a committed contact from received content.
    fn advance_active(&mut self, msg: &ParsedMessage, _snr: i8) -> Option<CompletedQso> {
        // Pull what we need without holding a borrow across `&mut self` calls.
        let (role, partner, partner_snr, logged) = match &self.state {
            State::Active(a) => (a.role, a.partner.clone(), a.partner_snr, a.logged),
            _ => return None,
        };

        // Auto-stop / lost-race: the partner is now addressing a different call.
        match addressed(msg) {
            Some((to, from)) => {
                if from == &partner && to != &self.me.call {
                    return self.abandon();
                }
                if from != &partner || to != &self.me.call {
                    return None; // not part of our QSO
                }
            }
            None => return None, // a CQ or unaddressed line — ignore mid-QSO
        }

        let me_fd = self.me.is_field_day();
        match (role, me_fd, msg) {
            // ---------- Standard, answering side: their report → roger+report ----------
            (
                Role::Answering,
                false,
                ParsedMessage::Exchange {
                    payload: ExchangePayload::Report(r),
                    ..
                },
            ) => {
                let reply = message::roger_report(&self.me, &partner, partner_snr);
                if let State::Active(a) = &mut self.state {
                    a.rcvd_report = Some(*r);
                    a.next = Some(reply);
                    a.step = 2;
                    a.overs_since_progress = 0;
                }
                None
            }

            // ---------- Field Day, answering side: their R+exchange → RR73 ----------
            (
                Role::Answering,
                true,
                ParsedMessage::Exchange {
                    payload:
                        ExchangePayload::FieldDay {
                            class,
                            section,
                            rogered: true,
                        },
                    ..
                },
            ) => {
                // Their R+exchange — send RR73 and log when it goes out (RR73 sent).
                let rr = message::rr73(&self.me, &partner);
                if let State::Active(a) = &mut self.state {
                    a.rcvd_fd = Some((class.clone(), section.clone()));
                    a.next = Some(rr);
                    a.finish_after_tx = Some(Finish::Idle);
                    a.log_on_tx = true;
                    a.step = 2;
                    a.overs_since_progress = 0;
                }
                None
            }

            // ---------- Standard, CQ side: their R-report → RR73 (log on send) ----------
            (
                Role::CallingCq,
                false,
                ParsedMessage::Exchange {
                    payload: ExchangePayload::RogerReport(r),
                    ..
                },
            ) => {
                let rr = message::rr73(&self.me, &partner);
                if let State::Active(a) = &mut self.state {
                    a.rcvd_report = Some(*r);
                    a.next = Some(rr);
                    a.log_on_tx = true;
                    a.step = 2;
                    a.overs_since_progress = 0;
                }
                None
            }

            // ---------- Any directed sign-off (RRR / RR73 / 73) completes it ----------
            // P2: a partner who sends us *any* sign-off is done. Bare `73` is the most
            // common ending on the air, and RRR/RR73/73 are interchangeable here —
            // gating on one specific token is what used to leave us repeating an over
            // forever. The answering side (and the FD CQ side, on their RR73) sends a
            // courtesy `73`; the Standard CQ side has already logged on RR73-sent and
            // just resumes CQ.
            (_, _, ParsedMessage::Signoff { kind, .. }) => {
                let done = (!logged).then(|| self.completed());
                let courtesy = matches!(role, Role::Answering) || (me_fd && is_roger(*kind));
                if courtesy {
                    let s73 = message::seven3(&self.me, &partner);
                    let resume = matches!(role, Role::CallingCq);
                    if let State::Active(a) = &mut self.state {
                        a.next = Some(s73);
                        a.finish_after_tx =
                            Some(if resume { Finish::ResumeCq } else { Finish::Idle });
                        a.logged = true;
                        a.step = 3;
                        a.overs_since_progress = 0;
                    }
                } else {
                    self.resume_cq();
                }
                done
            }

            _ => None,
        }
    }

    /// Lost the race mid-QSO: stop, and re-arm (answering) or resume CQ.
    fn abandon(&mut self) -> Option<CompletedQso> {
        if let State::Active(a) = &self.state {
            match (a.role, a.target.clone()) {
                (Role::Answering, Some(target)) => {
                    self.state = State::Armed { target };
                }
                _ => self.resume_cq(),
            }
        }
        None
    }

    fn resume_cq(&mut self) {
        if let State::Active(a) = &self.state {
            self.state = State::Calling {
                offset: a.offset,
                tx_parity: Some(a.tx_parity),
                cq_count: 0,
            };
        } else {
            self.state = State::Calling {
                offset: self.outgoing,
                tx_parity: None,
                cq_count: 0,
            };
        }
    }

    // -------------------------------------------------------------------- ticks

    /// A slot boundary: transmit if this is our slot. Returns `(tx, log)`.
    fn on_tick(&mut self, slot: SlotId) -> (Option<TxIntent>, Option<CompletedQso>) {
        let parity = (slot.0 % 2) as u8;
        match &self.state {
            State::Calling { .. } => self.tick_calling(slot, parity),
            State::Active(_) => self.tick_active(slot, parity),
            _ => (None, None),
        }
    }

    /// CQ slot: adopt our parity on the first tick, then call CQ every TX slot.
    /// With auto-QSY on, move to the best lane before the 4th unanswered CQ — being
    /// still in `Calling` here *is* "no response," since a reply would have already
    /// committed us to `Active` via `on_decode`, so the existing sequencing gives us
    /// the "wait for the response decode" behavior for free.
    fn tick_calling(
        &mut self,
        slot: SlotId,
        parity: u8,
    ) -> (Option<TxIntent>, Option<CompletedQso>) {
        // A locked offset freezes everything — the engine's own auto-QSY must not
        // hop either (the headline fix: a locked auto-QSY hop used to transmit on a
        // frequency the operator believed was frozen).
        let auto_hop = self.auto_hop && !self.offset_locked;
        let next = self.next_cq_offset;
        let mut hopped_to = None;
        let offset = match &mut self.state {
            State::Calling {
                offset,
                tx_parity,
                cq_count,
            } => {
                if *tx_parity.get_or_insert(parity) != parity {
                    return (None, None);
                }
                // After CQ_HOP_AFTER unanswered CQs, QSY to the suggested lane and
                // start the count over there. If no lane was suggested, stay put.
                if auto_hop && *cq_count >= CQ_HOP_AFTER {
                    if let Some(new) = next {
                        *offset = new;
                        hopped_to = Some(new);
                    }
                    *cq_count = 0;
                }
                *cq_count += 1;
                *offset
            }
            _ => return (None, None),
        };
        if let Some(new) = hopped_to {
            // Keep the click/preview offset in step with where we now transmit.
            self.outgoing = new;
            tracing::info!(offset = ?new, "qso engine: auto-QSY to clearer CQ lane");
        }
        let message = message::cq(&self.me);
        (
            Some(TxIntent {
                offset,
                slot,
                message,
            }),
            None,
        )
    }

    /// Exchange slot: transmit the queued message, firing the "log on send"
    /// trigger and any queued end-of-QSO transition.
    fn tick_active(
        &mut self,
        slot: SlotId,
        parity: u8,
    ) -> (Option<TxIntent>, Option<CompletedQso>) {
        // Decide the slot's action without holding a borrow across `&mut self`.
        enum Act {
            Send,
            GiveUp {
                logged: bool,
                role: Role,
                partner: Callsign,
            },
        }
        let act = match &self.state {
            State::Active(a) if parity == a.tx_parity && a.next.is_some() => {
                let cap = if a.logged {
                    TX_CAP_AFTER_LOG
                } else {
                    TX_CAP_DEFAULT
                };
                if a.overs_since_progress >= cap {
                    Act::GiveUp {
                        logged: a.logged,
                        role: a.role,
                        partner: a.partner.clone(),
                    }
                } else {
                    Act::Send
                }
            }
            _ => return (None, None), // not our slot, or nothing queued
        };

        match act {
            // Out of patience: stop hammering and fall back. A still-unlogged contact
            // is a genuine timeout, surfaced once as `TimedOut`; an already-logged one
            // (the terminal `RR73`) just ends quietly. Either way we free the slot —
            // back to CQ if we were running, else idle.
            Act::GiveUp {
                logged,
                role,
                partner,
            } => {
                if logged {
                    tracing::info!(?partner, "qso engine: logged contact done — releasing the over");
                } else {
                    tracing::info!(?partner, cap = TX_CAP_DEFAULT, "qso engine: gave up — no progress");
                    self.timed_out = Some(partner);
                }
                match role {
                    Role::CallingCq => self.resume_cq(),
                    Role::Answering => self.state = State::Idle,
                }
                (None, None)
            }
            Act::Send => {
                // Snapshot what we need; the borrow ends before the `&mut self` calls.
                let (offset, message, do_log, finish) = match &self.state {
                    State::Active(a) => match &a.next {
                        Some(m) => (
                            a.offset,
                            m.clone(),
                            a.log_on_tx && !a.logged,
                            a.finish_after_tx,
                        ),
                        None => return (None, None),
                    },
                    _ => return (None, None),
                };
                // Count this over against the give-up cap; any advancing decode resets it.
                if let State::Active(a) = &mut self.state {
                    a.overs_since_progress = a.overs_since_progress.saturating_add(1);
                }
                let tx = TxIntent {
                    offset,
                    slot,
                    message,
                };
                // "Log on send" trigger (RR73 sent).
                let mut log = None;
                if do_log {
                    log = Some(self.completed());
                    if let State::Active(a) = &mut self.state {
                        a.logged = true;
                        a.log_on_tx = false;
                    }
                }
                // Apply any queued transition now that the message is on the air.
                if let Some(finish) = finish {
                    match finish {
                        Finish::Idle => self.state = State::Idle,
                        Finish::ResumeCq => self.resume_cq(),
                    }
                }
                (Some(tx), log)
            }
        }
    }

    // ----------------------------------------------------------------- helpers

    /// The opener we send when answering `his` CQ: the bare exchange in Field Day
    /// (grid skipped), the grid message (`Tx1`) in Standard.
    fn opener(&self, his: &Callsign) -> OutgoingMessage {
        if self.me.is_field_day() {
            message::fd_exchange(&self.me, his)
        } else {
            message::answer_grid(&self.me, his)
        }
    }

    /// Build the completed-QSO record from the active contact.
    fn completed(&self) -> CompletedQso {
        let State::Active(a) = &self.state else {
            // Only ever called while active.
            return CompletedQso {
                call: Callsign(String::new()),
                grid: None,
                section: None,
                exchange_sent: String::new(),
                exchange_rcvd: String::new(),
            };
        };
        let (sent, rcvd) = if self.me.is_field_day() {
            let sent = format!("{} {}", self.me.fd_class, self.me.fd_section.0);
            let rcvd = a
                .rcvd_fd
                .as_ref()
                .map(|(c, s)| format!("{c} {}", s.0))
                .unwrap_or_default();
            (sent, rcvd)
        } else {
            let sent = format!("{:+03}", a.partner_snr);
            let rcvd = a
                .rcvd_report
                .map(|r| format!("{r:+03}"))
                .unwrap_or_default();
            (sent, rcvd)
        };
        CompletedQso {
            call: a.partner.clone(),
            grid: a.partner_grid.clone(),
            section: a.rcvd_fd.as_ref().map(|(_, s)| s.clone()),
            exchange_sent: sent,
            exchange_rcvd: rcvd,
        }
    }

    /// Project the internal state onto the published [`QsoState`].
    fn snapshot(&self) -> QsoState {
        // One-shot: the slot we gave up on publishes `TimedOut` once (then `step`
        // clears the flag and the next snapshot shows the real fall-back state).
        if let Some(call) = &self.timed_out {
            return QsoState {
                radio: self.radio.clone(),
                phase: QsoPhase::TimedOut,
                partner: Some(call.clone()),
                next_tx: None,
                tx_offset: Some(self.tx_offset()),
                offset_locked: self.offset_locked,
            };
        }
        let (phase, partner, next_tx) = match &self.state {
            State::Idle => (QsoPhase::Idle, None, None),
            State::Armed { target, .. } => (
                QsoPhase::Armed,
                target.call.clone(),
                target.call.as_ref().map(|c| self.opener(c)),
            ),
            State::Calling { .. } => (QsoPhase::Calling, None, Some(message::cq(&self.me))),
            State::Active(a) => (
                QsoPhase::InExchange { step: a.step },
                Some(a.partner.clone()),
                a.next.clone(),
            ),
        };
        QsoState {
            radio: self.radio.clone(),
            phase,
            partner,
            next_tx,
            // Always `Some`: the engine owns the offset even when idle, so the UI can
            // render the TX lane before a QSO and track an auto-QSY hop it didn't set.
            tx_offset: Some(self.tx_offset()),
            offset_locked: self.offset_locked,
        }
    }

    /// The engine's current TX audio offset — where it is (or would be) transmitting.
    /// During a contact/CQ this is the live `Calling`/`Active` offset; otherwise the
    /// owned `outgoing` the next CQ/answer will use. The radio transmits here.
    fn tx_offset(&self) -> OffsetHz {
        match &self.state {
            State::Calling { offset, .. } => *offset,
            State::Active(a) => a.offset,
            _ => self.outgoing,
        }
    }
}

/// The slot we transmit in when answering a station heard in `slot`: the
/// opposite T/R slot.
fn parity_after(slot: SlotId) -> u8 {
    ((slot.0 + 1) % 2) as u8
}

/// `(to, from)` for a directed message; `None` for a CQ or unaddressed line.
fn addressed(msg: &ParsedMessage) -> Option<(&Callsign, &Callsign)> {
    match msg {
        ParsedMessage::Exchange { to, from, .. } => Some((to, from)),
        ParsedMessage::Signoff { to, from, .. } => Some((to, from)),
        _ => None,
    }
}

/// `RR73`/`RRR` — a roger that completes the report exchange. (Used to choose the
/// reply on the FD CQ side / the resume-role inference; P2 made *recognition* of a
/// sign-off token-agnostic, so any `Signoff` now completes a contact regardless.)
fn is_roger(kind: Signoff) -> bool {
    matches!(kind, Signoff::Rr73 | Signoff::Rrr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{ContestProfile, OverAirMode, SignalSource, Timestamp};

    const ME: &str = "W9XYZ";
    const HIM: &str = "K1ABC";

    fn me(contest: ContestProfile) -> StationConfig {
        StationConfig {
            call: Callsign(ME.into()),
            grid: GridSquare("EM48".into()),
            fd_class: "3A".into(),
            fd_section: Section("WI".into()),
            contest,
        }
    }

    fn engine(contest: ContestProfile) -> Engine {
        Engine::new(RadioId("rig0".into()), me(contest), OffsetHz(1500.0))
    }

    fn call(s: &str) -> Callsign {
        Callsign(s.into())
    }

    /// An inbound decode carrying `msg`, heard in `slot` at `snr` dB.
    fn decode(msg: ParsedMessage, slot: u64, snr: i8) -> Decode {
        Decode {
            radio: RadioId("rig0".into()),
            mode: OverAirMode::Ft8,
            t: Timestamp(0),
            offset: OffsetHz(1200.0),
            snr_db: Some(snr),
            source: SignalSource::Received,
            content: DecodeContent::Slotted {
                slot: SlotId(slot),
                dt: 0.0,
                message: msg,
                raw: String::new(),
            },
        }
    }

    fn cq_from(who: &str, fd: bool) -> ParsedMessage {
        ParsedMessage::Cq {
            caller: call(who),
            contest: fd.then_some(types::ContestTag::FieldDay),
            grid: Some(GridSquare("FN42".into())),
        }
    }

    fn exch(to: &str, from: &str, payload: ExchangePayload) -> ParsedMessage {
        ParsedMessage::Exchange {
            to: call(to),
            from: call(from),
            payload,
        }
    }

    fn signoff(to: &str, from: &str, kind: Signoff) -> ParsedMessage {
        ParsedMessage::Signoff {
            to: call(to),
            from: call(from),
            kind,
        }
    }

    fn start_target() -> QsoCommand {
        QsoCommand::Start {
            target: DecodeRef {
                radio: RadioId("rig0".into()),
                slot: SlotId(0),
                call: Some(call(HIM)),
            },
        }
    }

    /// The text the engine would transmit on a tick in `slot`, if any.
    fn tx_text(e: &mut Engine, slot: u64) -> Option<String> {
        e.step(Event::Tick { slot: SlotId(slot) })
            .tx
            .map(|t| t.message.text)
    }

    #[test]
    fn standard_answering_full_flow() {
        let mut e = engine(ContestProfile::Standard);
        // Arm to K1ABC, then hear its CQ (slot 4, snr -5 → our report of them).
        assert_eq!(
            e.step(Event::Command(start_target())).state.phase,
            QsoPhase::Armed
        );
        let s = e.step(Event::Decode(decode(cq_from(HIM, false), 4, -5)));
        assert!(matches!(s.state.phase, QsoPhase::InExchange { .. }));
        // We answer with our grid in the opposite slot (CQ in 4 → we TX odd).
        assert_eq!(tx_text(&mut e, 5).as_deref(), Some("K1ABC W9XYZ EM48"));
        // Their report → we roger with our report of them (-05).
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Report(-12)),
            6,
            -5,
        )));
        assert_eq!(tx_text(&mut e, 7).as_deref(), Some("K1ABC W9XYZ R-05"));
        // Their RR73 → log on receive, then a single 73, then idle.
        let s = e.step(Event::Decode(decode(
            signoff(ME, HIM, Signoff::Rr73),
            8,
            -5,
        )));
        let log = s.log.expect("log on RR73 received");
        assert_eq!(log.call, call(HIM));
        assert_eq!(log.exchange_sent, "-05");
        assert_eq!(log.exchange_rcvd, "-12");
        assert_eq!(tx_text(&mut e, 9).as_deref(), Some("K1ABC W9XYZ 73"));
        assert_eq!(e.state().phase, QsoPhase::Idle);
    }

    #[test]
    fn standard_calling_cq_full_flow() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        // Adopt parity on the first tick and call CQ.
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("CQ W9XYZ EM48"));
        // A caller answers with their grid → we send a report (our snr of them, -8).
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            3,
            -8,
        )));
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ -08"));
        // Their R-report → we send RR73 and log on *send*.
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::RogerReport(-3)),
            5,
            -8,
        )));
        let s = e.step(Event::Tick { slot: SlotId(6) });
        assert_eq!(s.tx.unwrap().message.text, "K1ABC W9XYZ RR73");
        let log = s.log.expect("log on RR73 sent");
        assert_eq!(log.exchange_sent, "-08");
        assert_eq!(log.exchange_rcvd, "-03");
        // Their 73 → resume CQ on the same offset.
        e.step(Event::Decode(decode(
            signoff(ME, HIM, Signoff::Seven3),
            7,
            -8,
        )));
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    #[test]
    fn calling_cq_caller_answers_with_report_not_grid() {
        // P3: a caller skips the grid and answers our CQ with a bare report. We must
        // not keep calling CQ — jump straight to the roger+report (Tx3).
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("CQ W9XYZ EM48"));
        let s = e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Report(0)),
            3,
            -8,
        )));
        assert!(matches!(s.state.phase, QsoPhase::InExchange { .. }));
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ R-08"));
        // Their RR73 closes it: log on receive (our report sent, their report rcvd).
        let s = e.step(Event::Decode(decode(signoff(ME, HIM, Signoff::Rr73), 5, -8)));
        let log = s.log.expect("log when they roger our report");
        assert_eq!(log.exchange_sent, "-08");
        assert_eq!(log.exchange_rcvd, "+00");
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    #[test]
    fn answering_completes_on_bare_73() {
        // P2: the partner closes with a *bare* 73 (not RR73) — the most common ending
        // on the air. We must still log and finish, not keep re-sending our R-report.
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(start_target()));
        e.step(Event::Decode(decode(cq_from(HIM, false), 4, -5)));
        assert_eq!(tx_text(&mut e, 5).as_deref(), Some("K1ABC W9XYZ EM48"));
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Report(-12)),
            6,
            -5,
        )));
        assert_eq!(tx_text(&mut e, 7).as_deref(), Some("K1ABC W9XYZ R-05"));
        let s = e.step(Event::Decode(decode(signoff(ME, HIM, Signoff::Seven3), 8, -5)));
        let log = s.log.expect("a bare 73 completes the QSO");
        assert_eq!(log.call, call(HIM));
        assert_eq!(tx_text(&mut e, 9).as_deref(), Some("K1ABC W9XYZ 73"));
        assert_eq!(e.state().phase, QsoPhase::Idle);
    }

    #[test]
    fn cq_side_completes_on_rrr() {
        // P2: on the CQ side the partner acks with RRR (not 73). The old `is_final`
        // gate excluded RRR and we'd hang; now any sign-off finishes the contact.
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("CQ W9XYZ EM48"));
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            3,
            -8,
        )));
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ -08"));
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::RogerReport(-3)),
            5,
            -8,
        )));
        let s = e.step(Event::Tick { slot: SlotId(6) });
        assert_eq!(s.tx.unwrap().message.text, "K1ABC W9XYZ RR73");
        s.log.expect("log on RR73 sent");
        e.step(Event::Decode(decode(signoff(ME, HIM, Signoff::Rrr), 7, -8)));
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    #[test]
    fn times_out_after_n_unanswered_overs() {
        // P1: a committed contact whose partner goes silent must give up after
        // TX_CAP_DEFAULT overs and fall back to CQ — not repeat the over forever.
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(tx_text(&mut e, 0).as_deref(), Some("CQ W9XYZ EM48"));
        // A caller answers with a grid; we start sending our report, then they vanish.
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            1,
            -8,
        )));
        // We re-send the report on each of our (even) slots, three times…
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("K1ABC W9XYZ -08"));
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ -08"));
        assert_eq!(tx_text(&mut e, 6).as_deref(), Some("K1ABC W9XYZ -08"));
        // …then give up: this slot publishes TimedOut and transmits nothing,
        let s = e.step(Event::Tick { slot: SlotId(8) });
        assert_eq!(s.tx, None);
        assert_eq!(s.state.phase, QsoPhase::TimedOut);
        assert_eq!(s.state.partner, Some(call(HIM)));
        // and we're back to calling CQ.
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    #[test]
    fn cq_side_releases_rr73_after_log() {
        // P1/B2: once we've logged on RR73-sent, a silent partner must not pin us on
        // RR73 forever — we send it TX_CAP_AFTER_LOG times then resume CQ.
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(tx_text(&mut e, 0).as_deref(), Some("CQ W9XYZ EM48"));
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            1,
            -8,
        )));
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("K1ABC W9XYZ -08"));
        e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::RogerReport(-3)),
            3,
            -8,
        )));
        // RR73 #1 (logs on send), then RR73 #2, then we release and resume CQ.
        let s = e.step(Event::Tick { slot: SlotId(4) });
        assert_eq!(s.tx.unwrap().message.text, "K1ABC W9XYZ RR73");
        s.log.expect("log on RR73 sent");
        assert_eq!(tx_text(&mut e, 6).as_deref(), Some("K1ABC W9XYZ RR73"));
        let s = e.step(Event::Tick { slot: SlotId(8) });
        assert_eq!(s.tx, None, "stop re-sending RR73 once logged");
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    /// The TX offset of the CQ the engine would send on a tick in `slot`.
    fn cq_offset(e: &mut Engine, slot: u64) -> OffsetHz {
        e.step(Event::Tick { slot: SlotId(slot) })
            .tx
            .expect("a CQ on our parity slot")
            .offset
    }

    #[test]
    fn auto_qsy_hops_after_three_unanswered_cqs() {
        let mut e = engine(ContestProfile::Standard);
        e.set_auto_hop(true);
        e.set_next_cq_offset(Some(OffsetHz(900.0)));
        e.step(Event::Command(QsoCommand::CallCq));
        // First three CQs stay on the original 1500 Hz (engine's `outgoing`).
        assert_eq!(cq_offset(&mut e, 0), OffsetHz(1500.0));
        assert_eq!(cq_offset(&mut e, 2), OffsetHz(1500.0));
        assert_eq!(cq_offset(&mut e, 4), OffsetHz(1500.0));
        // No answer → the fourth QSYs to the suggested lane, and counting restarts.
        assert_eq!(cq_offset(&mut e, 6), OffsetHz(900.0));
        assert_eq!(cq_offset(&mut e, 8), OffsetHz(900.0));
    }

    #[test]
    fn no_auto_qsy_when_disabled() {
        let mut e = engine(ContestProfile::Standard);
        // A hop target is offered, but the toggle is off, so it's ignored.
        e.set_next_cq_offset(Some(OffsetHz(900.0)));
        e.step(Event::Command(QsoCommand::CallCq));
        for slot in [0, 2, 4, 6, 8] {
            assert_eq!(cq_offset(&mut e, slot), OffsetHz(1500.0));
        }
    }

    #[test]
    fn no_auto_qsy_without_a_suggested_lane() {
        let mut e = engine(ContestProfile::Standard);
        e.set_auto_hop(true); // on, but no lane fed → stay put rather than hop nowhere
        e.step(Event::Command(QsoCommand::CallCq));
        for slot in [0, 2, 4, 6, 8] {
            assert_eq!(cq_offset(&mut e, slot), OffsetHz(1500.0));
        }
    }

    #[test]
    fn a_reply_cancels_the_pending_qsy() {
        let mut e = engine(ContestProfile::Standard);
        e.set_auto_hop(true);
        e.set_next_cq_offset(Some(OffsetHz(900.0)));
        e.step(Event::Command(QsoCommand::CallCq));
        // Three unanswered CQs (count now at the hop threshold).
        for slot in [0, 2, 4] {
            assert_eq!(cq_offset(&mut e, slot), OffsetHz(1500.0));
        }
        // A caller answers our CQ before the would-be fourth → commit, don't QSY.
        let s = e.step(Event::Decode(decode(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            5,
            -8,
        )));
        assert!(matches!(s.state.phase, QsoPhase::InExchange { .. }));
        // The next TX is the contact reply, not a CQ on the hop offset.
        let tx = e.step(Event::Tick { slot: SlotId(6) }).tx.unwrap();
        assert!(tx.message.text.contains(HIM));
        assert_ne!(tx.offset, OffsetHz(900.0));
    }

    // ------------------------------------------------- TX-offset ownership + lock

    /// The headline fix: while the offset is locked, auto-QSY must not hop — even
    /// after the threshold of unanswered CQs. A locked offset never moves, so the
    /// engine keeps transmitting where the operator froze it.
    #[test]
    fn locked_blocks_auto_qsy_after_unanswered_cqs() {
        let mut e = engine(ContestProfile::Standard);
        e.set_auto_hop(true);
        e.set_next_cq_offset(Some(OffsetHz(900.0)));
        e.step(Event::Command(QsoCommand::SetOffsetLock(true)));
        e.step(Event::Command(QsoCommand::CallCq));
        // Past the hop threshold (3) and beyond: every CQ stays on the frozen lane.
        for slot in [0, 2, 4, 6, 8, 10] {
            assert_eq!(cq_offset(&mut e, slot), OffsetHz(1500.0));
        }
        assert_eq!(e.state().tx_offset, Some(OffsetHz(1500.0)));
    }

    /// An operator offset write (`SetTxOffset`) is ignored while locked — identical
    /// to a click while locked today: nothing moves.
    #[test]
    fn locked_ignores_set_tx_offset() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::SetOffsetLock(true)));
        e.step(Event::Command(QsoCommand::SetTxOffset(OffsetHz(2000.0))));
        assert_eq!(e.state().tx_offset, Some(OffsetHz(1500.0)), "write ignored while locked");
        // And the next CQ still transmits on the frozen offset, not the rejected write.
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(cq_offset(&mut e, 0), OffsetHz(1500.0));
    }

    /// Unlocked, auto-QSY moves the offset after the threshold *and* the published
    /// `QsoState.tx_offset` reflects the new lane (so the UI's TX indicator tracks a
    /// hop the operator didn't set by hand).
    #[test]
    fn unlocked_auto_qsy_moves_and_state_reflects_it() {
        let mut e = engine(ContestProfile::Standard);
        e.set_auto_hop(true);
        e.set_next_cq_offset(Some(OffsetHz(900.0)));
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(cq_offset(&mut e, 0), OffsetHz(1500.0));
        assert_eq!(cq_offset(&mut e, 2), OffsetHz(1500.0));
        assert_eq!(cq_offset(&mut e, 4), OffsetHz(1500.0));
        // The 4th CQ hops; both the TX intent and the published state follow.
        let s = e.step(Event::Tick { slot: SlotId(6) });
        assert_eq!(s.tx.expect("a CQ").offset, OffsetHz(900.0));
        assert_eq!(s.state.tx_offset, Some(OffsetHz(900.0)), "QsoState reflects the hop");
    }

    /// Unlocked, `SetTxOffset(x)` sets `outgoing` so the next CQ transmits at `x`.
    #[test]
    fn unlocked_set_tx_offset_drives_next_cq() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::SetTxOffset(OffsetHz(2000.0))));
        assert_eq!(e.state().tx_offset, Some(OffsetHz(2000.0)));
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(cq_offset(&mut e, 0), OffsetHz(2000.0), "next CQ transmits at the set offset");
    }

    /// `tx_offset` is always `Some` — including while Idle and Armed — so the UI can
    /// render the TX lane before any QSO begins.
    #[test]
    fn tx_offset_is_some_while_idle_and_armed() {
        let mut e = engine(ContestProfile::Standard);
        // Idle: the owned `outgoing`.
        let idle = e.state();
        assert_eq!(idle.phase, QsoPhase::Idle);
        assert_eq!(idle.tx_offset, Some(OffsetHz(1500.0)));
        // Armed: still Some (the offset is owned even while waiting for a CQ).
        e.step(Event::Command(start_target()));
        let armed = e.state();
        assert_eq!(armed.phase, QsoPhase::Armed);
        assert_eq!(armed.tx_offset, Some(OffsetHz(1500.0)));
    }

    /// `SetTxOffset` while calling CQ moves the *live* `Calling.offset`, so the move
    /// takes effect on the very next CQ rather than waiting for a re-arm.
    #[test]
    fn set_tx_offset_while_calling_moves_live_offset() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(cq_offset(&mut e, 0), OffsetHz(1500.0));
        e.step(Event::Command(QsoCommand::SetTxOffset(OffsetHz(1800.0))));
        assert_eq!(cq_offset(&mut e, 2), OffsetHz(1800.0), "live Calling.offset moved");
        assert_eq!(e.state().tx_offset, Some(OffsetHz(1800.0)));
    }

    /// Unlocking re-enables movement: a write rejected while locked applies once the
    /// lock is released.
    #[test]
    fn unlock_re_enables_offset_writes() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::SetOffsetLock(true)));
        e.step(Event::Command(QsoCommand::SetTxOffset(OffsetHz(2200.0))));
        assert_eq!(e.state().tx_offset, Some(OffsetHz(1500.0)));
        e.step(Event::Command(QsoCommand::SetOffsetLock(false)));
        e.step(Event::Command(QsoCommand::SetTxOffset(OffsetHz(2200.0))));
        assert_eq!(e.state().tx_offset, Some(OffsetHz(2200.0)), "write applies after unlock");
    }

    #[test]
    fn field_day_answering_logs_on_rr73_sent() {
        let mut e = engine(ContestProfile::ArrlFieldDay);
        e.step(Event::Command(start_target()));
        // Hear CQ FD → open with the bare exchange (no grid).
        e.step(Event::Decode(decode(cq_from(HIM, true), 4, -5)));
        assert_eq!(tx_text(&mut e, 5).as_deref(), Some("K1ABC W9XYZ 3A WI"));
        // Their R+exchange → we queue RR73; log fires when RR73 is *sent*.
        let s = e.step(Event::Decode(decode(
            exch(
                ME,
                HIM,
                ExchangePayload::FieldDay {
                    class: "2B".into(),
                    section: Section("IL".into()),
                    rogered: true,
                },
            ),
            6,
            -5,
        )));
        assert!(s.log.is_none(), "FD answering must not log on receive");
        let s = e.step(Event::Tick { slot: SlotId(7) });
        assert_eq!(s.tx.unwrap().message.text, "K1ABC W9XYZ RR73");
        let log = s.log.expect("FD answering logs on RR73 sent");
        assert_eq!(log.exchange_sent, "3A WI");
        assert_eq!(log.exchange_rcvd, "2B IL");
        assert_eq!(e.state().phase, QsoPhase::Idle);
    }

    #[test]
    fn field_day_calling_cq_logs_on_rr73_received() {
        let mut e = engine(ContestProfile::ArrlFieldDay);
        e.step(Event::Command(QsoCommand::CallCq));
        assert_eq!(tx_text(&mut e, 2).as_deref(), Some("CQ FD W9XYZ EM48"));
        // A caller answers with their bare exchange → we send the combined R+exchange.
        e.step(Event::Decode(decode(
            exch(
                ME,
                HIM,
                ExchangePayload::FieldDay {
                    class: "2B".into(),
                    section: Section("IL".into()),
                    rogered: false,
                },
            ),
            3,
            -8,
        )));
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ R 3A WI"));
        // Their RR73 → log on receive, send a single 73, resume CQ.
        let s = e.step(Event::Decode(decode(
            signoff(ME, HIM, Signoff::Rr73),
            5,
            -8,
        )));
        let log = s.log.expect("FD CQ side logs on RR73 received");
        assert_eq!(log.exchange_sent, "3A WI");
        assert_eq!(log.exchange_rcvd, "2B IL");
        assert_eq!(tx_text(&mut e, 6).as_deref(), Some("K1ABC W9XYZ 73"));
        assert_eq!(e.state().phase, QsoPhase::Calling);
    }

    #[test]
    fn armed_stays_put_when_target_works_someone_else() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(start_target()));
        // Target answers W1AAA, not us → we keep waiting, no transmission.
        let s = e.step(Event::Decode(decode(
            exch(
                "W1AAA",
                HIM,
                ExchangePayload::Grid(GridSquare("FN31".into())),
            ),
            4,
            -5,
        )));
        assert_eq!(s.state.phase, QsoPhase::Armed);
        assert_eq!(tx_text(&mut e, 5), None);
    }

    #[test]
    fn lost_race_mid_qso_rearms() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(start_target()));
        e.step(Event::Decode(decode(cq_from(HIM, false), 4, -5)));
        // We committed to answering; now the partner addresses someone else.
        let s = e.step(Event::Decode(decode(
            exch(
                "W1AAA",
                HIM,
                ExchangePayload::Grid(GridSquare("FN31".into())),
            ),
            6,
            -5,
        )));
        assert_eq!(
            s.state.phase,
            QsoPhase::Armed,
            "auto-stop re-arms to the target"
        );
    }

    #[test]
    fn abort_returns_to_idle() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(QsoCommand::CallCq));
        let s = e.step(Event::Command(QsoCommand::Abort));
        assert_eq!(s.state.phase, QsoPhase::Idle);
    }

    #[test]
    fn we_only_transmit_in_our_slot() {
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(start_target()));
        e.step(Event::Decode(decode(cq_from(HIM, false), 4, -5))); // TX parity = odd
        assert_eq!(tx_text(&mut e, 6), None, "even slot is not ours");
        assert!(tx_text(&mut e, 7).is_some(), "odd slot is ours");
    }

    /// A `Resume` command carrying the line the operator clicked, heard in `slot`
    /// at `snr` dB (our report of them).
    fn resume_cmd(msg: ParsedMessage, slot: u64, snr: i8) -> QsoCommand {
        QsoCommand::Resume {
            target: DecodeRef {
                radio: RadioId("rig0".into()),
                slot: SlotId(slot),
                call: Some(call(HIM)),
            },
            message: msg,
            snr,
            offset: OffsetHz(1200.0),
        }
    }

    #[test]
    fn resume_standard_from_report_to_us() {
        // The reported scenario: we armed, disarmed, and *then* the station
        // answered our earlier grid. Clicking its report picks the contact up.
        let mut e = engine(ContestProfile::Standard);
        let s = e.step(Event::Command(resume_cmd(
            exch(ME, HIM, ExchangePayload::Report(-12)),
            6,
            -5,
        )));
        assert!(matches!(s.state.phase, QsoPhase::InExchange { .. }));
        assert_eq!(s.state.partner, Some(call(HIM)));
        // Report heard in slot 6 (even) → we roger in the odd slot.
        assert_eq!(tx_text(&mut e, 7).as_deref(), Some("K1ABC W9XYZ R-05"));
        // From here the normal answering flow takes over (their RR73 logs).
        let s = e.step(Event::Decode(decode(signoff(ME, HIM, Signoff::Rr73), 8, -5)));
        let log = s.log.expect("log on RR73 received");
        assert_eq!(log.call, call(HIM));
        assert_eq!(log.exchange_sent, "-05");
        assert_eq!(log.exchange_rcvd, "-12");
        assert_eq!(tx_text(&mut e, 9).as_deref(), Some("K1ABC W9XYZ 73"));
        assert_eq!(e.state().phase, QsoPhase::Idle);
    }

    #[test]
    fn resume_standard_from_grid_to_us() {
        // They answered our (earlier) CQ with a grid while we sat idle.
        let mut e = engine(ContestProfile::Standard);
        e.step(Event::Command(resume_cmd(
            exch(ME, HIM, ExchangePayload::Grid(GridSquare("FN42".into()))),
            3,
            -8,
        )));
        // Grid in slot 3 (odd) → we report in the even slot.
        assert_eq!(tx_text(&mut e, 4).as_deref(), Some("K1ABC W9XYZ -08"));
    }

    #[test]
    fn resume_field_day_from_roger_exchange() {
        let mut e = engine(ContestProfile::ArrlFieldDay);
        e.step(Event::Command(resume_cmd(
            exch(
                ME,
                HIM,
                ExchangePayload::FieldDay {
                    class: "2B".into(),
                    section: Section("IL".into()),
                    rogered: true,
                },
            ),
            6,
            -5,
        )));
        // Their R+exchange → we send RR73, logging on send (FD answering side).
        let s = e.step(Event::Tick { slot: SlotId(7) });
        assert_eq!(s.tx.unwrap().message.text, "K1ABC W9XYZ RR73");
        let log = s.log.expect("FD answering logs on RR73 sent");
        assert_eq!(log.exchange_sent, "3A WI");
        assert_eq!(log.exchange_rcvd, "2B IL");
        assert_eq!(e.state().phase, QsoPhase::Idle);
    }

    #[test]
    fn resume_ignores_cq_and_lines_to_others() {
        let mut e = engine(ContestProfile::Standard);
        // A CQ is the armed path, not a resume.
        let s = e.step(Event::Command(resume_cmd(cq_from(HIM, false), 4, -5)));
        assert_eq!(s.state.phase, QsoPhase::Idle);
        // A line answering someone else isn't ours to pick up.
        let s = e.step(Event::Command(resume_cmd(
            exch("N0ONE", HIM, ExchangePayload::Report(-3)),
            4,
            -5,
        )));
        assert_eq!(s.state.phase, QsoPhase::Idle);
    }
}
