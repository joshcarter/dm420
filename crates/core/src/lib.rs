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

mod control;
mod decode;
mod health;
mod interlock;
mod map;
mod parse;
mod rig_adapter;
mod tx;

pub use control::{AudioControl, CoreControl, RigControl, TxControl};
pub use modes::Protocol;
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
    pub port: Option<String>,
    pub baud: u32,
    pub profile: LineProfile,
    /// Sweep ports/bauds to find the radio when `port` is unset or fails to open.
    pub autodetect: bool,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: None,
            baud: 19_200,
            profile: LineProfile::Default,
            autodetect: true,
        }
    }
}

/// Configuration for [`spawn`].
pub struct CoreConfig {
    pub radio: RadioId,
    /// Forwarded to Joel's rig actor; `false` hard-blocks TX (the default).
    pub allow_transmit: bool,
    pub decode: DecodeSource,
    /// How to reach the rig. `None` ⇒ no rig producer (mock/headless).
    pub serial: Option<SerialConfig>,
    /// Initial TX audio output device; `None` = system default. Live-editable
    /// afterward via [`CoreControl::tx`].
    pub tx_output: Option<String>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            radio: radio_id(),
            allow_transmit: false,
            decode: DecodeSource::None,
            serial: None,
            tx_output: None,
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
        tx_output,
    } = cfg;

    let mut control = CoreControl::default();

    // The interlock granter owns the single PTT token — the authority
    // `allow_transmit` ultimately unlocks. Serve it for bus clients (the QSO
    // shell) and share it into the rig adapter for in-process key-up validation.
    let granter = interlock::Granter::default();
    interlock::serve(bus, radio.clone(), granter.clone());

    // TX path: the audio-TX service that synthesizes, keys, and plays. Its output
    // device is live-editable from the UI via `control.tx`. Spawned whenever
    // transmit is permitted (the operator still keys it explicitly, per over).
    let tx_control = Arc::new(control::TxControl::new(tx_output));
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

    control
}
