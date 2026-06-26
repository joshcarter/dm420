//! The QSO engine — the contact state machine.
//!
//! Given the last message addressed to us and the active `ContestProfile`, it
//! computes the next outgoing message, advances on each slot boundary, and emits
//! completed contacts to the logbook. The "send on next interval" timing is the
//! core's job (engine + clock), not the UI's.
//!
//! Two layers:
//! - [`engine`] — the pure, synchronous state machine ([`Engine`]). No I/O; one
//!   [`Event`] in, one [`Step`] out. Exhaustively unit-tested.
//! - [`spawn`] — the async bus shell: serves `qso/{id}/command`, consumes
//!   decodes + selection + the clock, publishes `QsoState`, and logs completed
//!   contacts. Actual transmission is gated behind `allow_transmit` (off until
//!   the PTT interlock granter + audio-TX path exist — `docs/qso_flow.md`,
//!   `TODO.md`).
//!
//! Specs: `docs/qso_flow.md`, `docs/wsjtx_qso_sequencing.md`,
//! `docs/message-catalog.md` §5.

#![forbid(unsafe_code)]

mod engine;
/// Outgoing-message builders ([`message::cq`], [`message::answer_grid`],
/// [`message::fd_exchange`], …). Public so consumers — notably the GUI send-box
/// preview — render the *would-be* next message with the same formatter the engine
/// transmits with, instead of re-deriving the on-air strings (which drift).
pub mod message;
mod shell;

pub use engine::{CompletedQso, Engine, Event, Step, TxIntent};
pub use message::StationConfig;
pub use shell::{QsoAck, QsoControl, spawn};
