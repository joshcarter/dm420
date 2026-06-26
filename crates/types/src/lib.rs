//! Shared message vocabulary for the whole application.
//!
//! Every type that crosses the bus is defined here: the scalar newtypes
//! (`RadioId`, `AbsHz`, `OffsetHz`, `Callsign`, …) and the payload structs/enums
//! from `docs/message-catalog.md`. All derive
//! `Serialize, Deserialize, Clone, Debug, PartialEq` and hold no non-serializable
//! handles — that is the hard rule that keeps the future network transport open.
//!
//! This crate is deliberately dependency-light (serde only) so the pure-compute
//! crates (`dsp`, `modes`, `logbook`) and Joel's `rig`/`modes` work can use the
//! vocabulary without pulling in tokio or the bus.
//!
//! Spec: `docs/message-catalog.md`. Implemented alongside the `bus` task.
//!
//! ## Scope of this first cut
//!
//! Covers catalog §1–§8 and §10 — the rig/decode/QSO/TX seam (Joel's prototype
//! wires into this) plus everything the four GUI panels consume. Catalog §9
//! (cross-station gossip: `StationSnapshot`, `HeardStation`, `WorkingTarget`) is
//! deferred to the networking phase; a marked placeholder sits at §9 below.
//!
//! ## Derive policy
//!
//! Every type derives the catalog's mandated four:
//! `Serialize, Deserialize, Clone, Debug, PartialEq`. As a deliberate, wire-format-
//! neutral enhancement, the **float-free** identifier and scalar newtypes (and the
//! field-less enums) additionally derive `Copy, Eq, Hash` so they work as map keys
//! and routing/merge keys — the catalog names `QsoId` the logbook G-set merge key
//! and `RadioId` a topic routing key, both of which need `Eq + Hash` to be usable.
//! Types carrying an `f32` (`OffsetHz`, `SpectrumRow`, `Decode`, …) keep
//! `PartialEq` only, since `f32` is not `Eq`/`Hash`.
//!
//! Items marked **[Joel owns]** carry the catalog's *draft* shape — concrete enough
//! to compile and wire against, but the final variant taxonomy follows `ft8_lib`
//! output and the Field Day message set and is Joel's call to finalize.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

// =====================================================================
// §0  Shared time helper
// =====================================================================

/// Wall-clock time, milliseconds since the Unix epoch.
///
/// Pure: reads `SystemTime` only — no async, no bus I/O. Shared so every crate
/// stamps timestamps with byte-identical values. Returns `0` if the clock is
/// somehow before the epoch.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// =====================================================================
// §1  Scalar newtypes & enums
// =====================================================================
//
// Cheap wrappers, but they stop the absolute/offset and call/grid mix-ups at
// compile time. The string/integer identifiers add `Eq + Hash` (map/merge keys);
// `OffsetHz` is an `f32` so it stays `PartialEq`-only.

/// Stable per-radio id from config (e.g. `"k1"`). Topic routing key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RadioId(pub String);

/// One operator/core instance — the unit of multi-op gossip.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct StationId(pub String);

/// An amateur callsign, e.g. `"N0JDC"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Callsign(pub String);

impl Callsign {
    /// The canonical comparison form — trimmed and ASCII-upper-cased. The single
    /// place callsign normalization lives, so the worked set, the band-status
    /// aggregate, and the decode enricher all key the same station identically.
    pub fn normalized(&self) -> Callsign {
        Callsign(self.0.trim().to_ascii_uppercase())
    }
}

/// A Maidenhead grid locator, e.g. `"FN31"` / `"DN70KA"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridSquare(pub String);

/// An ARRL/RAC section, e.g. `"CO"`. Carried in the exchange/log, and also a
/// coarse map location for Field Day stations that send no grid (resolved to a
/// regional centroid by the GUI's `panel_data::section_to_lonlat`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Section(pub String);

/// Absolute dial (VFO) frequency in Hz. Distinct type from [`OffsetHz`] so the
/// two can never be mixed; absolute = `vfo + offset`, computed only at the edges.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AbsHz(pub u64);

/// Audio offset within the passband, in Hz (the waterslide's vertical axis).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct OffsetHz(pub f32);

/// UTC milliseconds since the epoch (slot alignment, logging).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timestamp(pub i64);

/// Slot index from the clock module (FT8 = 15 s, FT4 = 7.5 s).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(pub u64);

/// On-air protocol — what the operator means by "mode".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverAirMode {
    Ft8,
    Ft4,
    Psk31,
    Rtty,
}

/// The radio's sideband/data setting. The session layer derives this from the
/// active [`OverAirMode`]; the panel's "Mode" control sets the `OverAirMode`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RigMode {
    UsbData,
    LsbData,
    Usb,
    Lsb,
    Cw,
}

/// Amateur HF/6 m band.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Band {
    B160m,
    B80m,
    B40m,
    B30m,
    B20m,
    B17m,
    B15m,
    B12m,
    B10m,
    B6m,
}

/// Contest exchange profile. Both ship in v1; extend later.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContestProfile {
    Standard,
    ArrlFieldDay,
}

/// Whether a spectrum row / decode is received signal or our own transmission —
/// lets the waterslide render own-TX distinctly.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalSource {
    Received,
    OwnTx,
}

// =====================================================================
// §2  Spectrum  —  radio/{id}/spectrum  (StreamLossy)
// =====================================================================

/// One column of the waterfall: log-scaled FFT magnitudes in audio-offset space.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpectrumRow {
    pub radio: RadioId,
    pub mode: OverAirMode,
    pub t: Timestamp,
    /// Audio offset of bin 0.
    pub bin0_offset: OffsetHz,
    /// Offset step per bin, Hz.
    pub bin_hz: f32,
    /// Log-scaled magnitudes.
    pub mags: Vec<u8>,
    pub source: SignalSource,
}

// =====================================================================
// §3  Decodes  —  radio/{id}/decodes (StreamLossless), …/decodes_enriched
// =====================================================================

/// A single decoded signal. `content` carries the slotted (FT8/FT4) or streaming
/// (PSK31/RTTY) payload — the split that makes PSK31 a sibling, not a rewrite.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Decode {
    pub radio: RadioId,
    pub mode: OverAirMode,
    /// Slot start (slotted) or arrival (streaming).
    pub t: Timestamp,
    /// Audio offset of the signal.
    pub offset: OffsetHz,
    pub snr_db: Option<i8>,
    pub source: SignalSource,
    pub content: DecodeContent,
}

/// Family split for decode payloads. `Streaming` is architecture-only in v1.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum DecodeContent {
    /// FT8/FT4: a structured message anchored to a slot.
    Slotted {
        slot: SlotId,
        dt: f32,
        message: ParsedMessage,
        /// The decoder's raw, undecoded message text (e.g. `"CQ W1ABC FN42"`),
        /// kept verbatim alongside the parse so the two can be compared when a
        /// parse looks wrong. `message` is produced from this same string by
        /// `parse_message`; the decode archive persists both.
        raw: String,
    },
    /// PSK31/RTTY: free-running text (architecture only in v1).
    Streaming { text: String },
}

