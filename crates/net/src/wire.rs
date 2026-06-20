//! The on-the-wire gossip protocol — what rides the UDP socket.
//!
//! Every datagram is one [`Frame`]: a version + sender id + a [`Wire`] message.
//! Encoded as JSON during bring-up (debuggable; `docs/networking.md` plans a
//! switch to a compact codec once the schema settles). The log-sync variants are
//! declared here so the format is stable, but only [`Wire::Snapshot`] is handled
//! in step 1 (transport + discovery); the G-set anti-entropy loop is step 2.

use serde::{Deserialize, Serialize};
use types::{LogEntry, StationId, StationSnapshot};

/// Protocol version. Bumped on any incompatible `Frame`/`Wire` change; receivers
/// drop frames whose version they don't speak.
pub const PROTO_VERSION: u16 = 1;

/// One UDP datagram. The `from` id lets a receiver attribute the message without
/// reaching into the payload (and reply to `LogRequest`s by sender).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Frame {
    pub version: u16,
    pub from: StationId,
    pub msg: Wire,
}

/// A gossip message. See `docs/networking.md` for the full protocol.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Wire {
    /// Periodic full-state beacon (latest-wins per station by `seq`).
    Snapshot(StationSnapshot),
    /// Proactive push of *new local* contacts (`origin == me` only — the
    /// echo-storm guard). Step 2.
    LogPush(Vec<LogEntry>),
    /// "What I hold," per origin as `seq` ranges — the anti-entropy digest. Step 2.
    LogDigest(Vec<OriginHave>),
    /// "Send me these," per origin as `seq` ranges — the gap a peer is missing.
    /// Step 2.
    LogRequest(Vec<OriginWant>),
    /// Pull response: one MTU-bounded chunk of entries (a reply may span several).
    /// Step 2.
    LogReply(Vec<LogEntry>),
}

/// One author's holdings in a digest. Normally a single contiguous range
/// `1..=high`; extra ranges appear only where earlier UDP loss left a hole.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OriginHave {
    pub origin: StationId,
    pub ranges: Vec<SeqRange>,
}

/// The mirror of [`OriginHave`]: the gap a requester is missing for an author.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OriginWant {
    pub origin: StationId,
    pub ranges: Vec<SeqRange>,
}

/// An inclusive `[lo, hi]` run of `seq` values.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqRange {
    pub lo: u64,
    pub hi: u64,
}

/// Encode a frame for the wire.
pub fn encode(frame: &Frame) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(frame)
}

/// Decode a datagram back to a frame. Inverse of [`encode`].
pub fn decode(bytes: &[u8]) -> Result<Frame, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let frame = Frame {
            version: PROTO_VERSION,
            from: StationId("n0jdc".into()),
            msg: Wire::Snapshot(StationSnapshot {
                station: StationId("n0jdc".into()),
                seq: 7,
                working: None,
                band_activity: vec![],
                heard: vec![],
            }),
        };
        let bytes = encode(&frame).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back.version, PROTO_VERSION);
        assert_eq!(back.from, StationId("n0jdc".into()));
        match back.msg {
            Wire::Snapshot(s) => assert_eq!(s.seq, 7),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
