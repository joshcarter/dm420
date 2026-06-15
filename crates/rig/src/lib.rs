//! Radio back-ends behind the `RadioBackend` trait.
//!
//! v1 implements the Kenwood TS-480SAT / TS-590S CAT path (serial control of
//! frequency / mode / PTT) and reports `RigState`. The trait advertises
//! capabilities (e.g. `simultaneous_receivers`) so a future multi-receiver SDR
//! back-end is expressible without reworking the domain model.
//!
//! Add `serialport.workspace = true` when wiring the real CAT link.
//!
//! Specs: `OVERVIEW.md` §3.5, `docs/radio_control.md`, `docs/message-catalog.md` §4.
//! Owner: Joel (W4LL).