/// Structured parse of an over-air message.
///
/// **[Joel owns]** — final variant taxonomy follows `ft8_lib` + the Field Day
/// message set. Requirements this shape must keep: (a) structured, never a raw
/// string the map/QSO re-parse; (b) exposes a locator for the map via
/// [`ExchangePayload::Grid`] / [`ParsedMessage::Cq`]; (c) `Exchange` covers grid
/// or report (Standard) *and* class+section (Field Day). `Raw` retains decoder
/// text that couldn't be classified rather than dropping it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ParsedMessage {
    Cq {
        caller: Callsign,
        contest: Option<ContestTag>,
        grid: Option<GridSquare>,
    },
    Exchange {
        to: Callsign,
        from: Callsign,
        payload: ExchangePayload,
    },
    /// RRR / RR73 / 73.
    Signoff {
        to: Callsign,
        from: Callsign,
        kind: Signoff,
    },
    Free(String),
    /// Decoder produced text it couldn't classify — kept, not dropped.
    Raw(String),
}

/// The body of an [`ParsedMessage::Exchange`]. **[Joel owns]** final taxonomy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ExchangePayload {
    /// Standard FT8 grid exchange.
    Grid(GridSquare),
    /// Signal report.
    Report(i8),
    /// R-report.
    RogerReport(i8),
    /// ARRL Field Day exchange (`<count><class> <section>`, e.g. `3A WI`).
    ///
    /// `rogered` marks the `R`-prefixed form (`R 3A WI`) that both rogers the
    /// partner's exchange *and* sends ours — the Field Day analogue of
    /// [`ExchangePayload::Report`] vs [`ExchangePayload::RogerReport`]. The QSO
    /// engine derives the FD transition from this bit (content-driven, per
    /// `docs/wsjtx_qso_sequencing.md` §5), so it must live in the type, not be
    /// re-inferred from a step counter. **[Joel owns]** the final taxonomy.
    FieldDay {
        class: String,
        section: Section,
        rogered: bool,
    },
}

/// Contest tag carried on a CQ. **[Joel owns]** final taxonomy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContestTag {
    FieldDay,
    Test,
    Other(String),
}

/// Sign-off message kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signoff {
    Rrr,
    Rr73,
    Seven3,
}

/// A raw [`Decode`] joined against the merged logbook (own + gossiped peers), so
/// a station worked by your partner already reads as worked here.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EnrichedDecode {
    pub decode: Decode,
    /// Pulled out of `ParsedMessage` for the panels.
    pub callsign: Option<Callsign>,
    pub grid: Option<GridSquare>,
    pub worked: WorkedStatus,
    /// The RF band the decode's slot was received on. The dial isn't on the raw
    /// [`Decode`] (it's audio-domain), so the enricher resolves and stamps it here.
    pub band: Band,
}

// =====================================================================
// §4  Rig + operating state, and commands
// =====================================================================
//
// Two single-writer topics: the rig manager owns CAT-level state; the
// session/mode layer owns protocol-level state.

/// `radio/{id}/rig_state` (State) — written by the rig manager.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RigState {
    pub radio: RadioId,
    pub vfo: AbsHz,
    pub rig_mode: RigMode,
    pub ptt: bool,
    pub meters: Meters,
}

/// Front-panel meter readings.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Meters {
    pub s_unit: Option<f32>,
    pub alc: Option<f32>,
    pub swr: Option<f32>,
}

/// `radio/{id}/operating` (State) — written by the session/mode service.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OperatingState {
    pub radio: RadioId,
    pub mode: OverAirMode,
    pub contest: ContestProfile,
    /// Resolved from `vfo + mode`; published for labeling.
    pub band: Band,
}

/// `radio/{id}/command` (Command) — to the rig manager. Frequency-primitive; there
/// is deliberately **no** `SetBand` (that resolves to a `SetFreq` in the session
/// layer via the active mode's calling-frequency table).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RigCommand {
    SetFreq(AbsHz),
    SetRigMode(RigMode),
    PttRequest { on: bool, token: InterlockToken },
}

/// `session/{id}/command` (Command) — to the mode/session service. `TuneBand` is
/// where "/band 20m" lands: resolved to a `SetFreq` via the active mode's
/// calling-frequency table (mode-owned data), then forwarded to the rig.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum SessionCommand {
    SetMode(OverAirMode),
    SetContest(ContestProfile),
    TuneBand(Band),
}

/// The dial (calling) frequency for a band in a given mode — the **one source of
/// truth** for "where FT8/FT4 lives on each band." FT8 and FT4 have distinct
/// calling frequencies (e.g. 20 m is 14.074 FT8, 14.080 FT4); the architecture-only
/// modes fall back to the FT8 table. `None` for a `(band, mode)` with no established
/// calling frequency (FT4 has none on 160 m). Pure reference data keyed by the
/// shared `Band`/`OverAirMode` vocabulary, so the GUI, the session layer, and the
/// band scanner all resolve a band to a frequency the same way. (Lives here, not in
/// `modes`, because `modes` is the vendored, dependency-free decoder crate and can't
/// see these enums.)
pub fn calling_freq(band: Band, mode: OverAirMode) -> Option<AbsHz> {
    let hz = match mode {
        OverAirMode::Ft4 => match band {
            Band::B160m => return None, // FT4 has no established 160 m calling freq
            Band::B80m => 3_575_000,
            Band::B40m => 7_047_500,
            Band::B30m => 10_140_000,
            Band::B20m => 14_080_000,
            Band::B17m => 18_104_000,
            Band::B15m => 21_140_000,
            Band::B12m => 24_919_000,
            Band::B10m => 28_180_000,
            Band::B6m => 50_318_000,
        },
        // FT8, and a sensible default for the architecture-only modes.
        OverAirMode::Ft8 | OverAirMode::Psk31 | OverAirMode::Rtty => match band {
            Band::B160m => 1_840_000,
            Band::B80m => 3_573_000,
            Band::B40m => 7_074_000,
            Band::B30m => 10_136_000,
            Band::B20m => 14_074_000,
            Band::B17m => 18_100_000,
            Band::B15m => 21_074_000,
            Band::B12m => 24_915_000,
            Band::B10m => 28_074_000,
            Band::B6m => 50_313_000,
        },
    };
    Some(AbsHz(hz))
}

/// The six ARRL Field Day HF bands, longest-wavelength first: the universe the
/// operator's "active bands" selector picks from, the set the band-status producer
/// tracks, and the default when no `[bands] list` is configured. The single home
/// for this list so the selector, scanner, and band-status agree by construction.
pub const HF_BANDS: [Band; 6] = [
    Band::B160m,
    Band::B80m,
    Band::B40m,
    Band::B20m,
    Band::B15m,
    Band::B10m,
];

