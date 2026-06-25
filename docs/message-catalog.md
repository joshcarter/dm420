# HF Digital Mode App — Message Catalog v0.1

**Status:** first cut for review. The *rig/decode seam* (§2–§6) is the priority for Joel;
the *bus plumbing and cross-station* types (§7–§10) are Josh's domain and are sketched but
concrete enough to compile against. Items marked **[Joel owns]** are protocol-shape calls that
should follow `ft8_lib` output and the Field Day message set rather than this draft.

## 0. Conventions

- **Derives:** every type here derives `Serialize, Deserialize, Clone, Debug, PartialEq`.
  Nothing on the bus holds a non-serializable handle (no `Arc<dyn _>`, no channels) — this is
  the hard rule that keeps the network phase open.
- **Frequency model (resolves the §6 `f0_hz` TBD):** absolute dial frequency and audio-passband
  offset are *different types* so they can't be mixed up. Spectrum, decodes, and selection live
  in **offset** space (the waterslide's vertical axis); `RigState` carries the **absolute** VFO.
  Absolute = `vfo + offset`, computed only at the edges (logging, CAT, map is by locator anyway).
- **Mode vs rig mode:** `OverAirMode` is the on-air protocol (what Josh means by "mode").
  `RigMode` is the radio's sideband/data setting. The panel's "Mode" control sets `OverAirMode`;
  the session layer derives the `RigMode` it implies.
- **Single-writer per topic:** each topic has exactly one publisher. This is what makes the
  cross-station gossip merge trivial and is also why `rig_state` and `operating` are split (§4).
- **Delivery classes** (see the registry in §11): `State` (latest-wins, `watch`),
  `StreamLossy` (drop under pressure, `broadcast`), `StreamLossless` (per-subscriber `mpsc`,
  full queue = dead subscriber), `Command` (request/reply with correlation id + timeout).

```rust
// §1 scalar newtypes — cheap, but they stop the absolute/offset and call/grid mix-ups.
pub struct RadioId(pub String);      // stable per-radio id from config
pub struct StationId(pub String);    // one operator/core instance (multi-op gossip)
pub struct Callsign(pub String);
pub struct GridSquare(pub String);   // Maidenhead, e.g. "FN31"
pub struct Section(pub String);      // ARRL/RAC section, e.g. "CO" — exchange/log, and a coarse map location (Field Day, no grid)

pub struct AbsHz(pub u64);           // absolute dial frequency
pub struct OffsetHz(pub f32);        // audio offset within the passband

pub struct Timestamp(pub i64);       // UTC, ms since epoch (slot alignment, logging)
pub struct SlotId(pub u64);          // FT8=15s / FT4=7.5s slot index from the clock module

pub enum OverAirMode { Ft8, Ft4, Psk31, Rtty } // on-air protocol ("mode")
pub enum RigMode { UsbData, LsbData, Usb, Lsb, Cw } // radio sideband/data setting
pub enum Band { B160m, B80m, B40m, B30m, B20m, B17m, B15m, B12m, B10m, B6m }

pub enum ContestProfile { Standard, ArrlFieldDay } // both ship in v1; extend later

pub enum SignalSource { Received, OwnTx } // own-TX rendering on the waterslide
```

## 2. Spectrum  —  `radio/{id}/spectrum`  (StreamLossy)

```rust
pub struct SpectrumRow {
    pub radio: RadioId,
    pub mode: OverAirMode,
    pub t: Timestamp,
    pub bin0_offset: OffsetHz, // audio offset of bin 0
    pub bin_hz: f32,           // offset step per bin
    pub mags: Vec<u8>,         // log-scaled magnitudes
    pub source: SignalSource,  // OwnTx rows let the waterslide draw our own TX distinctly
}
```

## 3. Decodes  —  raw `radio/{id}/decodes` (StreamLossless), enriched `…/decodes_enriched`

```rust
pub struct Decode {
    pub radio: RadioId,
    pub mode: OverAirMode,
    pub t: Timestamp,          // slot start (slotted) or arrival (streaming)
    pub offset: OffsetHz,      // audio offset of the signal
    pub snr_db: Option<i8>,
    pub source: SignalSource,
    pub content: DecodeContent,
}

// The family split that makes PSK31 a sibling, not a rewrite.
pub enum DecodeContent {
    // `message` is the parse; `raw` is the decoder's verbatim text (e.g. "CQ W1ABC
    // FN42") kept alongside so the two can be compared (the decode archive saves both).
    Slotted { slot: SlotId, dt: f32, message: ParsedMessage, raw: String }, // FT8/FT4
    Streaming { text: String },                                             // PSK31/RTTY (arch only)
}

// [Joel owns] — final variant taxonomy follows ft8_lib + the Field Day message set.
// Requirements this shape must keep: (a) structured, never a raw string the map/QSO re-parse;
// (b) exposes a Locator for the map; (c) Exchange covers grid/report (Standard) AND
// class+section (Field Day).
pub enum ParsedMessage {
    Cq { caller: Callsign, contest: Option<ContestTag>, grid: Option<GridSquare> },
    Exchange { to: Callsign, from: Callsign, payload: ExchangePayload },
    Signoff { to: Callsign, from: Callsign, kind: Signoff }, // RRR / RR73 / 73
    Free(String),
    Raw(String), // decoder produced text it couldn't classify — keep it, don't drop it
}

pub enum ExchangePayload {
    Grid(GridSquare),                          // Standard FT8
    Report(i8),                                // signal report
    RogerReport(i8),                           // R-report
    FieldDay { class: String, section: Section }, // ARRL Field Day exchange
}

pub enum ContestTag { FieldDay, Test, Other(String) }
pub enum Signoff { Rrr, Rr73, Seven3 }

// Enrichment service joins raw decodes against the MERGED logbook (own + gossiped peers),
// so a station worked by your partner already reads as worked here.
pub struct EnrichedDecode {
    pub decode: Decode,
    pub callsign: Option<Callsign>, // pulled out of ParsedMessage for panels
    pub grid: Option<GridSquare>,
    pub worked: WorkedStatus,
}
```

## 4. Rig + operating state, and commands

Split into two single-writer topics: the rig manager owns CAT-level state; the session/mode
layer owns protocol-level state.

```rust
// radio/{id}/rig_state  (State) — written by the rig manager
pub struct RigState {
    pub radio: RadioId,
    pub vfo: AbsHz,
    pub rig_mode: RigMode,
    pub ptt: bool,
    pub meters: Meters,
}
pub struct Meters { pub s_unit: Option<f32>, pub alc: Option<f32>, pub swr: Option<f32> }

// radio/{id}/operating  (State) — written by the session/mode service
pub struct OperatingState {
    pub radio: RadioId,
    pub mode: OverAirMode,
    pub contest: ContestProfile,
    pub band: Band,            // resolved from vfo + mode; published for labeling
}

// radio/{id}/command  (Command) — to the rig manager. Frequency-primitive; NO SetBand.
pub enum RigCommand {
    SetFreq(AbsHz),
    SetRigMode(RigMode),
    PttRequest { on: bool, token: InterlockToken },
}

// session/{id}/command (Command) — to the mode/session service.
// TuneBand is where "/band 20m" lands: resolved to a SetFreq via the active mode's
// calling-frequency table (mode-owned data), then forwarded to the rig.
pub enum SessionCommand {
    SetMode(OverAirMode),
    SetContest(ContestProfile),
    TuneBand(Band), // -> calling freq for (mode, band) -> RigCommand::SetFreq
}

// Mode-owned reference data, not a message. One source of truth for "where FT8 lives on 20m".
// pub fn calling_freq(mode: OverAirMode, band: Band) -> AbsHz
```

## 5. Selection + QSO

```rust
// selection/{id}/active  (State) — which station/decode the operator picked. A gesture,
// not an action: it records WHO is selected (`target`) and WHERE (`context`). The single
// owner of the selection — both the Digital (waterslide) and Contacts (map) panels write
// it via one select command and read it back for their highlight; the Call Sign panel
// and the QSO engine (arm target) read it too. It is NOT the TX offset: the engine owns
// the offset (set via SetTxOffset). The Digital panel is the single operating authority —
// it reads a new selection and places the offset + retunes (map's `AbsFreq` only, and
// only when out-of-passband AND unlocked). The map is a pure select-input.
pub struct Selection {
    pub radio: RadioId,
    pub target: Option<DecodeRef>,          // None = bare-offset / deselect; Some = work that decode
    pub context: Option<SelectionContext>,  // where the selection is (for the offset/retune);
                                            //   None = select-by-call, no known frequency
}
// The freq/offset CONTEXT — where the selected lane/station is, so the Digital panel can
// place the TX offset and decide a retune. Not the TX offset itself (the engine owns that).
pub enum SelectionContext {
    Passband(OffsetHz),  // in-passband lane (waterslide click / bare offset / CLEAR QSY): set offset
    AbsFreq(AbsHz),      // a map pick at a known absolute freq: snap in-passband, else retune (unlocked)
}

// Stable handle so a selection survives the decode batch it came from.
// [Joel owns] the exact key — proposal: (radio, slot, within-slot index) or a DecodeId on Decode.
pub struct DecodeRef { pub radio: RadioId, pub slot: SlotId, pub call: Option<Callsign> }

// qso/{id}/command (Command)  /  qso/{id}/state (State + short history)
pub enum QsoCommand {
    Start { target: DecodeRef },                 // arm: wait for target's next CQ, then answer
    Resume { target: DecodeRef, message: ParsedMessage, snr: i8, offset: OffsetHz }, // pick up
                                                 //   a contact mid-stream from a line addressed
                                                 //   to us (no CQ wait) — clicked `<me> <them> …`
    CallCq,
    Abort,
    SetTxOffset(OffsetHz),                       // move the outgoing TX offset; issued only by the
                                                 //   Digital panel's selection handler (click / digit
                                                 //   / /clear / CLEAR QSY / placed map pick).
                                                 //   Ignored by the engine while locked.
    SetOffsetLock(bool),                         // freeze/unfreeze the TX offset (Tab / LOCKED).
                                                 //   While locked NO offset move happens — not a
                                                 //   write, not auto-QSY. Enforced in the engine.
}

pub struct QsoState {
    pub radio: RadioId,
    pub phase: QsoPhase,
    pub partner: Option<Callsign>,
    pub next_tx: Option<OutgoingMessage>, // what the engine will send next slot
    pub tx_offset: Option<OffsetHz>,      // engine's current TX offset — ALWAYS Some (the engine
                                          //   owns the offset even when idle, so the UI renders the
                                          //   TX lane before a QSO and tracks an auto-QSY hop)
    pub offset_locked: bool,              // whether the operator froze the TX offset (the engine
                                          //   alone enforces it; the UI just renders LOCKED)
}
pub enum QsoPhase { Idle, Calling, InExchange { step: u8 }, Complete, TimedOut }

// The engine builds these from the ContestProfile's template (grid vs class+section).
pub struct OutgoingMessage { pub text: String, pub structured: ParsedMessage }
```

The "send on next interval" timing is the core's job (QSO engine + clock), not the UI's — the
send button emits `QsoCommand` and the panel reflects `QsoState`.

The **TX audio offset has a single owner: the QSO engine** — it's the only component that both
reads the offset (to transmit: `TxIntent.offset` ← `Calling`/`Active` offset) and moves it
autonomously (auto-QSY after unanswered CQs). Every offset move flows through one command —
`SetTxOffset` — issued solely by the Digital panel's selection handler (waterslide click, digit,
`/clear`, CLEAR QSY, and a map pick it placed). `Selection` no longer carries the offset; it only
records the operator's pick. The engine is the one place the lock is enforced: while
`SetOffsetLock(true)`, **no** offset move happens — `SetTxOffset` *or* auto-QSY hop. The UI holds
no offset of its own; it renders `QsoState.tx_offset` / `offset_locked`.

