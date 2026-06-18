//! Startup settings, read from the environment.
//!
//! This is the app's composition root for configuration: it reads the `DM420_*`
//! environment variables into a plain [`Settings`] value and builds the
//! [`app_core::CoreConfig`] the real producers run from. Keeping all env-reading
//! here leaves `core` a pure library that takes explicit config — and the same
//! `Settings` struct is what a future per-panel settings UI will edit instead of
//! the environment.
//!
//! ## Variables
//!
//! - `DM420_REAL` — run the real rig/decode producers (`1`/non-empty) instead of
//!   the mocks. Defaults to mocks so the GUI always runs with no hardware.
//! - `DM420_AUDIO_INPUT` — capture device name (case-insensitive substring match,
//!   e.g. `USB PnP`). Unset ⇒ the system default input.
//! - `DM420_SERIAL_PORT` — rig CAT device, e.g. `/dev/cu.usbserial-120`. Unset ⇒
//!   autodetect.
//! - `DM420_SERIAL_BAUD` — rig baud (one of the standard Kenwood rates). Invalid
//!   ⇒ warn and keep the default.
//! - `DM420_SERIAL_PROFILE` — serial line profile: `none` | `dtr-rts` | `rtscts`.
//! - `DM420_MODE` — `ft8` | `ft4` (default `ft8`).
//! - `DM420_WAV` — replay this WAV instead of live capture (bring-up/testing).
//! - `DM420_CALLSIGN` — the operator's station call sign (default `N0JDC`).
//! - `DM420_GRID` — the operator's Maidenhead grid locator (default `DN70KA`).

use std::path::PathBuf;

use app_core::{CoreConfig, DecodeSource, LineProfile, Protocol, SerialConfig};

/// Default rig baud when `DM420_SERIAL_BAUD` is unset or invalid.
pub(crate) const DEFAULT_BAUD: u32 = 19_200;

/// Standard Kenwood CAT baud rates, fastest first — the choices offered by the
/// settings-form baud picker. Presentation data: kept here (not pulled out of
/// `app_core`'s public API) so `core`'s contract doesn't declare a specific
/// vendor's rate table.
pub(crate) const KENWOOD_BAUDS: &[u32] = &[115_200, 57_600, 38_400, 19_200, 9_600, 4_800];

/// The subset of [`Settings`] the operator can edit live from the UI (the rig +
/// audio hardware bindings). Held by `BusView` as the source of truth for the
/// settings form, and pushed to the running producers on apply.
#[derive(Clone, PartialEq, Eq)]
pub struct HardwareConfig {
    pub audio_input: Option<String>,
    pub serial: SerialConfig,
    pub protocol: Protocol,
}

/// The operator's station identity: call sign + Maidenhead grid locator. This is
/// GUI-only presentation/encoding state — it labels the top bar and feeds the FT8
/// message generator — so it lives outside [`Settings`] and never reaches `core`.
/// Held by `App`, edited live from the top bar when the GUI is unlocked.
#[derive(Clone, PartialEq, Eq)]
pub struct Station {
    pub call: String,
    pub grid: String,
}

impl Station {
    /// Read `DM420_CALLSIGN` / `DM420_GRID`, falling back to the project's home
    /// station. Both are upper-cased to FT8/Maidenhead convention.
    pub fn from_env() -> Self {
        Station {
            call: env_nonempty("DM420_CALLSIGN").unwrap_or_else(|| "N0JDC".into()).to_uppercase(),
            grid: env_nonempty("DM420_GRID").unwrap_or_else(|| "DN70KA".into()).to_uppercase(),
        }
    }
}

/// Parsed startup configuration. Built once at launch by [`Settings::from_env`].
pub struct Settings {
    /// Run the real producers (`DM420_REAL`) rather than the mocks.
    pub real: bool,
    /// Capture device name; `None` ⇒ system default input.
    pub audio_input: Option<String>,
    /// How to reach the rig.
    pub serial: SerialConfig,
    /// On-air protocol for the decoder.
    pub protocol: Protocol,
    /// If set, replay this WAV instead of opening the live capture device.
    pub wav: Option<PathBuf>,
}

impl Settings {
    /// Read the `DM420_*` environment into a `Settings`. Never fails: bad values
    /// log a warning and fall back to a sensible default.
    pub fn from_env() -> Self {
        Settings {
            real: env_flag("DM420_REAL"),
            audio_input: env_nonempty("DM420_AUDIO_INPUT"),
            serial: serial_from_env(),
            protocol: protocol_from_env(),
            wav: wav_from_env(),
        }
    }

    /// Whether the real producers should drive the bus.
    pub fn is_real(&self) -> bool {
        self.real
    }

    /// The live-editable hardware bindings (rig + audio), as the UI first sees
    /// them.
    pub fn hardware(&self) -> HardwareConfig {
        HardwareConfig {
            audio_input: self.audio_input.clone(),
            serial: self.serial.clone(),
            protocol: self.protocol,
        }
    }

    /// Build the `core` config for real mode: the configured rig, TX blocked, and
    /// either a WAV replay (`DM420_WAV`) or live capture of the configured device.
    pub fn core_config(&self) -> CoreConfig {
        let decode = match &self.wav {
            Some(path) => DecodeSource::Wav {
                path: path.clone(),
                protocol: self.protocol,
                looping: true,
            },
            None => DecodeSource::Live {
                input: self.audio_input.clone(),
                protocol: self.protocol,
            },
        };
        CoreConfig {
            radio: mocks::radio_id(),
            allow_transmit: false,
            decode,
            serial: Some(self.serial.clone()),
        }
    }
}

/// True if the var is set to a non-empty value other than `"0"`.
fn env_flag(key: &str) -> bool {
    std::env::var(key).map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// The var's value if set and non-empty, else `None`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn serial_from_env() -> SerialConfig {
    let port = env_nonempty("DM420_SERIAL_PORT");

    let baud = match env_nonempty("DM420_SERIAL_BAUD") {
        Some(s) => s.parse::<u32>().unwrap_or_else(|_| {
            eprintln!("dm420: DM420_SERIAL_BAUD='{s}' is not a number; using {DEFAULT_BAUD}");
            DEFAULT_BAUD
        }),
        None => DEFAULT_BAUD,
    };

    let profile = match env_nonempty("DM420_SERIAL_PROFILE") {
        Some(s) => LineProfile::parse(&s).unwrap_or_else(|| {
            eprintln!(
                "dm420: DM420_SERIAL_PROFILE='{s}' unknown (use none|dtr-rts|rtscts); using default"
            );
            LineProfile::Default
        }),
        None => LineProfile::Default,
    };

    SerialConfig {
        port,
        baud,
        profile,
        // Always allow the autodetect sweep as a fallback so the operator isn't
        // stuck guessing a port/baud; an explicit port is still tried first.
        autodetect: true,
    }
}

fn protocol_from_env() -> Protocol {
    match env_nonempty("DM420_MODE") {
        Some(s) => match s.trim().to_lowercase().as_str() {
            "ft8" => Protocol::Ft8,
            "ft4" => Protocol::Ft4,
            _ => {
                eprintln!("dm420: DM420_MODE='{s}' unknown (use ft8|ft4); using ft8");
                Protocol::Ft8
            }
        },
        None => Protocol::Ft8,
    }
}

fn wav_from_env() -> Option<PathBuf> {
    let p = PathBuf::from(env_nonempty("DM420_WAV")?);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("dm420: DM420_WAV='{}' does not exist; using live capture", p.display());
        None
    }
}