/// The default Field Day HF stop set: the six [`HF_BANDS`]
/// (160/80/40/20/15/10 m) × {FT8, FT4}, filtered to stops that have an established
/// calling frequency (FT4 has none on 160 m, so that stop drops — 11 total). The
/// single home for the default band/mode list, so the scanner's `StartSurvey` and
/// the band-status retention window agree by construction.
pub fn field_day_stops() -> Vec<(Band, OverAirMode)> {
    const MODES: [OverAirMode; 2] = [OverAirMode::Ft8, OverAirMode::Ft4];
    HF_BANDS
        .iter()
        .flat_map(|&b| MODES.iter().map(move |&m| (b, m)))
        .filter(|&(b, m)| calling_freq(b, m).is_some())
        .collect()
}

impl Band {
    /// Classify a dial/VFO frequency into its amateur band — the inverse of
    /// [`calling_freq`]. Returns `None` for a frequency outside the HF/6 m
    /// amateur allocations (e.g. in a gap between bands).
    pub fn from_hz(freq: AbsHz) -> Option<Band> {
        Some(match freq.0 {
            1_800_000..=2_000_000 => Band::B160m,
            3_500_000..=4_000_000 => Band::B80m,
            7_000_000..=7_300_000 => Band::B40m,
            10_100_000..=10_150_000 => Band::B30m,
            14_000_000..=14_350_000 => Band::B20m,
            18_068_000..=18_168_000 => Band::B17m,
            21_000_000..=21_450_000 => Band::B15m,
            24_890_000..=24_990_000 => Band::B12m,
            28_000_000..=29_700_000 => Band::B10m,
            50_000_000..=54_000_000 => Band::B6m,
            _ => return None,
        })
    }
}

// =====================================================================
// §5  Selection + QSO
// =====================================================================

/// `selection/{id}/active` (State) — which station/decode the operator has selected.
/// A *gesture*, not an operating action: it records **who** is selected (`target`) and
/// **where** they are (`context`). It is the single owner of the selection, written by
/// both the Digital (waterslide) and Contacts (map) panels via one select command and
/// read back by both for their highlight, by the Call Sign panel, and by the QSO
/// engine for the arm target.
///
/// The Digital panel is the **single operating authority**: it turns a *new* selection
/// into operating actions — set the TX offset (via [`QsoCommand::SetTxOffset`], which
/// the engine gates on the lock) and retune the dial, but only when the station is
/// outside the current passband *and* the offset is unlocked. The map is a pure
/// select-input: it emits a selection and does nothing else (no offset, no retune, no
/// lock awareness).
///
/// `target == None` is a bare-offset / deselect gesture (a click in empty spectrum, or
/// CLEAR QSY); `Some` is intent to work that decode. The TX offset itself is **not**
/// carried here — the QSO engine owns it; this only carries the freq/offset *context*
/// the Digital panel needs to place it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Selection {
    pub radio: RadioId,
    /// The selected decode (the station to work), or `None` for a bare-offset gesture
    /// / deselect.
    pub target: Option<DecodeRef>,
    /// Where the selection is, so the Digital panel can place the TX offset and decide
    /// a retune. `None` = select-by-call with no known frequency (a worked-only map
    /// spot): select the station, move nothing.
    pub context: Option<SelectionContext>,
}

/// The freq/offset *context* of a [`Selection`] — where the selected lane/station is,
/// for the Digital panel to set the TX offset and decide whether to retune the dial.
/// Deliberately **not** the TX offset: the QSO engine owns the offset (set via
/// [`QsoCommand::SetTxOffset`], lock-enforced); this only says where the operator
/// pointed so the single operating handler can react.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum SelectionContext {
    /// A lane already inside the current passband — a waterslide click, a bare-offset
    /// gesture, or CLEAR QSY. The Digital panel sets the TX offset here; no retune.
    Passband(OffsetHz),
    /// A station at a known absolute frequency — a Contacts-map pick (the map knows
    /// where it was heard/logged, but has no passband awareness). The Digital panel
    /// computes the in-passband offset, or — when unlocked — retunes the dial so the
    /// station lands mid-passband.
    AbsFreq(AbsHz),
}

/// Stable handle so a selection survives the decode batch it came from.
///
/// **[Joel/Josh own]** the exact stability key — proposal here is
/// `(radio, slot, call)`; the alternative is a `DecodeId` minted on each
/// [`Decode`]. Selection and `QsoCommand::Start` both depend on this choice.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecodeRef {
    pub radio: RadioId,
    pub slot: SlotId,
    pub call: Option<Callsign>,
}

/// `qso/{id}/command` (Command) — to the QSO engine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum QsoCommand {
    /// Arm to a target the DM420 way: receive-only until it next calls CQ, then
    /// answer (`docs/qso_flow.md` §4).
    Start { target: DecodeRef },
    /// Pick up a contact *mid-stream* from a decode addressed to us
    /// (`<my call> <their call> …`) — the operator clicked a line that answers a
    /// call we'd already disarmed from. Unlike [`QsoCommand::Start`], the engine
    /// commits at once, deriving our role and the reply from `message` rather than
    /// waiting for a CQ that won't come. `snr` is our report of them and `offset`
    /// the audio offset to answer on; both come from the clicked decode.
    Resume {
        target: DecodeRef,
        message: ParsedMessage,
        snr: i8,
        offset: OffsetHz,
    },
    CallCq,
    Abort,
    /// Set the engine's outgoing TX audio offset (the waterslide click / `/clear` /
    /// map-pick gesture). The engine is the **sole owner** of the offset and ignores
    /// this while the offset is locked, so the operator's writes and the engine's own
    /// auto-QSY obey one rule enforced in one place (`engine::on_command`).
    SetTxOffset(OffsetHz),
    /// Freeze / unfreeze the TX offset. While locked, **no** offset move happens —
    /// not [`QsoCommand::SetTxOffset`], not auto-QSY. (The Tab key / LOCKED button.)
    SetOffsetLock(bool),
}

/// `qso/{id}/state` (State + short history) — written by the QSO engine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QsoState {
    pub radio: RadioId,
    pub phase: QsoPhase,
    pub partner: Option<Callsign>,
    /// What the engine will send next slot.
    pub next_tx: Option<OutgoingMessage>,
    /// The engine's current TX audio offset — **always `Some`** (the engine owns the
    /// offset even when idle, so the UI can render the TX lane before a QSO and track
    /// an auto-QSY hop the operator didn't set by hand).
    pub tx_offset: Option<OffsetHz>,
    /// Whether the TX offset is locked (the operator froze it). Mirrors the engine's
    /// owned lock so the UI renders the LOCKED state and the engine alone enforces it.
    pub offset_locked: bool,
}

/// Phase of an in-progress contact.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QsoPhase {
    Idle,
    /// DM420's wait-for-CQ state: armed to a target, receive-only until it calls
    /// CQ (`docs/qso_flow.md` §4). No WSJT-X equivalent — WSJT-X replies at once.
    Armed,
    Calling,
    InExchange {
        step: u8,
    },
    Complete,
    TimedOut,
}

/// A message the QSO engine intends to transmit. The engine builds these from the
/// [`ContestProfile`]'s template (grid vs class+section).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OutgoingMessage {
    pub text: String,
    pub structured: ParsedMessage,
}

// =====================================================================
// §6  TX handoff  —  radio/{id}/audio_tx (Command), radio/{id}/tx_report (State)
// =====================================================================

