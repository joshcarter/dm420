# Claude Code Handoff — `bus` crate (v1, in-process)

## Goal

Implement the message bus that is the spine of an HF digital-mode radio application. Every
component (rig managers, decoders, GUI panels, scanner, logbook) communicates *only* through this
bus via typed, serde-serializable messages on scoped topics. This task builds the **v1 in-process
implementation**: a `BusHandle` over tokio channels. It must be designed so the same message types
and API can later ride a network transport unchanged — but **do not implement networking now.**

Pair this brief with `message-catalog-v0.1.md` (place it at `docs/message-catalog.md`). That catalog
is the authoritative source for all message *payload* types (§2–§10 there). This brief defines the
*transport layer* around them.

## Scope

**Build now:**
- The message payload types from the catalog (a `types` module), all deriving
  `Serialize, Deserialize, Clone, Debug, PartialEq`.
- `Topic` / `TopicKind` with canonical-string round-trip and per-topic delivery class.
- `BusHandle` with `publish`, `subscribe`, `request`, `serve` over four delivery classes.
- Late-join snapshot semantics per class.
- Wildcard subscription by topic kind (`radio/*/decodes`).
- A traffic recorder (record all envelopes to disk) and a replay function.
- Unit + integration tests and runnable examples.

**Out of scope — design so as not to preclude, but DO NOT build:**
- Network transport (WebSocket). Keep the `Envelope` + canonical-string routing ready for it.
- Cross-station gossip / mDNS. (A future bridge is just another bus client; see §9.)
- The GUI, real DSP/decoder, real rig CAT driver. Use fakes in tests/examples.
- Interlock *granter* logic. The bus carries interlock messages as Command topics; the granting
  service lives in `core` and is a separate task. (Noted in §10 so first-light TX is unblocked.)

## Workspace & dependencies

Assume a Cargo workspace (create it if absent) with this member set; **this task implements only
`bus`**: `bus, rig, audio, dsp, modes, qso, logbook, scanner, core, gui`.

`bus/Cargo.toml` (use current stable versions; these are minimums):
```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
```

Constraints: Rust stable, edition 2021. No `unsafe`. No blocking calls on async tasks. `BusHandle`
is `Clone + Send + Sync` and usable from many tasks. Public items documented. `cargo clippy` clean.

## Delivery classes

Each topic declares exactly one class. This is the single most important property — it is *declared
by the topic*, not chosen ad hoc at the call site.

| Class | Channel | Semantics | Late-join |
|---|---|---|---|
| `State` | `tokio::sync::watch` | latest-wins; only the most recent value matters | receives current value immediately |
| `StreamLossy` | `tokio::sync::broadcast` | bounded; drop-oldest under pressure; a lagging subscriber is told it lagged, never blocks the publisher | none (just starts receiving) |
| `StreamLossless` | per-subscriber bounded `mpsc` | every message, in order; **a full queue = a broken subscriber → disconnect it; never block the publisher** | replays a retained ring, then live |
| `Command` | request/reply | reliable, one server per topic, ack/error, timeout | n/a |

The non-negotiable invariant across all of this: **a slow or absent subscriber must never stall a
publisher.** For lossless topics this is enforced by dropping the subscriber when its queue is full
(its next `recv` returns `Closed`; it is expected to re-subscribe and get a fresh snapshot).

## Transport-layer types to implement

