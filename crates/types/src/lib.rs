//! Shared message vocabulary for the whole application.
//!
//! Every type that crosses the bus is defined here: the scalar newtypes
//! (`RadioId`, `AbsHz`, `OffsetHz`, `Callsign`, …) and the payload structs/enums
//! from `docs/message-catalog.md` (§1–§10). All derive
//! `Serialize, Deserialize, Clone, Debug, PartialEq` and hold no non-serializable
//! handles — that is the hard rule that keeps the future network transport open.
//!
//! This crate is deliberately dependency-light (serde only) so the pure-compute
//! crates (`dsp`, `modes`, `logbook`) and Joel's `rig`/`modes` work can use the
//! vocabulary without pulling in tokio or the bus.
//!
//! Spec: `docs/message-catalog.md`. Implemented alongside the `bus` task.
