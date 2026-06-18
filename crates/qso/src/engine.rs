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
    /// slot tick so we transmit every other slot.
    Calling {
        offset: OffsetHz,
        tx_parity: Option<u8>,
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
}

/// The QSO engine for one radio.
pub struct Engine {
    radio: RadioId,
    me: StationConfig,
    /// Latest outgoing offset from the selection (where a CQ would transmit).
    outgoing: OffsetHz,
    state: State,
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
        }
    }

    /// Replace the station identity / contest profile (live reconfig from the UI).
    pub fn set_station(&mut self, me: StationConfig) {
        self.me = me;
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
            Event::Command(cmd) => self.on_command(cmd),
            Event::Select(sel) => self.outgoing = sel.outgoing,
            Event::Decode(d) => log = self.on_decode(d),
            Event::Tick { slot } => {
                let out = self.on_tick(slot);
                tx = out.0;
                log = out.1;
            }
        }
        Step {
            state: self.snapshot(),
            tx,
            log,
        }
    }

    // ----------------------------------------------------------------- commands

    fn on_command(&mut self, cmd: QsoCommand) {
        match cmd {
            QsoCommand::CallCq => {
                tracing::info!(offset = ?self.outgoing, "qso engine: calling CQ");
                self.state = State::Calling {
                    offset: self.outgoing,
                    tx_parity: None,
                };
            }
            QsoCommand::Start { target } => {
                tracing::info!(
                    target = ?target.call,
                    "qso engine: armed — will answer when the target next calls CQ"
                );
                self.state = State::Armed { target };
            }
            QsoCommand::Abort => {
                tracing::info!("qso engine: abort → idle");
                self.state = State::Idle;
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
            State::Calling { offset, tx_parity } => Dispatch::Calling(*offset, *tx_parity),
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
    fn commit_from_armed(&mut self, msg: &ParsedMessage, offset: &OffsetHz, slot: SlotId, snr: i8) {
        let State::Armed { target, .. } = &self.state else {
            return;
        };
        let target = target.clone();
        match msg {
            // The target called CQ — snap to it and answer in the opposite slot.
            ParsedMessage::Cq { caller, grid, .. } if Some(caller) == target.call.as_ref() => {
                tracing::info!(target = ?caller, "qso engine: target called CQ → answering");
                let opener = self.opener(caller);
                self.state = State::Active(Box::new(Active {
                    role: Role::Answering,
                    partner: caller.clone(),
                    target: Some(target),
                    offset: *offset,
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
                }));
                None
            }
            _ => None,
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
            // ---------- Standard, answering side ----------
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
                }
                None
            }
            (Role::Answering, false, ParsedMessage::Signoff { kind, .. }) if is_roger(*kind) => {
                // Their RR73 — log now (RR73 received), then send a single 73.
                let done = self.completed();
                let s73 = message::seven3(&self.me, &partner);
                if let State::Active(a) = &mut self.state {
                    a.next = Some(s73);
                    a.finish_after_tx = Some(Finish::Idle);
                    a.logged = true;
                    a.step = 3;
                }
                Some(done)
            }

            // ---------- Field Day, answering side ----------
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
                }
                None
            }

            // ---------- Standard, CQ side ----------
            (
                Role::CallingCq,
                false,
                ParsedMessage::Exchange {
                    payload: ExchangePayload::RogerReport(r),
                    ..
                },
            ) => {
                // Their R-report — send RR73, logging when it goes out (RR73 sent).
                let rr = message::rr73(&self.me, &partner);
                if let State::Active(a) = &mut self.state {
                    a.rcvd_report = Some(*r);
                    a.next = Some(rr);
                    a.log_on_tx = true;
                    a.step = 2;
                }
                None
            }
            (Role::CallingCq, false, ParsedMessage::Signoff { kind, .. }) if is_final(*kind) => {
                // Their 73 — the QSO is done; resume CQ on the same offset.
                let done = (!logged).then(|| self.completed());
                self.resume_cq();
                done
            }

            // ---------- Field Day, CQ side ----------
            (Role::CallingCq, true, ParsedMessage::Signoff { kind, .. }) if is_roger(*kind) => {
                // Their RR73 — log now (RR73 received), send a single 73, resume CQ.
                let done = self.completed();
                let s73 = message::seven3(&self.me, &partner);
                if let State::Active(a) = &mut self.state {
                    a.next = Some(s73);
                    a.finish_after_tx = Some(Finish::ResumeCq);
                    a.logged = true;
                    a.step = 3;
                }
                Some(done)
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
            };
        } else {
            self.state = State::Calling {
                offset: self.outgoing,
                tx_parity: None,
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
    fn tick_calling(
        &mut self,
        slot: SlotId,
        parity: u8,
    ) -> (Option<TxIntent>, Option<CompletedQso>) {
        let offset = match &mut self.state {
            State::Calling { offset, tx_parity } => {
                if *tx_parity.get_or_insert(parity) != parity {
                    return (None, None);
                }
                *offset
            }
            _ => return (None, None),
        };
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
        // Snapshot what we need; the borrow ends before the `&mut self` calls.
        let (offset, message, do_log, finish) = match &self.state {
            State::Active(a) if parity == a.tx_parity => match &a.next {
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
            exchange_sent: sent,
            exchange_rcvd: rcvd,
        }
    }

    /// Project the internal state onto the published [`QsoState`].
    fn snapshot(&self) -> QsoState {
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

/// `RR73`/`RRR` — a roger that completes the report exchange.
fn is_roger(kind: Signoff) -> bool {
    matches!(kind, Signoff::Rr73 | Signoff::Rrr)
}

/// `73`/`RR73` — counts as a final sign-off (WSJT-X's `message_is_73`).
fn is_final(kind: Signoff) -> bool {
    matches!(kind, Signoff::Seven3 | Signoff::Rr73)
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
}
