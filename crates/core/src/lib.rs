//! Radio-agnostic application core — the bus-adapter layer.
//!
//! This crate is the seam between Josh's message bus and Joel's (W4LL) vendored
//! rig/audio/modes prototype. The vendored crates are deliberately bus-agnostic
//! (pure domain); all bus coupling lives here, mirroring the `mocks` crate's
//! `spawn(bus)` pattern so a real producer can displace a mock one topic at a
//! time. [`spawn`] launches:
//!
//! - **rig** ([`rig_adapter`]): starts Joel's rig actor (the in-memory mock rig by
//!   default), serves `radio/{id}/command` (`RigCommand`), and publishes
//!   `radio/{id}/rig_state` (`RigState`).
//! - **decode** ([`decode`]): a slot → `modes::decode` → `Decode` pipeline driven
//!   from a WAV recording or live cpal capture, publishing `radio/{id}/decodes`.
//!
//! Spec: `docs/message-catalog.md` §2–§4. See the architecture plan and
//! `crates/modes/ATTRIBUTION.md` (the decoder is an MIT `ft8_lib` port).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use bus::BusHandle;
use bus::types::RadioId;

mod clock;
mod control;
mod decode;
mod health;
mod interlock;
mod map;
mod parse;
mod rig_adapter;
mod scan;
mod tx;
mod worked;

pub use control::{AudioControl, CoreControl, RigControl, TxControl};
pub use modes::{Protocol, slot_period};
pub use parse::parse_message;
pub use rig::LineProfile;
pub use rig_adapter::CommandResult;

/// Names of input-capable audio devices, for a UI device picker. Empty on error.
pub fn list_audio_inputs() -> Vec<String> {
    audio::list_devices()
        .map(|ds| {
            ds.into_iter()
                .filter(|d| d.kind == audio::DeviceKind::Input)
                .map(|d| d.name)
                .collect()
        })
        .unwrap_or_default()
}

/// Names of output-capable audio devices, for a UI device picker. Empty on error.
pub fn list_audio_outputs() -> Vec<String> {
    audio::list_devices()
        .map(|ds| {
            ds.into_iter()
                .filter(|d| d.kind == audio::DeviceKind::Output)
                .map(|d| d.name)
                .collect()
        })
        .unwrap_or_default()
}

/// Names of available serial ports, likely-radio first, for a UI port picker.
/// Empty on error.
pub fn list_serial_ports() -> Vec<String> {
    rig::probe::candidate_ports(true).unwrap_or_default()
}

/// The stable USB identity (vid, pid, serial number) of the device currently at
/// `port`, for persisting alongside the path so it can be re-resolved after a
/// replug. Any field may be `None` (non-USB port, or the OS doesn't expose it).
pub fn usb_identity_for_port(port: &str) -> (Option<u16>, Option<u16>, Option<String>) {
    rig::list_ports()
        .ok()
        .and_then(|ports| ports.into_iter().find(|p| p.name == port))
        .map(|p| (p.vid, p.pid, p.serial_number))
        .unwrap_or((None, None, None))
}

/// The default radio id. Matches `mocks::radio_id()` so the GUI's existing topic
/// subscriptions line up whether the data comes from `core` or `mocks`.
pub fn radio_id() -> RadioId {
    RadioId("rig0".into())
}

/// Where the decode pipeline gets its audio.
pub enum DecodeSource {
    /// Replay a WAV recording, chunked into slots. `looping` restarts at the end
    /// so the GUI keeps showing traffic. Pacing is replay-relative, not wall-clock
    /// slot timing (see [`decode`]).
    Wav {
        path: PathBuf,
        protocol: Protocol,
        looping: bool,
    },
    /// Live cpal capture, one slot at a time, aligned to UTC slot boundaries.
    Live {
        input: Option<String>,
        protocol: Protocol,
    },
    /// No decode producer (rig only).
    None,
}