**Selection has a single owner too: the `selection/{id}/active` State.** Both the Digital and
Contacts panels select by writing it (one select command) and read it back for their highlight; the
Digital panel is the single operating authority that turns a *new* selection into the offset/retune
response (the map does none of that). There is no reverse map→waterslide channel and no GUI-local
selection copy.

## 6. TX handoff  —  `radio/{id}/audio_tx` (intent on the bus; codec co-located with rig mgr)

```rust
pub enum TxRequest {
    // FT4/FT8: one message, scheduled to a slot boundary by the rig-side codec + clock.
    SlottedMessage {
        radio: RadioId, mode: OverAirMode, offset: OffsetHz,
        slot: SlotId, message: OutgoingMessage, token: InterlockToken,
    },
    // PSK31/RTTY (architecture only): live stream — open, append, close.
    StreamStart { radio: RadioId, mode: OverAirMode, offset: OffsetHz, token: InterlockToken },
    StreamAppend { radio: RadioId, text: String },
    StreamStop { radio: RadioId },
}

// radio/{id}/tx_report (State) — so the QSO engine learns the outcome (interlock denial,
// PTT failure) instead of assuming the transmission happened.
pub struct TxReport { pub radio: RadioId, pub slot: Option<SlotId>, pub outcome: TxOutcome }
pub enum TxOutcome { Sent, Denied(InterlockError), Failed(String) }

// radio/{id}/tx_log (StreamLossless) — a raw record of every attempted over, for the
// diagnostic archive (the `archive` crate). NOT on `decodes` (the live QSO engine
// consumes that) and richer than `tx_report` (keeps the full message + outcome, so
// even interlock-denied / failed attempts are captured). Capture-only — nothing in
// the operating path reads it.
pub struct TxLogEntry {
    pub radio: RadioId, pub mode: OverAirMode, pub slot: SlotId, pub offset: OffsetHz,
    pub message: OutgoingMessage, pub outcome: TxOutcome, pub t: Timestamp,
}
```