/// Intent to transmit, placed on the bus; the codec is co-located with the rig
/// manager. Slotted for FT4/FT8; the stream variants are architecture-only in v1.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TxRequest {
    /// FT4/FT8: one message, scheduled to a slot boundary by the rig-side codec.
    SlottedMessage {
        radio: RadioId,
        mode: OverAirMode,
        offset: OffsetHz,
        slot: SlotId,
        message: OutgoingMessage,
        token: InterlockToken,
    },
    /// PSK31/RTTY (architecture only): open a live stream.
    StreamStart {
        radio: RadioId,
        mode: OverAirMode,
        offset: OffsetHz,
        token: InterlockToken,
    },
    StreamAppend {
        radio: RadioId,
        text: String,
    },
    StreamStop {
        radio: RadioId,
    },
}

/// `radio/{id}/tx_report` (State) — so the QSO engine learns the outcome
/// (interlock denial, PTT failure) instead of assuming the transmission happened.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TxReport {
    pub radio: RadioId,
    pub slot: Option<SlotId>,
    pub outcome: TxOutcome,
}

/// Result of a transmission attempt.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TxOutcome {
    Sent,
    Denied(InterlockError),
    Failed(String),
}

/// Reply to a [`TxRequest`] on `radio/{id}/audio_tx` — receipt only. The real
/// result (sent/denied/failed) is reported separately on `radio/{id}/tx_report`;
/// a client awaiting this ack learns the over has finished and can then release
/// its interlock token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TxAck {
    Accepted,
}

/// `radio/{id}/tx_log` (StreamLossless) — a record of every over the audio-TX
/// path attempted, for the raw diagnostic archive (the `archive` crate).
/// Deliberately **not** on `Decodes` (the live QSO engine consumes that) and
/// richer than [`TxReport`] (which is State/last-value and carries no message
/// text): this keeps the full intended message plus the `outcome`, so even
/// interlock-denied / failed attempts are captured. Never read by the operating
/// path — capture-only.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TxLogEntry {
    pub radio: RadioId,
    pub mode: OverAirMode,
    /// The slot the over was scheduled to.
    pub slot: SlotId,
    /// Audio offset the over was sent at (the dial/VFO is on `RigState`).
    pub offset: OffsetHz,
    /// The message we intended to send (on-air text + structured parse).
    pub message: OutgoingMessage,
    /// What actually happened: sent / interlock-denied / failed.
    pub outcome: TxOutcome,
    /// Wall-clock (epoch ms) when the over was attempted (≈ slot boundary).
    pub t: Timestamp,
}

// =====================================================================
// §7  Logbook  —  logbook/entries  (StreamLossless + gossiped)
// =====================================================================

/// A logged contact. `id` is the G-set merge key; `origin` drives the own-vs-
/// network distinction in the panel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub id: QsoId,
    pub radio: Option<RadioId>,
    pub call: Callsign,
    pub mode: OverAirMode,
    pub band: Band,
    pub freq: AbsHz,
    pub time: Timestamp,
    pub exchange_sent: String,
    pub exchange_rcvd: String,
    /// For the map.
    pub grid: Option<GridSquare>,
    /// For the map: the ARRL/RAC section from a Field Day exchange. A coarser
    /// location source than `grid`, used to place a contact that carried no grid
    /// (Field Day responders send only their section). `#[serde(default)]` keeps
    /// logs written before this field was added loadable.
    #[serde(default)]
    pub section: Option<Section>,
    // ...remaining ADIF-mappable fields land here as the logbook crate grows.
}

/// Unique per `(origin, seq)` — the logbook G-set merge key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct QsoId {
    pub origin: StationId,
    pub seq: u64,
}

/// Whether a station has been worked, and by whom (own vs network).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WorkedStatus {
    New,
    WorkedByMe,
    WorkedByNetwork(StationId),
}

/// The canonical key under which a logged contact counts as "worked" — the single
/// place the dupe rule lives.
///
/// Worked-status is re-derived in several consumers (the band scanner's
/// `worked: HashSet<(Callsign, Band)>`, the Contacts map in `gui::bus_view`, the
/// `core::scan` shell) with subtly divergent keys — some upper-case the callsign,
/// some don't. This function is the canonical definition they should converge on;
/// it is **additive and not yet consumed** (the consumer migration is a separate,
/// supervised step because it changes visible dupe behavior).
///
/// The key is `(call, band)` with the callsign normalized (trimmed + ASCII
/// upper-cased), deliberately dropping the [`OverAirMode`]: under ARRL Field Day —
/// and for this all-digital app generally — every digital mode collapses into a
/// single "digital" mode, so a station worked on 20 m FT8 is a dupe on 20 m FT4.
/// Worked-ness is **per band**, so the same station on another band is a new
/// contact.
///
/// `contest` selects the rule in one place. Both shipping profiles collapse modes
/// today; keeping the parameter means a future per-mode award view (e.g. "WAS on
/// FT8") is a change here rather than a literal scattered across consumers.
pub fn worked_key(entry: &LogEntry, contest: ContestProfile) -> (Callsign, Band) {
    let call = entry.call.normalized();
    match contest {
        // Field Day collapses every digital mode into one; the Standard profile we
        // run is likewise all-digital today, so both key on `(call, band)` alone.
        ContestProfile::ArrlFieldDay | ContestProfile::Standard => (call, entry.band),
    }
}

/// `logbook/worked` (State) — the authoritative worked set, owned by the single
/// `core::worked` producer.
///
/// A latest-wins snapshot of every `(callsign, band)` that counts as worked under
/// [`worked_key`]: Field Day (and this all-digital app) collapses every digital mode
/// per band, so a station worked on 20 m FT8 is a dupe on 20 m FT4, while the same
/// call on another band is a new contact. The producer folds `logbook/entries`
/// through `worked_key` *once*; every consumer — the band scanner, the GUI Contacts
/// map + waterslide, the `core::scan` tally — subscribes and reads this instead of
/// re-deriving the dupe rule with its own (previously divergent) key.
///
/// Each entry's [`WorkedStatus`] carries the origin dimension: `WorkedByMe` today
/// (every logged contact originates locally), becoming `WorkedByNetwork(StationId)`
/// once peer logs merge in over `logbook/entries` (networking — the multi-op
/// substrate, not built yet). Entry order is unspecified; consumers treat it as a set.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkedSet {
    pub entries: Vec<WorkedEntry>,
}

/// One `(callsign, band)` in a [`WorkedSet`], with how it was worked. `call` is
/// normalized exactly as [`worked_key`] returns it (trimmed + ASCII upper-cased).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkedEntry {
    pub call: Callsign,
    pub band: Band,
    pub status: WorkedStatus,
}

impl WorkedSet {
    /// Whether `(call, band)` has been worked. `call` is matched case-insensitively
    /// against the normalized keys, so a caller needn't pre-upper-case the lookup.
    pub fn is_worked(&self, call: &Callsign, band: Band) -> bool {
        self.entries
            .iter()
            .any(|e| e.band == band && e.call.0.eq_ignore_ascii_case(&call.0))
    }
}

