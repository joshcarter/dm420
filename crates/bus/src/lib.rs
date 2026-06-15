//! The message bus — the spine every component communicates through.
//!
//! `BusHandle` over tokio channels with four delivery classes (`State`,
//! `StreamLossy`, `StreamLossless`, `Command`), wildcard subscription by topic
//! kind, late-join snapshot semantics, and a record/replay path. The non-
//! negotiable invariant: **a slow or absent subscriber must never stall a
//! publisher.**
//!
//! Payload types come from the [`types`] crate (re-exported here so existing
//! handoff code reads `bus::types::*`). This crate owns only the transport.
//!
//! Spec: `docs/bus-handoff.md` (transport) + `docs/message-catalog.md` (§11 topic
//! registry). Owner: Josh (N0JDC).

pub use types;