## 7. Logbook  —  `logbook/entries` (StreamLossless + gossiped)

```rust
pub struct LogEntry {
    pub id: QsoId,             // unique per (origin, seq); the G-set merge key
    pub origin: StationId,     // who logged it -> own vs network distinction in the panel
    pub radio: Option<RadioId>,
    pub call: Callsign,
    pub mode: OverAirMode,
    pub band: Band,
    pub freq: AbsHz,
    pub time: Timestamp,
    pub exchange_sent: String,
    pub exchange_rcvd: String,
    pub grid: Option<GridSquare>, // for the map
    // ...remaining ADIF-mappable fields
}
pub struct QsoId { pub origin: StationId, pub seq: u64 }

pub enum WorkedStatus { New, WorkedByMe, WorkedByNetwork(StationId) }

// logbook/worked (State, latest-wins) — the authoritative worked set, owned by the
// single `core::worked` producer. It folds `logbook/entries` through the canonical
// `worked_key(entry, contest) -> (Callsign, Band)` once; the band scanner, the GUI
// Contacts map + waterslide, and the `core::scan` tally all subscribe and read it
// instead of re-deriving the dupe rule. `worked_key` collapses every digital mode per
// band (Field Day / all-digital). Each entry's WorkedStatus carries origin (WorkedByMe
// today; WorkedByNetwork(StationId) once peer logs merge in — the multi-op substrate).
pub struct WorkedSet { pub entries: Vec<WorkedEntry> }
pub struct WorkedEntry { pub call: Callsign, pub band: Band, pub status: WorkedStatus }
```

