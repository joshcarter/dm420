# docs/networking.md — LAN gossip between operators

**Status:** design (Josh-owned networking phase). Nothing here is built yet; this is
the spec the `net` crate and the §9 types will implement. Authoritative for the
on-the-wire protocol and the `net` crate's bus wiring.

DM420's third differentiator (after the waterslide and multi-band monitoring) is
**LAN sharing of contacts and heard-stations between operators, with no central
database**. This doc pins down how that works: discovery, transport, message
schema, merge semantics, and how the `net` service attaches to the bus — so a
second instance (e.g. a Raspberry Pi on the bench) sees the first one's log and
working-intent, and vice versa.

It implements the design intent in `OVERVIEW.md §5` and the deferred message shapes
in `docs/message-catalog.md §9` / `crates/types/src/lib.rs §9`.

## Decisions (settled)

- **Discovery:** mDNS/DNS-SD via the `mdns-sd` crate. Service type
  `_dm420._udp.local.`; each instance advertises its `StationId`, protocol
  version, and the UDP port it listens on. No static peer config needed on a LAN;
  a manual `DM420_PEERS=host:port,…` override exists for segments where mDNS is
  blocked.
- **Transport:** plain **UDP** (one socket, datagrams to each known peer). No TCP,
  no connection state. This matches the gossip model: state is periodically
  re-pushed, so a dropped datagram is self-healing.
- **Share scope:** worked contacts **and** heard-but-unworked stations **and**
  working-intent. The full collaboration set (see `StationSnapshot` below).
- **No central node, eventual consistency.** A peer learns what it happens to
  receive; correctness never depends on any single datagram arriving.

UDP is lossy and MTU-bounded (~1500 B practical, 64 KB hard cap). Two consequences
shape the protocol:

1. **Snapshots are latest-wins State** — losing one is harmless, the next push
   (every `SNAPSHOT_INTERVAL`, default 5 s) supersedes it. `heard` is recency-
   bounded so a snapshot fits one datagram; if it ever wouldn't, the oldest heard
   stations are dropped from the datagram (they re-appear when re-heard).
2. **Log entries are a G-set that must not lose members** — a dropped contact
   datagram would otherwise mean a peer *permanently* misses that QSO. So log
   sync runs an **anti-entropy** loop on top of UDP: peers periodically advertise
   a compact digest of what they hold, the receiver computes the gap, and
   re-requests it. Convergence comes from the digest **repeating** — any datagram
   lost this round is re-derived as a gap and re-requested next round — *not* from
   any single send being reliable. That's what gives reliable delivery over an
   unreliable datagram, without TCP.

   Two properties make this robust at scale (see "Merge semantics" for the worked
   reasoning):
   - **Digests are per-origin `seq` ranges, not id lists.** Because `seq` is
     monotonic per author, a peer's holdings for an origin are normally one
     contiguous range (`1..=high`), so "B logged 120 contacts while I was offline"
     is expressed and requested as a single trailing range — a few bytes — not 120
     ids.
   - **Bulk replies are chunked to the MTU.** A reply of 100+ full `LogEntry`s
     (~20–30 KB) sent as one datagram would be IP-fragmented into ~20 fragments,
     and losing any one fragment drops the *whole* datagram. So the server splits
     its reply across as many ~MTU-sized `LogReply` datagrams as needed; each
     succeeds or fails independently, and any lost chunk reappears as a gap on the
     next digest.

## Message schema (`types` §9)

These fill the `§9 — DEFERRED` placeholder in `crates/types/src/lib.rs`. All derive
the catalog's mandated `Serialize, Deserialize, Clone, Debug, PartialEq`. `seq` is a
per-station monotonic counter (process-lifetime or persisted), used for ordering and
as the G-set key — **never** a wall-clock, so clock skew between operators can't
reorder anything.

```rust
// §9  Cross-station gossip

/// One operator's current working intent — what peers consume to avoid competing
/// for the same contact. Published the moment we arm to a station or commit to a
/// caller; cleared back to None when we drop to Idle.
pub struct WorkingTarget {
    pub radio: RadioId,
    pub band: Band,
    pub offset: OffsetHz,
    pub call: Option<Callsign>,   // target station, once known
}

/// A station we've heard (decoded) but not necessarily worked. Shared so the map
/// and band-scan aggregate everyone's ears, not just ours.
pub struct HeardStation {
    pub call: Callsign,
    pub grid: Option<GridSquare>,
    pub band: Band,
    pub snr: i8,
    pub last_heard: Timestamp,    // aged by the *receiver's* local clock on receipt
}

/// Periodic full-state beacon, latest-wins per station (State-class topic
/// `station/{id}/snapshot`). Carries everything except the log G-set, which syncs
/// separately (see anti-entropy below).
pub struct StationSnapshot {
    pub station: StationId,
    pub seq: u64,                       // monotonic; supersedes any lower-seq snapshot
    pub working: Option<WorkingTarget>,
    pub band_activity: Vec<BandActivity>,   // already defined in §8
    pub heard: Vec<HeardStation>,           // recency-bounded to fit one datagram
}
```