```rust
pub enum DeliveryClass { State, StreamLossy, StreamLossless, Command }

/// Typed topic. Round-trips to a canonical string for the recorder and the future network layer.
/// `TopicKind` is the same set without the scope id (used for wildcard subscription).
pub enum Topic {
    Spectrum(RadioId),          // StreamLossy
    Decodes(RadioId),           // StreamLossless
    DecodesEnriched(RadioId),   // StreamLossless
    RigState(RadioId),          // State
    Operating(RadioId),         // State
    RigCommand(RadioId),        // Command
    SessionCommand(RadioId),    // Command
    AudioTx(RadioId),           // Command
    TxReport(RadioId),          // State
    Selection(RadioId),         // State
    QsoCommand(RadioId),        // Command
    QsoState(RadioId),          // State
    LogbookEntries,             // StreamLossless
    LogbookQuery,               // Command
    ScannerCommand,             // Command
    ScannerState,               // State
    ScannerCandidates,          // State
    ClockStatus,                // State
    StationSnapshot(StationId), // State
}

impl Topic {
    pub fn canonical(&self) -> String;              // e.g. "radio/k1/decodes", "logbook/entries"
    pub fn parse(s: &str) -> Result<Topic, BusError>;
    pub fn kind(&self) -> TopicKind;
    pub fn delivery_class(&self) -> DeliveryClass;  // single source of truth
}

/// Tie each payload type to its delivery class (and let the bus assert topic/payload agreement).
pub trait BusMessage:
    serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static
{
    const CLASS: DeliveryClass;
}

pub enum TopicSelector {
    Exact(Topic),
    Wildcard(TopicKind), // matches every scope id for this kind, including ids created later
}

/// Recorder / future-network form only. In-process live delivery does NOT build envelopes.
pub struct Envelope {
    pub version: u16,
    pub topic: String,            // canonical routing key
    pub correlation: Option<u64>, // set for Command request/reply
    pub payload: serde_json::Value,
    pub recorded_at: Timestamp,   // from catalog (UTC ms)
}

#[derive(thiserror::Error, Debug)]
pub enum BusError {
    Timeout,
    NoHandler,            // request to a Command topic with no server
    ServerExists,         // second serve() on a Command topic
    Lagged { skipped: u64 }, // StreamLossy subscriber fell behind
    Closed,               // channel/subscription gone (e.g. lossless subscriber was dropped)
    Serialization(String),
    BadTopic(String),
    ClassMismatch,        // payload CLASS disagrees with topic.delivery_class()
}
```

### `BusHandle`

```rust
#[derive(Clone)]
pub struct BusHandle { /* Arc<Inner> */ }

impl BusHandle {
    pub fn new() -> Self;

    /// State / StreamLossy / StreamLossless only. Returns immediately; never blocks on a
    /// subscriber. Asserts `M::CLASS == topic.delivery_class()`.
    pub fn publish<M: BusMessage>(&self, topic: &Topic, msg: M) -> Result<(), BusError>;

    /// On first recv, a `State` sub yields the current value (if any); a `StreamLossless` sub
    /// yields the retained ring in order; a `StreamLossy` sub yields only future messages.
    pub fn subscribe<M: BusMessage>(&self, sel: TopicSelector)
        -> Result<Subscription<M>, BusError>;

    /// Command topics. Mints a correlation id, awaits the reply or times out.
    pub async fn request<Req, Rep>(&self, topic: &Topic, req: Req, timeout: Duration)
        -> Result<Rep, BusError>
    where Req: BusMessage, Rep: BusMessage;

    /// Register the single server for a Command topic. Errors `ServerExists` if already served.
    pub fn serve<Req, Rep>(&self, topic: &Topic) -> Result<RequestStream<Req, Rep>, BusError>
    where Req: BusMessage, Rep: BusMessage;

    /// Tap every published message, serialize to NDJSON envelopes at `path`. Opt-in: serialization
    /// cost is only paid while a recorder is attached.
    pub fn attach_recorder(&self, path: &Path) -> Result<RecorderHandle, BusError>;
}

pub struct Subscription<M> { /* .. */ }
impl<M: BusMessage> Subscription<M> {
    /// Snapshot item(s) first (per class), then live. `Err(Lagged)` on lossy fall-behind;
    /// `Err(Closed)` if a lossless subscription was dropped for being too slow (re-subscribe).
    pub async fn recv(&mut self) -> Result<M, BusError>;
}

pub struct RequestStream<Req, Rep> { /* .. */ }
impl<Req: BusMessage, Rep: BusMessage> RequestStream<Req, Rep> {
    pub async fn next(&mut self) -> Option<(Req, Responder<Rep>)>;
}
pub struct Responder<Rep> { /* holds correlation routing */ }
impl<Rep: BusMessage> Responder<Rep> {
    pub fn reply(self, rep: Rep);
}

/// Re-publish a recorded NDJSON file onto a bus, preserving relative timing (`speed` multiplier;
/// use a very large speed for golden tests). Deserializes each envelope back to its typed message.
pub async fn replay(bus: &BusHandle, path: &Path, speed: f32) -> Result<(), BusError>;
```