// =====================================================================
// §8  Scanner + band activity
// =====================================================================

/// `scanner/command` (Command) — to the band scanner.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ScannerCommand {
    /// Survey the given `(band, mode)` stops — each band/mode panel toggle is one
    /// stop — dwelling `dwell_slots` per stop. `dwell_slots >= 2` (even/odd parity).
    StartSurvey {
        stops: Vec<(Band, OverAirMode)>,
        dwell_slots: u8,
    },
    /// Replace the live sweep's stops mid-scan (the panel's band/mode toggles)
    /// **without** resetting the accumulated counts. Ignored when not scanning.
    SetStops {
        stops: Vec<(Band, OverAirMode)>,
    },
    Cancel,
}

/// Reply to a [`ScannerCommand`] — receipt only; the real outcome shows up as a new
/// [`ScannerState`] on `scanner/state`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScannerAck {
    Ok,
}

/// `scanner/state` (State) — written by the scanner.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScannerState {
    pub status: ScanStatus,
    pub current: Option<Band>,
    /// The mode being dwelled on right now (pairs with `current`); `None` when idle.
    pub current_mode: Option<OverAirMode>,
    pub last_scan: Option<Timestamp>,
}

/// Scanner run state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStatus {
    Idle,
    Scanning,
}

/// Per-(band, mode) scan counts feeding the band-scan panel and (later) the
/// cross-station `band_activity` gossip. Counts are cumulative over the scan
/// (distinct callsigns). `cq` (stations that called CQ) is a subset of `heard`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BandActivity {
    pub band: Band,
    pub mode: OverAirMode,
    /// Distinct stations heard transmitting.
    pub heard: u32,
    /// Distinct stations heard calling CQ (a subset of `heard`).
    pub cq: u32,
    /// Distinct heard stations not yet logged on this band + mode.
    pub unworked: u32,
    pub t: Timestamp,
}

/// `band/status` (State) — the always-on band-activity aggregate written by the
/// `core::band_status` producer. Unlike [`BandActivity`] (the scanner's per-sweep
/// gossip, populated only while a survey runs), this is a rolling window over
/// *every* decode source (local receive, scanning, and peers) for the configured
/// `(band, mode)` stops, so the read-only Band Status panel shows what's active
/// with no scan running.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BandStatus {
    pub rows: Vec<BandStatusRow>,
    pub t: Timestamp,
}

/// One configured `(band, mode)`'s rolling counts of distinct stations over the
/// retention window. `cq` and `unworked` are each subsets of `heard`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BandStatusRow {
    pub band: Band,
    pub mode: OverAirMode,
    /// Distinct stations heard (mine ∪ peers).
    pub heard: u32,
    /// Distinct stations heard calling CQ (a subset of `heard`).
    pub cq: u32,
    /// Distinct heard stations not worked on this band (a subset of `heard`).
    pub unworked: u32,
}

// =====================================================================
// §9  Cross-station gossip  —  station/{id}/snapshot  (State + gossiped)
// =====================================================================
//
// The LAN-sharing vocabulary (Josh-owned networking phase). Carried over UDP by
// the `net` crate and re-published onto the bus; see `docs/networking.md` for the
// transport, merge semantics, and anti-entropy loop. The log G-set itself reuses
// §7 `LogEntry`/`QsoId`; only the beacon types live here.

/// One operator's current working intent — what peers consume to avoid competing
/// for the same contact. Set the moment we arm to a station or commit to a caller;
/// cleared to `None` when the engine drops to `Idle`. Carries an `OffsetHz` (`f32`)
/// so it stays `PartialEq`-only, like the rest of the float-carrying payloads.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkingTarget {
    pub radio: RadioId,
    pub band: Band,
    pub offset: OffsetHz,
    /// The peer's dial (center) frequency the `offset` is measured against. Peers
    /// on the same band may sit on different dials; a consumer must re-base the
    /// offset onto its own dial (`offset + peer.dial - my.dial`) before placing it,
    /// or two stations 1 kHz apart but both at the same audio offset would collide.
    pub dial: AbsHz,
    /// The target station, once known (unknown while merely armed to a frequency).
    pub call: Option<Callsign>,
}

/// A station we've heard (decoded) but not necessarily worked. Shared so the map
/// and band-scan aggregate everyone's ears, not just ours. `last_heard` is aged by
/// the *receiver's* local clock on receipt, never the sender's — immune to skew.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HeardStation {
    pub call: Callsign,
    pub grid: Option<GridSquare>,
    pub band: Band,
    pub mode: OverAirMode,
    pub snr: i8,
    pub last_heard: Timestamp,
}

/// Periodic full-state beacon, latest-wins per station by `seq` (the State topic
/// `station/{id}/snapshot`). Carries everything except the log G-set, which syncs
/// separately via the `net` anti-entropy loop. `heard` is recency-bounded by the
/// sender so a snapshot fits one datagram.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StationSnapshot {
    pub station: StationId,
    /// Monotonic per station — supersedes any lower-`seq` snapshot. Never a
    /// wall-clock, so clock skew between operators can't reorder beacons.
    pub seq: u64,
    pub working: Option<WorkingTarget>,
    pub band_activity: Vec<BandActivity>,
    pub heard: Vec<HeardStation>,
}

// =====================================================================
// §10  Clock + interlock
// =====================================================================

/// `clock/status` (State) — clock health for the UI's sync indicator *and* the
/// authoritative slot identity for sequencing. The clock module owns the
/// mode-aware slot period (FT8 = 15 s, FT4 = 7.5 s), so consumers must read
/// [`slot`](Self::slot) from here rather than recompute it from the wall clock —
/// that keeps the QSO tick parity commensurate with the decode pipeline's slot
/// numbering (mismatched periods were the FT4 TX-window bug).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ClockStatus {
    pub offset_ms: f32,
    pub slot_phase: f32,
    /// The slot the clock is currently in (mode-aware period).
    pub slot: SlotId,
    /// The active on-air mode the clock derives its period from. Consumers (the
    /// QSO engine) read this as the authoritative current mode — correct even
    /// before the first decode, so an FT4 CQ-first over is synthesized as FT4.
    pub mode: OverAirMode,
}

/// A PTT interlock grant. Flow: `Request -> Grant{token, ttl} -> PttRequest{token}
/// -> Release`. The v1 granter always grants within TTL; the rig manager drops PTT
/// if the token expires (covers a client crash).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InterlockToken(pub u64);

/// Why an interlock-guarded action was refused.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterlockError {
    Denied,
    Expired,
    NotHolder,
}

/// `interlock/{id}` (Command) — a TX client asks the granter for, or returns, the
/// PTT token. The granter enforces a single live holder; the grant self-expires at
/// its TTL so a crashed client can't wedge the transmitter.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterlockRequest {
    /// Request the PTT token (granted only if no live holder exists).
    Acquire,
    /// Return the token early (otherwise it lapses at TTL).
    Release(InterlockToken),
}