## 8. Scanner + band activity (Josh-owned)

```rust
// scanner/command (Command) ; scanner/state (State) ; scanner/candidates (State, replace)
pub enum ScannerCommand {
    StartSurvey { stops: Vec<(Band, OverAirMode)>, dwell_slots: u8 }, // each toggle = one stop; dwell >= 2 (parity)
    SetStops { stops: Vec<(Band, OverAirMode)> },                     // change stops live mid-scan (no count reset)
    Cancel,
}
pub enum ScannerAck { Ok } // receipt for a ScannerCommand (scanner/state carries the result)
pub struct ScannerState { pub status: ScanStatus, pub current: Option<Band>, pub current_mode: Option<OverAirMode>, pub last_scan: Option<Timestamp> }
pub enum ScanStatus { Idle, Scanning }

// scanner/candidates carries the full snapshot `Vec<BandActivity>` (one per scanned
// band/mode) as a single State value; cumulative per (band, mode); cq ⊆ heard.
pub struct BandActivity { pub band: Band, pub mode: OverAirMode, pub heard: u32, pub cq: u32, pub unworked: u32, pub t: Timestamp }
```

## 9. Cross-station gossip (Josh-owned)

Single-writer-per-station, last-writer-wins by `seq`, aged by *local receive time*. No central
node; mDNS discovery + periodic full-state push (+ on-change for `working`).