/// How to reach the rig over CAT (Kenwood serial). An explicit `port` is opened
/// first; with `autodetect` set, a failed/absent port falls back to sweeping the
/// likely-radio ports × standard bauds to find the radio. `baud`/`profile` are
/// the manual hints used when not autodetecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialConfig {
    /// Device path, e.g. `"/dev/cu.usbserial-120"`. `None` ⇒ autodetect only.
    /// On macOS the path embeds the USB location id and changes on every replug,
    /// so it is only a *hint* — the stable USB identity below is tried first.
    pub port: Option<String>,
    /// Stable USB serial number (iSerial) of the radio's CAT interface. When set,
    /// the supervisor resolves it to whatever path the device currently has,
    /// surviving replug/port renumbering. The strong key for "this is my radio".
    pub usb_serial: Option<String>,
    /// USB vendor/product id — a weaker fallback used to identify the device when
    /// no serial number is exposed (only when it picks out a single port).
    pub usb_vid: Option<u16>,
    pub usb_pid: Option<u16>,
    pub baud: u32,
    pub profile: LineProfile,
    /// Sweep ports/bauds to find the radio when the identity/`port` don't resolve.
    pub autodetect: bool,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: None,
            usb_serial: None,
            usb_vid: None,
            usb_pid: None,
            baud: 19_200,
            profile: LineProfile::Default,
            autodetect: true,
        }
    }
}

/// Default TX audio gain (linear, applied to the full-scale synth output). The
/// synth emits at 0 dBFS; `0.5` (−6 dBFS) backs off enough to keep a typical
/// rig's ALC at/under threshold while leaving plenty of drive. Operators trim
/// from here with their rig's input-gain control (or the `[audio] tx_gain` key).
pub const DEFAULT_TX_GAIN: f32 = 0.5;

/// Configuration for [`spawn`].
pub struct CoreConfig {
    pub radio: RadioId,
    /// Forwarded to Joel's rig actor; `false` hard-blocks TX (the default).
    pub allow_transmit: bool,
    pub decode: DecodeSource,
    /// How to reach the rig. `None` ⇒ no rig producer (mock/headless).
    pub serial: Option<SerialConfig>,
    /// Where to persist the logbook (JSONL, one contact per line). `None` ⇒ no
    /// logbook producer, so the log stays in-memory only (mock mode supplies its
    /// own fake logbook).
    pub logbook: Option<PathBuf>,
    /// Where to append the raw decode/transmit archive (JSONL, every heard + sent
    /// message). `None` ⇒ no archive producer — the default; opt in via the config
    /// TOML's `[archive] decodes`. See the `archive` crate.
    pub decode_archive: Option<PathBuf>,
    /// Initial TX audio output device; `None` = system default. Live-editable
    /// afterward via [`CoreControl::tx`].
    pub tx_output: Option<String>,
    /// Linear gain applied to the synth's full-scale (0 dBFS) output before it
    /// reaches the rig. Clamped to `[0.0, 1.0]`; see [`DEFAULT_TX_GAIN`].
    /// Live-editable afterward via [`CoreControl::tx`].
    pub tx_gain: f32,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            radio: radio_id(),
            allow_transmit: false,
            decode: DecodeSource::None,
            serial: None,
            logbook: None,
            decode_archive: None,
            tx_output: None,
            tx_gain: DEFAULT_TX_GAIN,
        }
    }
}

