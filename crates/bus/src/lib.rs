//! The message bus — the spine every component communicates through.
//!
//! `BusHandle` over tokio channels with four delivery classes ([`DeliveryClass`]:
//! `State`, `StreamLossy`, `StreamLossless`, `Command`), wildcard subscription by
//! topic kind, late-join snapshot semantics, and (in [`recorder`]) a record/replay
//! path. The non-negotiable invariant: **a slow or absent subscriber must never
//! stall a publisher.**
//!
//! Payload types come from the [`types`] crate (re-exported here so handoff code
//! reads `bus::types::*`). This crate owns only the transport.
//!
//! # The four delivery classes
//!
//! | Class | Channel | Late-join |
//! |---|---|---|
//! | [`DeliveryClass::State`] | `watch` | current value immediately |
//! | [`DeliveryClass::StreamLossy`] | `broadcast` | none (future messages only) |
//! | [`DeliveryClass::StreamLossless`] | per-subscriber `mpsc` + ring | replays the ring, then live |
//! | [`DeliveryClass::Command`] | request/reply | n/a |
//!
//! A topic *declares* its class ([`Topic::delivery_class`]); the call site never
//! chooses it. [`BusMessage::CLASS`] ties a payload to its class so the bus can
//! reject a payload published on the wrong topic.
//!
//! # Example
//!
//! ```
//! use bus::{BusHandle, Topic, TopicSelector};
//! use bus::types::{ClockStatus, SlotId};
//!
//! # async fn ex() -> Result<(), bus::BusError> {
//! let bus = BusHandle::new();
//! let mut sub = bus.subscribe::<ClockStatus>(TopicSelector::Exact(Topic::ClockStatus))?;
//! bus.publish(&Topic::ClockStatus, ClockStatus { offset_ms: -3.0, slot_phase: 0.1, slot: SlotId(0) })?;
//! let got = sub.recv().await?; // late-join: yields the current State value
//! assert_eq!(got.offset_ms, -3.0);
//! # Ok(()) }
//! ```
//!
//! Spec: `docs/bus-handoff.md` (transport) + `docs/message-catalog.md` (§11 topic
//! registry). Owner: Josh (N0JDC).

#![forbid(unsafe_code)]

pub use types;

mod error;
mod handle;
mod message;
mod recorder;
mod topic;

pub use error::BusError;
pub use handle::{BusHandle, RecorderHandle, RequestStream, Responder, Subscription};
pub use message::BusMessage;
pub use recorder::{ENVELOPE_VERSION, Envelope, replay};
pub use topic::{DeliveryClass, Topic, TopicKind, TopicSelector};