```rust
pub struct StationSnapshot {
    pub station: StationId,
    pub seq: u64,                       // per-writer monotonic; clock-skew-proof
    pub working: Option<WorkingTarget>, // collision-avoidance signal
    pub band_activity: Vec<BandActivity>,
    pub heard: Vec<HeardStation>,       // recency-bounded so it fits a datagram
    // log entries sync as a separate G-set keyed by QsoId
}
pub struct WorkingTarget { pub radio: RadioId, pub band: Band, pub offset: OffsetHz, pub call: Option<Callsign> }
pub struct HeardStation { pub call: Callsign, pub grid: Option<GridSquare>, pub band: Band, pub last_heard: Timestamp, pub snr_db: Option<i8> }
```

## 10. Clock + interlock

```rust
pub struct ClockStatus { pub offset_ms: f32, pub slot_phase: f32, pub slot: SlotId } // clock/status (State); slot = authoritative mode-aware slot id (FT8 15s / FT4 7.5s)

pub struct InterlockToken(pub u64);
pub enum InterlockError { Denied, Expired, NotHolder }
// Request -> Grant{token, ttl} -> PttRequest{token} -> Release. v1 granter always grants
// within TTL; rig manager drops PTT if the token expires (covers client crash).
```

## 11. Topic registry

| Topic | Writer | Type | Class |
|---|---|---|---|
| `radio/{id}/spectrum` | rig mgr / dsp | `SpectrumRow` | StreamLossy |
| `radio/{id}/decodes` | decoder | `Decode` | StreamLossless |
| `radio/{id}/decodes_enriched` | enrichment | `EnrichedDecode` | StreamLossless |
| `logbook/worked` | worked-status producer | `WorkedSet` | State |
| `radio/{id}/rig_state` | rig mgr | `RigState` | State |
| `radio/{id}/operating` | session svc | `OperatingState` | State |
| `radio/{id}/command` | (UI/qso) → rig mgr | `RigCommand` | Command |
| `session/{id}/command` | UI → session svc | `SessionCommand` | Command |
| `radio/{id}/audio_tx` | qso/mode → rig mgr | `TxRequest` | Command/in-proc |
| `radio/{id}/tx_report` | rig mgr | `TxReport` | State |
| `radio/{id}/tx_log` | rig mgr (audio-tx) | `TxLogEntry` | StreamLossless |
| `selection/{id}/active` | UI → all | `Selection` | State |
| `qso/{id}/command` | UI → qso engine | `QsoCommand` | Command |
| `qso/{id}/state` | qso engine | `QsoState` | State (+history) |
| `logbook/entries` | logbook | `LogEntry` | StreamLossless |
| `scanner/command` | UI → scanner | `ScannerCommand` | Command |
| `scanner/state` | scanner | `ScannerState` | State |
| `clock/status` | clock | `ClockStatus` | State |
| `station/{sid}/snapshot` | gossip bridge | `StationSnapshot` | State (gossiped) |

## 12. Open spots to close next

1. **[Joel]** `ParsedMessage` / `ExchangePayload` final variants against `ft8_lib` output and the
   Field Day message set. *(Resolved: CQ carries a 4-char grid in both Standard and Field Day. A
   Field Day **responder** sends only its ARRL/RAC `Section` (no grid), so the map now places those
   from a section → regional-centroid table (`gui::panel_data::section_to_lonlat`) in addition to
   grids. `LogEntry`/`CompletedQso` carry the `Section` so worked Field Day contacts plot too.)*
2. **[Joel/Josh]** `DecodeRef` stability key — `(radio, slot, index)` vs a `DecodeId` minted on
   each `Decode`. Selection and QSO `Start` both depend on it.
3. **[Josh]** Whether `operating` and `rig_state` stay split (clean single-writer) or merge into
   one `RadioState` the core composes (simpler for panels, two writers to reconcile).
4. **[Josh]** Calling-frequency table representation — static per-mode const vs config-loadable
   band plan (config-loadable matters once non-US band plans show up post-open-source).