/// Launch the real producers onto `bus`. Must be called from within a tokio
/// runtime context (like `mocks::spawn`); the rig state poller and live capture
/// run on their own std threads, while command-serving and WAV replay run as
/// tokio tasks.
///
/// Returns a [`CoreControl`] for live reconfiguration from the UI — the running
/// rig/audio producers read their settings through it, so the operator can
/// change device/port/baud/mode without a restart.
pub fn spawn(bus: &BusHandle, cfg: CoreConfig) -> CoreControl {
    let CoreConfig {
        radio,
        allow_transmit,
        decode,
        serial,
        logbook,
        decode_archive,
        tx_output,
        tx_gain,
    } = cfg;

    tracing::info!(
        radio = ?radio,
        allow_transmit,
        tx_output = ?tx_output,
        has_serial = serial.is_some(),
        "core: launching producers",
    );

    let mut control = CoreControl::default();

    // The interlock granter owns the single PTT token — the authority
    // `allow_transmit` ultimately unlocks. Serve it for bus clients (the QSO
    // shell) and share it into the rig adapter for in-process key-up validation.
    let granter = interlock::Granter::default();
    interlock::serve(bus, radio.clone(), granter.clone());

    // TX path: the audio-TX service that synthesizes, keys, and plays. Its output
    // device is live-editable from the UI via `control.tx`. Spawned whenever
    // transmit is permitted (the operator still keys it explicitly, per over).
    let tx_control = Arc::new(control::TxControl::new(tx_output, tx_gain));
    if allow_transmit {
        tx::spawn(bus, radio.clone(), tx_control.clone());
    }
    control.tx = Some(tx_control);

    // The rig producer is optional: with no serial config there's simply no rig
    // on the bus (the GUI shows it as down). A present config never panics —
    // `rig_adapter` supervises the connection and reports health.
    if let Some(serial) = serial {
        let rig = Arc::new(RigControl::new(serial));
        rig_adapter::spawn(bus, radio.clone(), allow_transmit, rig.clone(), granter.clone());
        control.rig = Some(rig);
    }

    // The raw decode/transmit archive: append every heard + sent message to a JSONL
    // file for offline diagnostics. Opt-in (config TOML `[archive] decodes`); `None`
    // ⇒ no producer. Subscribe-only and off the operating path, so it can't affect
    // RX/TX. Spawned here while `radio` is still owned (the decode match below
    // consumes it).
    if let Some(path) = decode_archive {
        archive::spawn(bus, radio.clone(), path);
    }

    // The band scanner (spawned after the decode match below) needs the radio id,
    // but that match consumes `radio` — clone it here while it's still owned.
    let scanner_radio = radio.clone();
    // Same for the worked-status producer (spawned alongside the logbook below).
    let worked_radio = radio.clone();

    // The active mode for the slot clock below: live capture follows it through
    // `AudioControl`, so capture this only as the WAV/none fallback.
    let fallback_proto = match &decode {
        DecodeSource::Wav { protocol, .. } | DecodeSource::Live { protocol, .. } => *protocol,
        DecodeSource::None => Protocol::Ft8,
    };
    match decode {
        DecodeSource::Wav {
            path,
            protocol,
            looping,
        } => decode::spawn_wav(bus, radio, path, protocol, looping),
        DecodeSource::Live { input, protocol } => {
            let audio = Arc::new(AudioControl::new(input, protocol));
            decode::spawn_live(bus, radio, audio.clone());
            control.audio = Some(audio);
        }
        DecodeSource::None => {}
    }

    // Band scanner: on demand, time-slices the receiver across bands/modes, holding
    // the TX interlock for the sweep. Needs the in-process AudioControl (a mode
    // switch restarts capture — not a bus op) and the granter, both owned here.
    scan::spawn(bus, scanner_radio, control.audio.clone(), granter.clone());

    // Slot-timing clock: the single authoritative slot identity (`clock/status`),
    // mode-aware so its slot numbering matches the decoder's and the QSO tick
    // parity. Displaces the FT8-hardcoded mock clock that `spawn_support` used to
    // inject into the real path. In live mode it tracks the operator's selected
    // mode via `AudioControl`; otherwise it uses the configured protocol.
    clock::spawn(bus, control.audio.clone(), fallback_proto);

    // The logbook owns `logbook/entries` in real mode: it persists QSOs the engine
    // logs on RR73 and replays history on startup. In mock mode there's no path, so
    // the fake logbook (from `mocks::spawn`) drives the topic instead.
    if let Some(path) = logbook {
        logbook::spawn(bus, path);
    }

    // The worked-status producer: the single owner of "which (call, band) I've
    // worked". It folds `logbook/entries` through the canonical `worked_key` and
    // publishes the authoritative `WorkedSet` on `radio/{id}/worked`; the scanner, the
    // GUI map/waterslide, and the `core::scan` tally all subscribe instead of
    // re-deriving the dupe rule. Spawned whether or not a logbook is configured — with
    // no entries it simply publishes nothing and consumers default to "all unworked".
    // Both shipping contest profiles collapse digital modes per band today; thread the
    // selected profile through here when contest selection lands in config.
    worked::spawn(bus, worked_radio, types::ContestProfile::ArrlFieldDay);

    // LAN gossip: discover other operators on the network and exchange station
    // snapshots (shared logbook + working-intent ride this in later steps). Config
    // is read from env for now (interim, like the rest); `DM420_NET=0` opts out.
    // Best-effort — `net::spawn` degrades to a logged warning if the socket or
    // mDNS can't come up, so a missing network never blocks the rest of the app.
    if let Some(net_cfg) = net::NetConfig::from_env() {
        net::spawn(bus, net_cfg);
    }

    control
}
