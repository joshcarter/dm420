//! Bus error type. One enum for every failure mode the transport can surface.

/// Errors returned by [`BusHandle`](crate::BusHandle) operations and
/// [`Subscription::recv`](crate::Subscription::recv).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    /// A `request` did not receive a reply within its timeout.
    #[error("request timed out")]
    Timeout,

    /// A `request` was issued to a Command topic with no registered server.
    #[error("no handler registered for command topic")]
    NoHandler,

    /// A second `serve` was attempted on a Command topic that already has a server.
    #[error("a server is already registered for this command topic")]
    ServerExists,

    /// A `StreamLossy` **or** `StreamLossless` subscriber fell behind the live tail
    /// and the channel dropped `skipped` messages before this `recv`. The subscriber
    /// is still live; keep reading. (Lossless overflow now *lags* the subscriber —
    /// it is no longer disconnected for being slow; see `docs/bus-handoff.md`.)
    #[error("subscriber lagged, {skipped} message(s) skipped")]
    Lagged {
        /// How many messages were dropped.
        skipped: u64,
    },

    /// The subscription/channel is gone: every sender for the topic has been dropped
    /// (or a Command responder was dropped without replying). A slow `StreamLossless`
    /// subscriber is **no longer** closed for lagging — it now receives
    /// [`BusError::Lagged`] and stays subscribed.
    #[error("channel closed")]
    Closed,

    /// Serialization or deserialization failed (recorder / replay boundary).
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A topic string did not parse to a known [`Topic`](crate::Topic).
    #[error("bad topic: {0}")]
    BadTopic(String),

    /// The payload's [`BusMessage::CLASS`](crate::BusMessage::CLASS) disagreed
    /// with the topic's delivery class, or the payload type registered for the
    /// topic differs from the one used here.
    #[error("payload class/type disagrees with topic")]
    ClassMismatch,
}
