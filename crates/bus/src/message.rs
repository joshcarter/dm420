//! The [`BusMessage`] trait ties each payload type to its delivery class, letting
//! the bus assert at publish/subscribe time that a payload agrees with its topic.
//!
//! The impls below are the authoritative payload→class binding for the vocabulary
//! in the [`types`] crate; they mirror the catalog §11 topic registry. The trait
//! is local to this crate, so these impls don't run into the orphan rule.

use serde::{Serialize, de::DeserializeOwned};

use crate::topic::DeliveryClass;

/// A type that can ride the bus. `CLASS` is the delivery class of the topic(s)
/// this payload is published on — the bus checks it against the topic's own
/// `delivery_class()` to catch wiring mistakes.
pub trait BusMessage: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// The delivery class this payload is carried under.
    const CLASS: DeliveryClass;
}

macro_rules! bus_message {
    ($ty:ty, $class:expr) => {
        impl BusMessage for $ty {
            const CLASS: DeliveryClass = $class;
        }
    };
}

use DeliveryClass::*;
use types::*;

// --- StreamLossy ---
bus_message!(SpectrumRow, StreamLossy);

// --- StreamLossless ---
bus_message!(Decode, StreamLossless);
bus_message!(EnrichedDecode, StreamLossless);
bus_message!(LogEntry, StreamLossless);
bus_message!(TxLogEntry, StreamLossless);

// --- State ---
bus_message!(RigState, State);
bus_message!(OperatingState, State);
bus_message!(TxReport, State);
bus_message!(Selection, State);
bus_message!(QsoState, State);
bus_message!(ScannerState, State);
bus_message!(ClockStatus, State);
bus_message!(Vec<BandActivity>, State); // scanner/candidates payload — full per-scan snapshot
bus_message!(StationSnapshot, State); // station/{id}/snapshot (State, gossiped — §9)
bus_message!(SubsystemHealth, State); // health/{id} (State, latest-wins per subsystem)

// --- Command (request payloads; reply types are chosen per call site) ---
bus_message!(RigCommand, Command);
bus_message!(SessionCommand, Command);
bus_message!(TxRequest, Command);
bus_message!(TxAck, Command);
bus_message!(InterlockRequest, Command);
bus_message!(InterlockReply, Command);
bus_message!(QsoCommand, Command);
bus_message!(ScannerCommand, Command);
bus_message!(ScannerAck, Command);
