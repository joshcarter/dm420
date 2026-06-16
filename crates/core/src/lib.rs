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

use bus::BusHandle;
use bus::types::RadioId;

mod decode;
mod map;
mod parse;
mod rig_adapter;

pub use modes::Protocol;
pub use parse::parse_message;
pub use rig_adapter::CommandResult;

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

/// Configuration for [`spawn`].
pub struct CoreConfig {
    pub radio: RadioId,
    /// Forwarded to Joel's rig actor; `false` hard-blocks TX (the default).
    pub allow_transmit: bool,
    pub decode: DecodeSource,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            radio: radio_id(),
            allow_transmit: false,
            decode: DecodeSource::None,
        }
    }
}

/// Launch the real producers onto `bus`. Must be called from within a tokio
/// runtime context (like `mocks::spawn`); the rig state poller and live capture
/// run on their own std threads, while command-serving and WAV replay run as
/// tokio tasks.
pub fn spawn(bus: &BusHandle, cfg: CoreConfig) {
    let CoreConfig {
        radio,
        allow_transmit,
        decode,
    } = cfg;

    rig_adapter::spawn(bus, radio.clone(), allow_transmit);

    match decode {
        DecodeSource::Wav {
            path,
            protocol,
            looping,
        } => decode::spawn_wav(bus, radio, path, protocol, looping),
        DecodeSource::Live { input, protocol } => decode::spawn_live(bus, radio, input, protocol),
        DecodeSource::None => {}
    }
}