The log-sync wire messages live in the `net` crate (they're transport framing, not
bus vocabulary):

```rust
/// What rides the UDP socket. `bincode`- or `serde_json`-encoded; version-gated.
enum Wire {
    Snapshot(StationSnapshot),       // unsolicited, every SNAPSHOT_INTERVAL
    LogPush(Vec<LogEntry>),          // proactive: new local contacts (origin == me only)
    LogDigest(Vec<OriginHave>),      // "what I hold," per origin as seq ranges
    LogRequest(Vec<OriginWant>),     // "send me these," per origin as seq ranges
    LogReply(Vec<LogEntry>),         // pull response; one MTU-bounded chunk (may be several)
}

/// One author's holdings in a digest. Normally a single contiguous range
/// `1..=high`; extra ranges only appear where earlier UDP loss left a hole.
struct OriginHave {
    origin: StationId,
    ranges: Vec<SeqRange>,           // inclusive (lo, hi) seq ranges held for this origin
}
struct OriginWant {                  // same shape; the gap the requester is missing
    origin: StationId,
    ranges: Vec<SeqRange>,
}
struct SeqRange { lo: u64, hi: u64 } // inclusive
```

## Merge semantics

- **Snapshots:** keyed by `StationId`, latest-wins by `seq`. A snapshot with `seq`
  ≤ the last one seen from that station is dropped. Entries are aged by **local
  receive time**, never the sender's timestamp — immune to clock skew. A peer that
  goes silent for `PEER_TTL` (default 30 s, i.e. several missed snapshots) is
  dropped from the live set; its heard stations and intent vanish from the UI.
- **Log entries:** a grow-only set (**G-set**) keyed by `QsoId { origin, seq }`.
  Merge = set union; a `QsoId` already held is ignored. Because each entry's
  `origin` is its author and each author is the single writer of its own ids, the
  union is conflict-free with no last-writer-wins needed. This is exactly the
  dedup the logbook already does on startup replay — the same `seen: HashSet<QsoId>`
  guard generalizes from "my own restart" to "the whole network."

  **Why per-origin ranges converge cheaply.** `seq` is monotonic per author, so a
  peer's holdings for an origin collapse to a high-water mark plus the occasional
  hole. Worked example: A had merged B up to `seq 47`, went offline an hour while B
  logged through `seq 167`. On rejoin, B's `LogDigest` advertises `{origin:B,
  ranges:[1..=167]}`; A diffs against its own `[1..=47]` and requests exactly
  `{origin:B, ranges:[48..=167]}` — one range, not 120 ids. B streams the 120
  entries as several MTU-sized `LogReply` chunks. If chunk N is lost, A's next
  digest shows the corresponding hole (e.g. `[48..=103, 112..=167]`), it re-requests
  just `104..=111`, and the round repeats until the ranges match. Earlier scattered
  losses are carried the same way, as extra small ranges.

  **Epidemic spread (no author needed).** The digest/serve path is **pull-based and
  serves *every* origin a peer holds**, so a third operator C that already merged
  B's entries can serve them to A even after B goes offline — the log spreads to
  whoever's online, not just from its author. The only non-converging case is two
  operators that are *never* online together with no third peer to bridge them
  (there's no store-and-forward through an offline peer's disk). Contrast the
  **proactive `LogPush`**, which is `origin == me` only (below) — that asymmetry is
  deliberate: push spontaneously, and only your own; relay only when asked.

## Bus wiring — the `net` crate

`net` is a new workspace crate that mirrors the `core::spawn` / `mocks::spawn`
service pattern: it owns its sockets on background tasks and talks to the rest of
the app **only over the bus**. It depends on `types` and `bus` (like every other
component) and on `mdns-sd` + tokio's UDP. It is spawned from `core::spawn` (real
mode) behind the same optional-config gate the logbook uses.

```
              ┌─────────────────────── this instance ──────────────────────┐
  bus topics  │                                                             │
  ───────────►│  net::spawn(bus, station_id, cfg)                           │
  logbook/    │    • subscribe LogbookEntries  ─ gossip OUT (origin==me)     │
  entries     │    • subscribe QsoState(rig0)  ─ derive my WorkingTarget     │
  qso/rig0/   │    • subscribe Decodes/Enriched ─ derive my HeardStations    │──► UDP ──►  peers
  state       │                                                             │◄── UDP ◄──
  decodes…    │    • publish  LogbookEntries   ◄ gossip IN  (origin!=me)     │
              │    • publish  StationSnapshot(peer) ◄ peers' beacons         │
  ◄───────────│    • mDNS browse/announce, anti-entropy timer               │
              └─────────────────────────────────────────────────────────────┘