### Implementation notes (recommended, not mandated)

- Keep a registry of per-topic channels created lazily on first publish/subscribe. For the hot path,
  store concrete typed channels (type-erase with `Any` + downcast) so live delivery does **not**
  serialize. Serialize only at the recorder tap and (future) network boundary.
- `State` = `watch::channel`; `StreamLossy` = `broadcast::channel(cap)`; `StreamLossless` = a fan-out
  of per-subscriber `mpsc::channel(cap)` plus a retained `VecDeque` ring per topic for replay.
- Retention (replay ring) sizes are per-topic config with sane defaults: decodes ≈ last 10 slots,
  `logbook/entries` ≈ a few thousand, others as needed. Make it a small config struct.
- `request`/`serve`: correlation id from an `AtomicU64`. In-process, map id → `oneshot::Sender<Rep>`;
  `Responder::reply` fires the oneshot. The correlation id rides the `Envelope` so the same shape
  works over a network later. One server per Command topic.
- Wildcard: on publish, route to exact-topic subscribers *and* to `Wildcard(kind)` subscribers whose
  kind matches.

## Integration pattern (how components connect afterward)

Producers publish; consumers subscribe; commands use request/serve. Minimal shapes:

```rust
// Rig manager (owns one radio): serve its command topic, publish its state/decodes.
let mut cmds = bus.serve::<RigCommand, CommandResult>(&Topic::RigCommand(id.clone()))?;
tokio::spawn(async move {
    while let Some((cmd, responder)) = cmds.next().await {
        let result = apply_to_radio(cmd);          // CAT I/O
        responder.reply(result);
    }
});
bus.publish(&Topic::RigState(id.clone()), rig_state)?; // on every state change

// A panel (stateless view): subscribe and reconstruct from the bus alone.
let mut decodes = bus.subscribe::<EnrichedDecode>(
    TopicSelector::Wildcard(TopicKind::DecodesEnriched))?; // all radios
while let Ok(d) = decodes.recv().await { waterslide.place(d); }
```

Suggested bring-up order for the two of us afterward: (1) bus + a fake rig manager that `replay`s a
recorded/canned decode file, (2) the waterslide panel subscribing to spectrum + decodes, (3) swap in
the real rig manager and decoder. The replay path means the UI can be built with no radio attached.

## Acceptance criteria (make these tests)

1. **Serde round-trip** for every payload type in the catalog (to `serde_json` and back, equal).
2. **Topic round-trip**: `Topic::parse(t.canonical()) == t` for every variant; `delivery_class()`
   matches the catalog registry.
3. **State late-join**: publish a value, then subscribe → first `recv` returns that value.
4. **Lossless order + late-join**: publish N, subscribe → retained ring replays in order, then live
   messages continue with no gaps or reordering.
5. **Lossy under load**: a slow lossy subscriber gets `Err(Lagged)` and the publisher never blocks;
   a second healthy subscriber on the same topic is unaffected.
6. **Lossless slow subscriber never stalls the publisher**: a subscriber that stops draining is
   disconnected (its next `recv` → `Closed`); the publisher keeps going; a re-subscribe yields a
   fresh snapshot.
7. **Request/reply**: `request` returns the served reply; `Timeout` when no reply arrives in time;
   `NoHandler` when no server is registered; second `serve` on a topic → `ServerExists`.
8. **Record → replay golden test**: record a scripted session, `replay` at high speed onto a fresh
   bus, capture the re-published sequence, assert it equals the original envelope sequence.
9. **Concurrency**: clone `BusHandle` across several tokio tasks publishing/subscribing concurrently
   without deadlock or data races (run under `cargo test` with the multi-thread runtime).

Verify with `cargo test`, `cargo clippy --all-targets -- -D warnings`, and run the two examples in
`examples/` (`fake_pubsub.rs`, `fake_command.rs`).

## Deliverables

- `bus` crate: `types` module (catalog payloads), `topic`, `delivery` (the four classes),
  `handle` (`BusHandle`), `recorder`, `error`, with doc comments.
- `examples/fake_pubsub.rs`, `examples/fake_command.rs`.
- Tests covering all acceptance criteria above.
- A short `bus/README.md`: the four classes, the publish/subscribe/request/serve API, and the
  "no subscriber may stall a publisher" invariant.
