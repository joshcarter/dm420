//! Traffic recorder and replay.
//!
//! The recorder taps every *published* message (State / StreamLossy /
//! StreamLossless — commands flow through request/serve and aren't published),
//! serializing each to an NDJSON [`Envelope`] on disk. Serialization cost is paid
//! only while a recorder is attached. [`replay`] re-publishes a recorded file onto
//! a bus, preserving relative timing.
//!
//! The recorder *attach/stop* machinery lives in `handle.rs` (it touches the
//! `BusHandle` internals); this module owns the on-disk [`Envelope`] form and the
//! replay path, which is pure public-API.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::BusError;
use crate::handle::BusHandle;
use crate::topic::{Topic, TopicKind};

/// Current envelope schema version.
pub const ENVELOPE_VERSION: u16 = 1;

/// The recorded / future-network form of a message. In-process live delivery does
/// **not** build envelopes — this is only materialized at the recorder tap and
/// (later) the network boundary.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Envelope {
    pub version: u16,
    /// Canonical routing key (e.g. `"radio/k1/decodes"`).
    pub topic: String,
    /// Set for Command request/reply correlation; `None` for published messages.
    pub correlation: Option<u64>,
    pub payload: serde_json::Value,
    /// UTC ms when recorded.
    pub recorded_at: types::Timestamp,
}

/// Re-publish a recorded NDJSON file onto `bus`, preserving relative timing.
/// `speed` is a multiplier (use a very large value for golden tests to replay
/// near-instantly). Each envelope is deserialized back to its typed message and
/// published. Command and deferred-gossip topics in the file are skipped (they are
/// never produced by `publish`).
pub async fn replay(bus: &BusHandle, path: &Path, speed: f32) -> Result<(), BusError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| BusError::Serialization(e.to_string()))?;

    let mut prev_ts: Option<i64> = None;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let env: Envelope =
            serde_json::from_str(line).map_err(|e| BusError::Serialization(e.to_string()))?;

        if let Some(p) = prev_ts {
            let delta_ms = (env.recorded_at.0 - p).max(0) as f32;
            if speed > 0.0 && delta_ms > 0.0 {
                let wait_s = delta_ms / speed / 1000.0;
                if wait_s > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f32(wait_s)).await;
                }
            }
        }
        prev_ts = Some(env.recorded_at.0);
        publish_envelope(bus, &env)?;
    }
    Ok(())
}

/// Deserialize one envelope to its concrete payload type (by topic kind) and
/// publish it. Skips Command topics and deferred §9 gossip, which `publish` never
/// emits.
fn publish_envelope(bus: &BusHandle, env: &Envelope) -> Result<(), BusError> {
    use types::*;

    let topic = Topic::parse(&env.topic)?;

    macro_rules! pub_as {
        ($t:ty) => {{
            let msg: $t = serde_json::from_value(env.payload.clone())
                .map_err(|e| BusError::Serialization(e.to_string()))?;
            bus.publish(&topic, msg)
        }};
    }

    match topic.kind() {
        TopicKind::Spectrum => pub_as!(SpectrumRow),
        TopicKind::Decodes => pub_as!(Decode),
        TopicKind::DecodesEnriched => pub_as!(EnrichedDecode),
        TopicKind::RigState => pub_as!(RigState),
        TopicKind::Operating => pub_as!(OperatingState),
        TopicKind::TxReport => pub_as!(TxReport),
        TopicKind::TxLog => pub_as!(TxLogEntry),
        TopicKind::Selection => pub_as!(Selection),
        TopicKind::QsoState => pub_as!(QsoState),
        TopicKind::LogbookEntries => pub_as!(LogEntry),
        TopicKind::ScannerState => pub_as!(ScannerState),
        // Provisional: scanner/candidates payload shape is not finalized; carried
        // as BandActivity for now (class-consistent State).
        TopicKind::ScannerCandidates => pub_as!(BandActivity),
        TopicKind::ClockStatus => pub_as!(ClockStatus),
        TopicKind::Health => pub_as!(SubsystemHealth),
        // Not produced by publish(): command topics + deferred §9 gossip. Skip.
        TopicKind::RigCommand
        | TopicKind::SessionCommand
        | TopicKind::AudioTx
        | TopicKind::Interlock
        | TopicKind::QsoCommand
        | TopicKind::LogbookQuery
        | TopicKind::ScannerCommand
        | TopicKind::StationSnapshot => Ok(()),
    }
}
