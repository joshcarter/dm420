//! Typed topics, their canonical-string round-trip, and the per-topic delivery
//! class. `Topic` is the routing key for in-process delivery and the future
//! network layer; `TopicKind` is the same set without the scope id, used for
//! wildcard subscription.

use types::{RadioId, StationId, SubsystemId};

use crate::error::BusError;

/// How a topic's messages are delivered. **Declared by the topic**, never chosen
/// at the call site — this is the single most important property of the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryClass {
    /// Latest-wins; only the most recent value matters (`tokio::sync::watch`).
    /// Late joiners receive the current value immediately.
    State,
    /// Bounded; drop-oldest under pressure (`tokio::sync::broadcast`). A lagging
    /// subscriber is told it lagged and never blocks the publisher. No late-join.
    StreamLossy,
    /// Every message, in order (per-subscriber `mpsc`). A full queue means a
    /// broken subscriber → it is disconnected; the publisher never blocks. Late
    /// joiners replay a retained ring, then live.
    StreamLossless,
    /// Reliable request/reply with one server per topic, ack/error, and timeout.
    Command,
}

/// A typed topic with its scope id. Round-trips to a canonical string via
/// [`Topic::canonical`] / [`Topic::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Topic {
    Spectrum(RadioId),
    Decodes(RadioId),
    DecodesEnriched(RadioId),
    RigState(RadioId),
    Operating(RadioId),
    RigCommand(RadioId),
    SessionCommand(RadioId),
    AudioTx(RadioId),
    TxReport(RadioId),
    Selection(RadioId),
    QsoCommand(RadioId),
    QsoState(RadioId),
    LogbookEntries,
    LogbookQuery,
    ScannerCommand,
    ScannerState,
    ScannerCandidates,
    ClockStatus,
    StationSnapshot(StationId),
    Health(SubsystemId),
}

/// The topic set without scope ids — the unit of wildcard subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopicKind {
    Spectrum,
    Decodes,
    DecodesEnriched,
    RigState,
    Operating,
    RigCommand,
    SessionCommand,
    AudioTx,
    TxReport,
    Selection,
    QsoCommand,
    QsoState,
    LogbookEntries,
    LogbookQuery,
    ScannerCommand,
    ScannerState,
    ScannerCandidates,
    ClockStatus,
    StationSnapshot,
    Health,
}

/// What to subscribe to: one exact topic, or every scope id of a kind (including
/// ids created later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicSelector {
    Exact(Topic),
    Wildcard(TopicKind),
}

impl Topic {
    /// The canonical routing string, e.g. `"radio/k1/decodes"`, `"logbook/entries"`.
    pub fn canonical(&self) -> String {
        match self {
            Topic::Spectrum(id) => format!("radio/{}/spectrum", id.0),
            Topic::Decodes(id) => format!("radio/{}/decodes", id.0),
            Topic::DecodesEnriched(id) => format!("radio/{}/decodes_enriched", id.0),
            Topic::RigState(id) => format!("radio/{}/rig_state", id.0),
            Topic::Operating(id) => format!("radio/{}/operating", id.0),
            Topic::RigCommand(id) => format!("radio/{}/command", id.0),
            Topic::SessionCommand(id) => format!("session/{}/command", id.0),
            Topic::AudioTx(id) => format!("radio/{}/audio_tx", id.0),
            Topic::TxReport(id) => format!("radio/{}/tx_report", id.0),
            Topic::Selection(id) => format!("selection/{}/active", id.0),
            Topic::QsoCommand(id) => format!("qso/{}/command", id.0),
            Topic::QsoState(id) => format!("qso/{}/state", id.0),
            Topic::LogbookEntries => "logbook/entries".to_string(),
            Topic::LogbookQuery => "logbook/query".to_string(),
            Topic::ScannerCommand => "scanner/command".to_string(),
            Topic::ScannerState => "scanner/state".to_string(),
            Topic::ScannerCandidates => "scanner/candidates".to_string(),
            Topic::ClockStatus => "clock/status".to_string(),
            Topic::StationSnapshot(sid) => format!("station/{}/snapshot", sid.0),
            Topic::Health(id) => format!("health/{}", id.as_str()),
        }
    }

    /// Parse a canonical string back to a `Topic`. Inverse of [`Topic::canonical`].
    pub fn parse(s: &str) -> Result<Topic, BusError> {
        let parts: Vec<&str> = s.split('/').collect();
        let topic = match parts.as_slice() {
            ["radio", id, "spectrum"] => Topic::Spectrum(RadioId(id.to_string())),
            ["radio", id, "decodes"] => Topic::Decodes(RadioId(id.to_string())),
            ["radio", id, "decodes_enriched"] => Topic::DecodesEnriched(RadioId(id.to_string())),
            ["radio", id, "rig_state"] => Topic::RigState(RadioId(id.to_string())),
            ["radio", id, "operating"] => Topic::Operating(RadioId(id.to_string())),
            ["radio", id, "command"] => Topic::RigCommand(RadioId(id.to_string())),
            ["radio", id, "audio_tx"] => Topic::AudioTx(RadioId(id.to_string())),
            ["radio", id, "tx_report"] => Topic::TxReport(RadioId(id.to_string())),
            ["session", id, "command"] => Topic::SessionCommand(RadioId(id.to_string())),
            ["selection", id, "active"] => Topic::Selection(RadioId(id.to_string())),
            ["qso", id, "command"] => Topic::QsoCommand(RadioId(id.to_string())),
            ["qso", id, "state"] => Topic::QsoState(RadioId(id.to_string())),
            ["logbook", "entries"] => Topic::LogbookEntries,
            ["logbook", "query"] => Topic::LogbookQuery,
            ["scanner", "command"] => Topic::ScannerCommand,
            ["scanner", "state"] => Topic::ScannerState,
            ["scanner", "candidates"] => Topic::ScannerCandidates,
            ["clock", "status"] => Topic::ClockStatus,
            ["station", sid, "snapshot"] => Topic::StationSnapshot(StationId(sid.to_string())),
            ["health", id] => {
                let sid = SubsystemId::parse(id).ok_or_else(|| BusError::BadTopic(s.to_string()))?;
                Topic::Health(sid)
            }
            _ => return Err(BusError::BadTopic(s.to_string())),
        };
        Ok(topic)
    }

