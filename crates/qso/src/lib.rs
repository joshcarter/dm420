//! The QSO engine — the contact state machine.
//!
//! Given the last message addressed to us and the active `ContestProfile`, it
//! computes the next outgoing message, advances on each slot boundary, and emits
//! completed contacts to the logbook. The "send on next interval" timing is the
//! core's job (engine + clock), not the UI's.
//!
//! Specs: `docs/radio_control.md`, `docs/message-catalog.md` §5.