```

### Outbound (my data → peers)

- **Log:** subscribe `logbook/entries`. Any entry with `origin == my StationId` is
  a new local contact → buffer it into the G-set and **proactively `LogPush`** it to
  peers. **Push guard:** entries with `origin != me` are ones *we* injected from a
  peer — never proactively re-push them (prevents an N-way echo storm). This guard
  is on *push only*: the anti-entropy digest still advertises, and `LogRequest`
  still serves, the **full** G-set across all origins — that's the pull-based relay
  that lets the log reach peers whose author is offline.
- **Intent:** subscribe `qso/{id}/state`. Map the engine's phase to
  `WorkingTarget`: `Armed`/`Calling`/`InExchange` → `Some(target)`, `Idle`/
  `Complete`/`TimedOut` → `None`. A change publishes an **immediate** snapshot
  (don't wait for the 5 s tick — collision avoidance is latency-sensitive).
- **Heard:** subscribe `radio/{id}/decodes_enriched` (already carries call/grid/
  worked status). Fold decodes into a recency-bounded `heard` map, attached to each
  periodic snapshot.

### Inbound (peers' data → my bus)

- **`LogPush` / `LogReply` (a request's answer):** for each entry whose `QsoId` we
  don't hold, add to the G-set and **publish it onto `logbook/entries`**. The
  existing logbook service is already subscribed there, so it **persists peer
  contacts to disk with zero new code** — and the map/log panels render them once
  they read `origin`. Entries we already hold are dropped (idempotent).
- **`LogRequest`:** for each `OriginWant` range, reply with the entries we hold in
  that range (any origin — this is the pull-based relay), split across as many
  MTU-sized `LogReply` datagrams as the range needs.
- **`Snapshot`:** publish onto `station/{peer}/snapshot` (the State topic already in
  the bus). Panels subscribe `station/*/snapshot` (wildcard) to show every peer's
  intent + heard set. The QSO engine consumes peers' `working` to exclude a station
  another op is working from auto-pick.
- **`LogDigest`:** compare against our G-set; for any `QsoId` the peer holds that we
  don't, send a `LogRequest`. (Symmetric: they do the same with our digests.)

### Anti-entropy timer

Every `DIGEST_INTERVAL` (default 15 s, jittered to avoid sync), broadcast a
`LogDigest` — our holdings as per-origin `seq` ranges (`Vec<OriginHave>`), which
stays compact even at Field-Day scale because a complete-prefix holding is one range
per author. On receiving a peer's digest, diff it against our own holdings and
`LogRequest` the difference; the peer answers with chunked `LogReply`s. This is what
makes the log converge despite UDP loss: any entry — or reply chunk — missed this
round shows up as a range gap and is re-requested next round, until both sides'
ranges match. Convergence is bounded by the digest interval, not by any send
succeeding.

## What the UI must add (separate from `net`)

The transport delivers peer data onto existing bus topics; the panels then have to
*render the distinction*. Today they ignore `origin`. Tracked as its own work:

- `MapSpot` / logbook rows gain `origin: StationId`; mine vs. peer gets a visual
  treatment (per `log_book.md` / `map_panel.md` — heard ≠ worked, mine ≠ peer).
- Waterslide flags a station another operator is `working` (from `station/*/snapshot`).
- Auto-pick (when built in the QSO engine) excludes `WorkedByNetwork` and
  currently-`working` stations — `qso/engine.rs:14-16` already notes this as the
  hook awaiting gossip.

## Config / env

| Variable | Purpose | Default |
|---|---|---|
| `DM420_STATION_ID` | this operator's `StationId` for gossip (stable per op) | host name |
| `DM420_NET` | enable LAN gossip (`1`) / disable (`0`) | enabled in real mode |
| `DM420_NET_PORT` | UDP listen port | `0` (ephemeral, advertised via mDNS) |
| `DM420_PEERS` | manual `host:port,…` peers when mDNS is unavailable | mDNS only |

## Build order (incremental, each demoable on two instances)

1. **`net` crate skeleton + §9 types.** mDNS announce/browse, UDP socket, `Wire`
   enum, snapshot push/receive. Prove two instances discover each other and
   exchange `StationSnapshot`s (log the peer set). No bus side-effects yet.
2. **Shared logbook.** Gossip `origin==me` `LogEntry`s out; inject peer entries onto
   `logbook/entries`; add the anti-entropy digest/request loop. Then the UI
   `origin` rendering. ← the headline win on two boxes.
3. **Working-intent.** Engine → `WorkingTarget` in snapshots; consume peers' intent;
   waterslide flag + auto-pick exclusion. ← the "don't compete" feature.
4. **Heard + band-activity aggregation** on the map / band-scan, then ADIF and
   multi-radio-in-one-box as Field-Day-timed follow-ups.

## Open questions

- **Encoding:** `serde_json` (debuggable, larger) vs. `bincode` (compact). Lean
  `bincode` for datagrams once the schema stabilizes; JSON during bring-up.
- **`seq` persistence:** persist the per-station log `seq` across restarts (so a
  restarted op doesn't reissue ids a peer already merged), or accept that a restart
  starts a fresh `seq` run and rely on `(origin, seq)` still being globally unique
  because `origin` is stable? Persisting is safer — write the high-water mark next
  to the logbook JSON.
- **Security:** LAN-trust only for v1 (Field Day club network). No auth/encryption;
  revisit if it ever leaves a trusted segment.
- **Heard-station privacy/volume:** whether to cap shared heard stations by band or
  SNR to keep datagrams small on a busy Field Day.