/// Reply to an [`InterlockRequest`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterlockReply {
    /// Token granted; valid for `ttl_ms` from the moment of the grant.
    Granted { token: InterlockToken, ttl_ms: u64 },
    /// Acquisition refused (someone else holds a live token).
    Denied(InterlockError),
    /// Release acknowledged.
    Released,
}

// =====================================================================
// §12  Subsystem health
// =====================================================================
//
// Hardware-backed producers (rig, audio capture) report their liveness here so
// the UI can surface a fault where live data would otherwise be — and so a
// missing or disconnected device degrades gracefully instead of taking the app
// down. Each subsystem owns one `health/{id}` State topic (latest-wins).

/// A hardware-backed subsystem whose liveness the UI surfaces. One `health/{id}`
/// topic per variant. The decode pipeline rides under [`SubsystemId::Audio`],
/// since its health is the capture device's health.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubsystemId {
    Rig,
    Audio,
}

impl SubsystemId {
    /// The canonical topic-path segment, e.g. `"rig"`.
    pub fn as_str(self) -> &'static str {
        match self {
            SubsystemId::Rig => "rig",
            SubsystemId::Audio => "audio",
        }
    }

    /// Inverse of [`SubsystemId::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rig" => Some(SubsystemId::Rig),
            "audio" => Some(SubsystemId::Audio),
            _ => None,
        }
    }
}

/// A subsystem's current liveness. `Degraded`/`Down` carry a short human reason
/// for the UI to show (e.g. `"device 'USB PnP' not found"`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HealthState {
    /// Connected and delivering data.
    Healthy,
    /// Running, but impaired (e.g. reconnecting, or a soft fault). The reason is
    /// shown to the operator.
    Degraded(String),
    /// Not delivering data (device missing, open failed, disconnected). The
    /// producer keeps retrying; the reason is shown to the operator.
    Down(String),
}

/// `health/{id}` (State) — a subsystem's liveness for the UI's fault display.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubsystemHealth {
    pub id: SubsystemId,
    pub state: HealthState,
    /// When this state began (UTC ms). Lets the UI show "down for 12s".
    pub since: Timestamp,
}

impl SubsystemHealth {
    /// True when the subsystem is not delivering data (`Down` or `Degraded`).
    pub fn is_faulted(&self) -> bool {
        !matches!(self.state, HealthState::Healthy)
    }

    /// The operator-facing reason, if the subsystem is faulted.
    pub fn reason(&self) -> Option<&str> {
        match &self.state {
            HealthState::Healthy => None,
            HealthState::Degraded(m) | HealthState::Down(m) => Some(m.as_str()),
        }
    }
}

