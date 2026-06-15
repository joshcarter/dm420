//! Radio-agnostic application core.
//!
//! Owns the cross-cutting services that have no business in a single panel or
//! back-end: the UTC clock/scheduler (the 15 s / 7.5 s heartbeat), the interlock
//! granter (TX arbitration), decode enrichment (joining raw decodes against the
//! merged logbook), and composition of app-level state published on the bus.
//!
//! Specs: `OVERVIEW.md` §3.2 (workers), `docs/message-catalog.md` §10 (clock +
//! interlock), §3 (`EnrichedDecode`).