    /// The scope-free kind of this topic (used for wildcard routing).
    pub fn kind(&self) -> TopicKind {
        match self {
            Topic::Spectrum(_) => TopicKind::Spectrum,
            Topic::Decodes(_) => TopicKind::Decodes,
            Topic::DecodesEnriched(_) => TopicKind::DecodesEnriched,
            Topic::RigState(_) => TopicKind::RigState,
            Topic::Operating(_) => TopicKind::Operating,
            Topic::RigCommand(_) => TopicKind::RigCommand,
            Topic::SessionCommand(_) => TopicKind::SessionCommand,
            Topic::AudioTx(_) => TopicKind::AudioTx,
            Topic::TxReport(_) => TopicKind::TxReport,
            Topic::Selection(_) => TopicKind::Selection,
            Topic::QsoCommand(_) => TopicKind::QsoCommand,
            Topic::QsoState(_) => TopicKind::QsoState,
            Topic::LogbookEntries => TopicKind::LogbookEntries,
            Topic::LogbookQuery => TopicKind::LogbookQuery,
            Topic::ScannerCommand => TopicKind::ScannerCommand,
            Topic::ScannerState => TopicKind::ScannerState,
            Topic::ScannerCandidates => TopicKind::ScannerCandidates,
            Topic::ClockStatus => TopicKind::ClockStatus,
            Topic::StationSnapshot(_) => TopicKind::StationSnapshot,
            Topic::Health(_) => TopicKind::Health,
        }
    }

    /// The delivery class for this topic — the single source of truth.
    pub fn delivery_class(&self) -> DeliveryClass {
        self.kind().delivery_class()
    }
}

impl TopicKind {
    /// The delivery class every topic of this kind uses. Mirrors the catalog §11
    /// topic registry.
    pub fn delivery_class(&self) -> DeliveryClass {
        use DeliveryClass::*;
        match self {
            TopicKind::Spectrum => StreamLossy,
            TopicKind::Decodes => StreamLossless,
            TopicKind::DecodesEnriched => StreamLossless,
            TopicKind::LogbookEntries => StreamLossless,
            TopicKind::RigState => State,
            TopicKind::Operating => State,
            TopicKind::TxReport => State,
            TopicKind::Selection => State,
            TopicKind::QsoState => State,
            TopicKind::ScannerState => State,
            TopicKind::ScannerCandidates => State,
            TopicKind::ClockStatus => State,
            TopicKind::StationSnapshot => State,
            TopicKind::Health => State,
            TopicKind::RigCommand => Command,
            TopicKind::SessionCommand => Command,
            TopicKind::AudioTx => Command,
            TopicKind::QsoCommand => Command,
            TopicKind::LogbookQuery => Command,
            TopicKind::ScannerCommand => Command,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance criterion #2: `Topic::parse(t.canonical()) == t` for every
    /// variant, and `delivery_class()` matches the catalog registry.
    #[test]
    fn topic_round_trip_and_class() {
        let r = || RadioId("k1".into());
        let cases = [
            (Topic::Spectrum(r()), DeliveryClass::StreamLossy),
            (Topic::Decodes(r()), DeliveryClass::StreamLossless),
            (Topic::DecodesEnriched(r()), DeliveryClass::StreamLossless),
            (Topic::RigState(r()), DeliveryClass::State),
            (Topic::Operating(r()), DeliveryClass::State),
            (Topic::RigCommand(r()), DeliveryClass::Command),
            (Topic::SessionCommand(r()), DeliveryClass::Command),
            (Topic::AudioTx(r()), DeliveryClass::Command),
            (Topic::TxReport(r()), DeliveryClass::State),
            (Topic::Selection(r()), DeliveryClass::State),
            (Topic::QsoCommand(r()), DeliveryClass::Command),
            (Topic::QsoState(r()), DeliveryClass::State),
            (Topic::LogbookEntries, DeliveryClass::StreamLossless),
            (Topic::LogbookQuery, DeliveryClass::Command),
            (Topic::ScannerCommand, DeliveryClass::Command),
            (Topic::ScannerState, DeliveryClass::State),
            (Topic::ScannerCandidates, DeliveryClass::State),
            (Topic::ClockStatus, DeliveryClass::State),
            (Topic::StationSnapshot(StationId("s1".into())), DeliveryClass::State),
            (Topic::Health(SubsystemId::Rig), DeliveryClass::State),
            (Topic::Health(SubsystemId::Audio), DeliveryClass::State),
        ];
        for (topic, class) in cases {
            let s = topic.canonical();
            assert_eq!(Topic::parse(&s).unwrap(), topic, "round-trip failed for {s}");
            assert_eq!(topic.delivery_class(), class, "wrong class for {s}");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        for bad in ["", "radio", "radio/k1", "radio/k1/nope", "bogus/topic", "logbook/nope"] {
            assert!(Topic::parse(bad).is_err(), "expected error for {bad:?}");
        }
    }
}