// =====================================================================
// Tests — acceptance criterion #1: serde round-trip for every payload type.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize to JSON and back; assert the value survives unchanged.
    fn round_trip<T>(value: T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(&value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, back, "round-trip mismatch for JSON: {json}");
    }

    fn sample_decode() -> Decode {
        Decode {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Ft8,
            t: Timestamp(1_700_000_000_000),
            offset: OffsetHz(1500.0),
            snr_db: Some(-12),
            source: SignalSource::Received,
            content: DecodeContent::Slotted {
                slot: SlotId(42),
                dt: 0.2,
                message: ParsedMessage::Cq {
                    caller: Callsign("N0JDC".into()),
                    contest: Some(ContestTag::FieldDay),
                    grid: Some(GridSquare("DN70".into())),
                },
                raw: "CQ N0JDC DN70".into(),
            },
        }
    }

    fn sample_outgoing() -> OutgoingMessage {
        OutgoingMessage {
            text: "N0JDC W4LL R-09".into(),
            structured: ParsedMessage::Exchange {
                to: Callsign("N0JDC".into()),
                from: Callsign("W4LL".into()),
                payload: ExchangePayload::RogerReport(-9),
            },
        }
    }

    #[test]
    fn scalar_newtypes_round_trip() {
        round_trip(RadioId("k1".into()));
        round_trip(StationId("station-a".into()));
        round_trip(Callsign("N0JDC".into()));
        round_trip(GridSquare("DN70KA".into()));
        round_trip(Section("CO".into()));
        round_trip(AbsHz(14_074_000));
        round_trip(OffsetHz(1234.5));
        round_trip(Timestamp(1_700_000_000_000));
        round_trip(SlotId(7));
        for m in [
            OverAirMode::Ft8,
            OverAirMode::Ft4,
            OverAirMode::Psk31,
            OverAirMode::Rtty,
        ] {
            round_trip(m);
        }
        for m in [
            RigMode::UsbData,
            RigMode::LsbData,
            RigMode::Usb,
            RigMode::Lsb,
            RigMode::Cw,
        ] {
            round_trip(m);
        }
        for b in [Band::B160m, Band::B20m, Band::B6m] {
            round_trip(b);
        }
        round_trip(ContestProfile::Standard);
        round_trip(ContestProfile::ArrlFieldDay);
        round_trip(SignalSource::Received);
        round_trip(SignalSource::OwnTx);
    }

    #[test]
    fn band_from_hz_classifies_and_round_trips() {
        // Every band's standard FT8/FT4 calling frequency classifies back to it.
        for b in [
            Band::B160m,
            Band::B80m,
            Band::B40m,
            Band::B30m,
            Band::B20m,
            Band::B17m,
            Band::B15m,
            Band::B12m,
            Band::B10m,
            Band::B6m,
        ] {
            for m in [OverAirMode::Ft8, OverAirMode::Ft4] {
                if let Some(f) = calling_freq(b, m) {
                    assert_eq!(Band::from_hz(f), Some(b), "{b:?} {m:?} @ {}", f.0);
                }
            }
        }
        assert_eq!(Band::from_hz(AbsHz(14_074_000)), Some(Band::B20m));
        assert_eq!(Band::from_hz(AbsHz(7_000_000)), Some(Band::B40m)); // lower edge
        assert_eq!(Band::from_hz(AbsHz(7_300_000)), Some(Band::B40m)); // upper edge
        assert_eq!(Band::from_hz(AbsHz(5_000_000)), None); // between 80 m and 40 m
        assert_eq!(Band::from_hz(AbsHz(100_000_000)), None); // above 6 m
    }

    #[test]
    fn calling_freq_is_mode_specific() {
        // FT8 and FT4 sit on different calling frequencies on the same band.
        assert_eq!(
            calling_freq(Band::B20m, OverAirMode::Ft8),
            Some(AbsHz(14_074_000))
        );
        assert_eq!(
            calling_freq(Band::B20m, OverAirMode::Ft4),
            Some(AbsHz(14_080_000))
        );
        // FT4 has no established 160 m calling frequency; FT8 does.
        assert_eq!(calling_freq(Band::B160m, OverAirMode::Ft4), None);
        assert_eq!(
            calling_freq(Band::B160m, OverAirMode::Ft8),
            Some(AbsHz(1_840_000))
        );
    }

    #[test]
    fn spectrum_round_trip() {
        round_trip(SpectrumRow {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Ft8,
            t: Timestamp(1),
            bin0_offset: OffsetHz(0.0),
            bin_hz: 6.25,
            mags: vec![0, 64, 128, 255],
            source: SignalSource::Received,
        });
    }

    #[test]
    fn tx_log_round_trip() {
        round_trip(TxLogEntry {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Ft8,
            slot: SlotId(42),
            offset: OffsetHz(1500.0),
            message: OutgoingMessage {
                text: "CQ W4LL EM74".into(),
                structured: ParsedMessage::Cq {
                    caller: Callsign("W4LL".into()),
                    contest: None,
                    grid: Some(GridSquare("EM74".into())),
                },
            },
            outcome: TxOutcome::Sent,
            t: Timestamp(1_700_000_000_000),
        });
    }

    #[test]
    fn decode_family_round_trip() {
        round_trip(sample_decode());
        // Streaming content variant.
        round_trip(Decode {
            content: DecodeContent::Streaming {
                text: "hello psk".into(),
            },
            ..sample_decode()
        });
        // Each ParsedMessage variant.
        round_trip(ParsedMessage::Signoff {
            to: Callsign("N0JDC".into()),
            from: Callsign("W4LL".into()),
            kind: Signoff::Rr73,
        });
        round_trip(ParsedMessage::Free("TNX 73".into()));
        round_trip(ParsedMessage::Raw("?? garbled ??".into()));
        // Each ExchangePayload variant.
        round_trip(ExchangePayload::Grid(GridSquare("FN31".into())));
        round_trip(ExchangePayload::Report(-15));
        round_trip(ExchangePayload::RogerReport(3));
        round_trip(ExchangePayload::FieldDay {
            class: "2A".into(),
            section: Section("CO".into()),
            rogered: false,
        });
        round_trip(ExchangePayload::FieldDay {
            class: "3A".into(),
            section: Section("WI".into()),
            rogered: true,
        });
        round_trip(EnrichedDecode {
            decode: sample_decode(),
            callsign: Some(Callsign("N0JDC".into())),
            grid: Some(GridSquare("DN70".into())),
            worked: WorkedStatus::WorkedByNetwork(StationId("peer-1".into())),
            band: Band::B20m,
        });
        round_trip(WorkedSet::default());
        round_trip(WorkedSet {
            entries: vec![
                WorkedEntry {
                    call: Callsign("W1ABC".into()),
                    band: Band::B20m,
                    status: WorkedStatus::WorkedByMe,
                },
                WorkedEntry {
                    call: Callsign("K2DEF".into()),
                    band: Band::B40m,
                    status: WorkedStatus::WorkedByNetwork(StationId("peer-1".into())),
                },
            ],
        });
    }

    #[test]
    fn rig_and_session_round_trip() {
        round_trip(RigState {
            radio: RadioId("k1".into()),
            vfo: AbsHz(14_074_000),
            rig_mode: RigMode::UsbData,
            ptt: false,
            meters: Meters {
                s_unit: Some(5.0),
                alc: None,
                swr: Some(1.2),
            },
        });
        round_trip(OperatingState {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Ft8,
            contest: ContestProfile::ArrlFieldDay,
            band: Band::B20m,
        });
        round_trip(RigCommand::SetFreq(AbsHz(7_074_000)));
        round_trip(RigCommand::SetRigMode(RigMode::LsbData));
        round_trip(RigCommand::PttRequest {
            on: true,
            token: InterlockToken(99),
        });
        round_trip(SessionCommand::SetMode(OverAirMode::Ft4));
        round_trip(SessionCommand::SetContest(ContestProfile::Standard));
        round_trip(SessionCommand::TuneBand(Band::B15m));
    }

    #[test]
    fn selection_and_qso_round_trip() {
        round_trip(Selection {
            radio: RadioId("k1".into()),
            target: Some(DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(42),
                call: Some(Callsign("N0JDC".into())),
            }),
            context: Some(SelectionContext::Passband(OffsetHz(1500.0))),
        });
        // A map pick carries an absolute frequency; a bare-offset / select-by-call
        // gesture carries no context.
        round_trip(Selection {
            radio: RadioId("k1".into()),
            target: Some(DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(3),
                call: Some(Callsign("W4LL".into())),
            }),
            context: Some(SelectionContext::AbsFreq(AbsHz(14_075_500))),
        });
        round_trip(Selection {
            radio: RadioId("k1".into()),
            target: None,
            context: None,
        });
        round_trip(QsoCommand::Start {
            target: DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(42),
                call: None,
            },
        });
        round_trip(QsoCommand::Resume {
            target: DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(7),
                call: Some(Callsign("K1ABC".into())),
            },
            message: ParsedMessage::Exchange {
                to: Callsign("N0JDC".into()),
                from: Callsign("K1ABC".into()),
                payload: ExchangePayload::Report(-12),
            },
            snr: -5,
            offset: OffsetHz(1200.0),
        });
        round_trip(QsoCommand::CallCq);
        round_trip(QsoCommand::Abort);
        round_trip(QsoCommand::SetTxOffset(OffsetHz(1234.0)));
        round_trip(QsoCommand::SetOffsetLock(true));
        round_trip(QsoCommand::SetOffsetLock(false));
        round_trip(QsoState {
            radio: RadioId("k1".into()),
            phase: QsoPhase::InExchange { step: 2 },
            partner: Some(Callsign("W4LL".into())),
            next_tx: Some(sample_outgoing()),
            tx_offset: Some(OffsetHz(1500.0)),
            offset_locked: true,
        });
    }

    #[test]
    fn tx_round_trip() {
        round_trip(TxRequest::SlottedMessage {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Ft8,
            offset: OffsetHz(1500.0),
            slot: SlotId(42),
            message: sample_outgoing(),
            token: InterlockToken(7),
        });
        round_trip(TxRequest::StreamStart {
            radio: RadioId("k1".into()),
            mode: OverAirMode::Psk31,
            offset: OffsetHz(1000.0),
            token: InterlockToken(8),
        });
        round_trip(TxRequest::StreamAppend {
            radio: RadioId("k1".into()),
            text: "CQ CQ".into(),
        });
        round_trip(TxRequest::StreamStop {
            radio: RadioId("k1".into()),
        });
        round_trip(TxReport {
            radio: RadioId("k1".into()),
            slot: Some(SlotId(42)),
            outcome: TxOutcome::Sent,
        });
        round_trip(TxReport {
            radio: RadioId("k1".into()),
            slot: None,
            outcome: TxOutcome::Denied(InterlockError::Expired),
        });
        round_trip(TxReport {
            radio: RadioId("k1".into()),
            slot: None,
            outcome: TxOutcome::Failed("PTT timeout".into()),
        });
    }

    #[test]
    fn logbook_round_trip() {
        round_trip(LogEntry {
            id: QsoId {
                origin: StationId("station-a".into()),
                seq: 17,
            },
            radio: Some(RadioId("k1".into())),
            call: Callsign("W4LL".into()),
            mode: OverAirMode::Ft8,
            band: Band::B20m,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000),
            exchange_sent: "-09".into(),
            exchange_rcvd: "R-12".into(),
            grid: Some(GridSquare("EM73".into())),
            section: None,
        });
        round_trip(WorkedStatus::New);
        round_trip(WorkedStatus::WorkedByMe);
    }

    #[test]
    fn scanner_round_trip() {
        round_trip(ScannerCommand::StartSurvey {
            stops: vec![
                (Band::B40m, OverAirMode::Ft8),
                (Band::B40m, OverAirMode::Ft4),
                (Band::B20m, OverAirMode::Ft8),
            ],
            dwell_slots: 2,
        });
        round_trip(ScannerCommand::SetStops {
            stops: vec![(Band::B20m, OverAirMode::Ft8)],
        });
        round_trip(ScannerCommand::Cancel);
        round_trip(ScannerState {
            status: ScanStatus::Scanning,
            current: Some(Band::B20m),
            current_mode: Some(OverAirMode::Ft4),
            last_scan: Some(Timestamp(1_700_000_000_000)),
        });
        round_trip(BandActivity {
            band: Band::B40m,
            mode: OverAirMode::Ft8,
            heard: 23,
            cq: 6,
            unworked: 7,
            t: Timestamp(1_700_000_000_000),
        });
    }

    #[test]
    fn clock_and_interlock_round_trip() {
        round_trip(ClockStatus {
            offset_ms: -3.5,
            slot_phase: 0.42,
            slot: SlotId(7),
            mode: OverAirMode::Ft4,
        });
        round_trip(InterlockToken(12345));
        for e in [
            InterlockError::Denied,
            InterlockError::Expired,
            InterlockError::NotHolder,
        ] {
            round_trip(e);
        }
    }

    #[test]
    fn subsystem_health_round_trip() {
        for id in [SubsystemId::Rig, SubsystemId::Audio] {
            assert_eq!(SubsystemId::parse(id.as_str()), Some(id));
            for state in [
                HealthState::Healthy,
                HealthState::Degraded("reconnecting".into()),
                HealthState::Down("device not found".into()),
            ] {
                round_trip(SubsystemHealth {
                    id,
                    state,
                    since: Timestamp(1_700_000_000_000),
                });
            }
        }
        assert_eq!(SubsystemId::parse("bogus"), None);
    }

    /// Build a `LogEntry` for a worked-key test: only `call`, `mode` and `band`
    /// matter to the key, the rest are filler.
    fn log_entry(call: &str, mode: OverAirMode, band: Band) -> LogEntry {
        LogEntry {
            id: QsoId {
                origin: StationId("station-a".into()),
                seq: 1,
            },
            radio: Some(RadioId("k1".into())),
            call: Callsign(call.into()),
            mode,
            band,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000),
            exchange_sent: "3A CO".into(),
            exchange_rcvd: "3A WCF".into(),
            grid: None,
            section: None,
        }
    }

    #[test]
    fn worked_key_collapses_digital_modes_per_band() {
        // The Field Day rule: every digital mode is one mode. A station worked on a
        // band is a dupe there regardless of FT8 vs FT4 — same key.
        let ft8 = log_entry("W1ABC", OverAirMode::Ft8, Band::B20m);
        let ft4 = log_entry("W1ABC", OverAirMode::Ft4, Band::B20m);
        assert_eq!(
            worked_key(&ft8, ContestProfile::ArrlFieldDay),
            worked_key(&ft4, ContestProfile::ArrlFieldDay),
        );
        // The mode never appears in the key, so it is dropped, not encoded.
        assert_eq!(
            worked_key(&ft8, ContestProfile::ArrlFieldDay),
            (Callsign("W1ABC".into()), Band::B20m),
        );
    }

    #[test]
    fn worked_key_distinguishes_per_band() {
        // The same call on a different band is a new contact — distinct keys.
        let on_20 = log_entry("W1ABC", OverAirMode::Ft8, Band::B20m);
        let on_40 = log_entry("W1ABC", OverAirMode::Ft8, Band::B40m);
        assert_ne!(
            worked_key(&on_20, ContestProfile::ArrlFieldDay),
            worked_key(&on_40, ContestProfile::ArrlFieldDay),
        );
    }

    #[test]
    fn worked_key_normalizes_callsign_case_and_whitespace() {
        // Canonical: the callsign is trimmed and upper-cased so two consumers can't
        // disagree on a dupe over case alone.
        let messy = log_entry(" w1abc ", OverAirMode::Ft8, Band::B20m);
        let clean = log_entry("W1ABC", OverAirMode::Ft8, Band::B20m);
        assert_eq!(
            worked_key(&messy, ContestProfile::ArrlFieldDay),
            worked_key(&clean, ContestProfile::ArrlFieldDay),
        );
    }

    #[test]
    fn worked_key_standard_profile_also_collapses_modes() {
        // The Standard profile is all-digital today, so it keys identically.
        let ft8 = log_entry("K2DEF", OverAirMode::Ft8, Band::B15m);
        let ft4 = log_entry("K2DEF", OverAirMode::Ft4, Band::B15m);
        assert_eq!(
            worked_key(&ft8, ContestProfile::Standard),
            worked_key(&ft4, ContestProfile::Standard),
        );
    }

    #[test]
    fn worked_set_is_worked_matches_per_band_case_insensitively() {
        // The published snapshot consumers read: W1ABC worked on 20 m only.
        let set = WorkedSet {
            entries: vec![WorkedEntry {
                call: Callsign("W1ABC".into()),
                band: Band::B20m,
                status: WorkedStatus::WorkedByMe,
            }],
        };
        // Worked on 20 m, regardless of the lookup's case (keys are normalized).
        assert!(set.is_worked(&Callsign("W1ABC".into()), Band::B20m));
        assert!(set.is_worked(&Callsign("w1abc".into()), Band::B20m));
        // Worked-ness is per band: the same call on 40 m is still unworked.
        assert!(!set.is_worked(&Callsign("W1ABC".into()), Band::B40m));
        // A different call is unworked.
        assert!(!set.is_worked(&Callsign("K2DEF".into()), Band::B20m));
    }

    #[test]
    fn callsign_normalized_trims_and_upcases() {
        assert_eq!(Callsign("  w4ll ".into()).normalized(), Callsign("W4LL".into()));
        // Already-canonical input is unchanged.
        assert_eq!(Callsign("N0JDC".into()).normalized(), Callsign("N0JDC".into()));
    }

    #[test]
    fn field_day_stops_are_the_six_hf_bands_with_a_calling_freq() {
        let stops = field_day_stops();
        // 6 bands × 2 modes = 12, less 160 m FT4 (no established calling freq) = 11.
        assert_eq!(stops.len(), 11);
        assert!(stops.iter().all(|&(b, m)| calling_freq(b, m).is_some()));
        assert!(!stops.contains(&(Band::B160m, OverAirMode::Ft4)));
        assert!(stops.contains(&(Band::B160m, OverAirMode::Ft8)));
        assert!(stops.contains(&(Band::B10m, OverAirMode::Ft4)));
    }

    #[test]
    fn band_status_round_trips() {
        round_trip(BandStatus {
            rows: vec![BandStatusRow {
                band: Band::B20m,
                mode: OverAirMode::Ft8,
                heard: 7,
                cq: 2,
                unworked: 5,
            }],
            t: Timestamp(1_700_000_000_000),
        });
    }
}
