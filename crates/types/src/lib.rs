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

/// A Maidenhead grid locator, e.g. `"FN31"` / `"DN70KA"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridSquare(pub String);

/// An ARRL/RAC section, e.g. `"CO"` — exchange/log only, never a map location.
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

// NOTE: `calling_freq(mode, band) -> AbsHz` is mode-owned *reference data*, not a
// message — it lives in the `modes` crate, not here. One source of truth for
// "where FT8 lives on 20m".

// =====================================================================
// §5  Selection + QSO
// =====================================================================

/// `selection/{id}/active` (State) — a gesture, not an action; the mode service
/// interprets it. `target == None` is a bare retune (click in the FFT); `Some` is
/// intent to work that decode (click on the text).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Selection {
    pub radio: RadioId,
    /// Where the next TX audio is tuned.
    pub outgoing: OffsetHz,
    pub target: Option<DecodeRef>,
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
    Start { target: DecodeRef },
    CallCq,
    Abort,
}

/// `qso/{id}/state` (State + short history) — written by the QSO engine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QsoState {
    pub radio: RadioId,
    pub phase: QsoPhase,
    pub partner: Option<Callsign>,
    /// What the engine will send next slot.
    pub next_tx: Option<OutgoingMessage>,
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

// =====================================================================
// §7  Logbook  —  logbook/entries  (StreamLossless + gossiped)
// =====================================================================

/// A logged contact. `id` is the G-set merge key; `origin` drives the own-vs-
/// network distinction in the panel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub id: QsoId,
    pub origin: StationId,
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

// =====================================================================
// §8  Scanner + band activity
// =====================================================================

/// `scanner/command` (Command) — to the band scanner.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ScannerCommand {
    /// `dwell_slots >= 2` (even/odd slots).
    StartSurvey {
        bands: Vec<Band>,
        dwell_slots: u8,
    },
    Cancel,
}

/// `scanner/state` (State) — written by the scanner.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScannerState {
    pub status: ScanStatus,
    pub current: Option<Band>,
    pub last_scan: Option<Timestamp>,
}

/// Scanner run state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanStatus {
    Idle,
    Scanning,
}

/// Per-band counts feeding the band-scan panel and (later) the cross-station
/// `band_activity` gossip.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BandActivity {
    pub band: Band,
    pub stations_seen: u32,
    pub unworked: u32,
    pub t: Timestamp,
}

// =====================================================================
// §9  Cross-station gossip  —  DEFERRED
// =====================================================================
//
// `StationSnapshot`, `WorkingTarget`, `HeardStation` and the gossip G-set live
// in the networking phase (Josh-owned). Neither the GUI wiring nor Joel's
// rig/decode framework needs them yet, so they are intentionally not implemented
// here. See `docs/message-catalog.md` §9 for the draft shapes.

// =====================================================================
// §10  Clock + interlock
// =====================================================================

/// `clock/status` (State) — clock health for the UI's sync indicator.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ClockStatus {
    pub offset_ms: f32,
    pub slot_phase: f32,
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
            outgoing: OffsetHz(1500.0),
            target: Some(DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(42),
                call: Some(Callsign("N0JDC".into())),
            }),
        });
        round_trip(QsoCommand::Start {
            target: DecodeRef {
                radio: RadioId("k1".into()),
                slot: SlotId(42),
                call: None,
            },
        });
        round_trip(QsoCommand::CallCq);
        round_trip(QsoCommand::Abort);
        round_trip(QsoState {
            radio: RadioId("k1".into()),
            phase: QsoPhase::InExchange { step: 2 },
            partner: Some(Callsign("W4LL".into())),
            next_tx: Some(sample_outgoing()),
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
            origin: StationId("station-a".into()),
            radio: Some(RadioId("k1".into())),
            call: Callsign("W4LL".into()),
            mode: OverAirMode::Ft8,
            band: Band::B20m,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000),
            exchange_sent: "-09".into(),
            exchange_rcvd: "R-12".into(),
            grid: Some(GridSquare("EM73".into())),
        });
        round_trip(WorkedStatus::New);
        round_trip(WorkedStatus::WorkedByMe);
    }

    #[test]
    fn scanner_round_trip() {
        round_trip(ScannerCommand::StartSurvey {
            bands: vec![Band::B40m, Band::B20m, Band::B15m, Band::B10m],
            dwell_slots: 2,
        });
        round_trip(ScannerCommand::Cancel);
        round_trip(ScannerState {
            status: ScanStatus::Scanning,
            current: Some(Band::B20m),
            last_scan: Some(Timestamp(1_700_000_000_000)),
        });
        round_trip(BandActivity {
            band: Band::B40m,
            stations_seen: 23,
            unworked: 7,
            t: Timestamp(1_700_000_000_000),
        });
    }

    #[test]
    fn clock_and_interlock_round_trip() {
        round_trip(ClockStatus {
            offset_ms: -3.5,
            slot_phase: 0.42,
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
}
