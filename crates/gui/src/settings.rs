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
//! - `DM420_MOCK` — run the mock producers instead of the real rig/decode path.
//!   Real producers are the **default**; set this (`1`/non-empty) when you want
//!   the GUI to run with no hardware.
//! - `DM420_AUDIO_INPUT` — capture device name (case-insensitive substring match,
//!   e.g. `USB PnP`). Unset ⇒ the system default input.
//! - `DM420_SERIAL_PORT` — rig CAT device, e.g. `/dev/cu.usbserial-120`. Unset ⇒
//!   autodetect.
//! - `DM420_SERIAL_BAUD` — rig baud (one of the standard Kenwood rates). Invalid
//!   ⇒ warn and keep the default.
//! - `DM420_SERIAL_PROFILE` — serial line profile: `none` | `dtr-rts` | `rtscts`.
//!   The serial port/baud/profile (plus the `autodetect` flag, which has no env
//!   var) are persisted to the config file's `[serial]` table when edited in the
//!   unlocked UI; the env vars override the saved values for a single launch.
//! - `DM420_MODE` — `ft8` | `ft4` (default `ft8`).
//! - `DM420_WAV` — replay this WAV instead of live capture (bring-up/testing).
//! - `DM420_LOGBOOK` — path to the persisted logbook JSON (default
//!   `~/.dm420/logbook.json`). Real mode only; the mock logbook is in-memory.
//! - `DM420_CALLSIGN` — the operator's station call sign (default `N0JDC`).
//! - `DM420_GRID` — the operator's Maidenhead grid locator (default `DN70KA`).

use std::path::{Path, PathBuf};

use app_core::{CoreConfig, DecodeSource, LineProfile, Protocol, SerialConfig, DEFAULT_TX_GAIN};

/// Default rig baud when `DM420_SERIAL_BAUD` is unset or invalid.
pub(crate) const DEFAULT_BAUD: u32 = 19_200;

/// Standard Kenwood CAT baud rates, fastest first — the choices offered by the
/// settings-form baud picker. Presentation data: kept here (not pulled out of
/// `app_core`'s public API) so `core`'s contract doesn't declare a specific
/// vendor's rate table.
pub(crate) const KENWOOD_BAUDS: &[u32] = &[115_200, 57_600, 38_400, 19_200, 9_600, 4_800];

/// Default log level when neither `RUST_LOG` nor `[logging] level` is set.
pub(crate) const DEFAULT_LOG_LEVEL: &str = "info";

/// Where DM420's TOML config lives: `$HOME/.dm420/config.toml`, falling back to
/// `config.toml` in the current directory when there's no home. Holds the
/// `[station]` and `[audio]` tables (the latter also carries `tx_gain`, the
/// linear TX drive level — hand-edited, no env var) and the `[logging] level`.
/// The writers
/// (`Station::save`, [`save_audio_config`]) create the parent directory on first
/// save. The format/persistence is interim and TBD — see `joels-notes.md`.
pub(crate) fn config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".dm420").join("config.toml");
    }
    PathBuf::from("config.toml")
}

/// Create the config directory (`$HOME/.dm420`) if it doesn't exist yet, then
/// write `text` to `path`. Logs on error rather than failing — a config write is
/// best-effort.
fn write_config(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, text) {
        tracing::warn!(path = %path.display(), error = %e, "could not write config");
    }
}

/// The configured log level for DM420's crates: the `[logging] level` key in the
/// config file, or [`DEFAULT_LOG_LEVEL`] if unset. Read once at startup by
/// [`crate::logging::init`] — before the subscriber exists, so it logs nothing
/// itself. `RUST_LOG` (handled in `logging`) overrides this when present.
pub fn log_level() -> String {
    let text = std::fs::read_to_string(config_path()).unwrap_or_default();
    parse_table_value(&text, "logging", "level").unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string())
}

/// The subset of [`Settings`] the operator can edit live from the UI (the rig +
/// audio hardware bindings). Held by `BusView` as the source of truth for the
/// settings form, and pushed to the running producers on apply.
#[derive(Clone, PartialEq, Eq)]
pub struct HardwareConfig {
    pub audio_input: Option<String>,
    /// TX audio output device (the rig's data-in); `None` = system default.
    pub audio_output: Option<String>,
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
    /// Load the operator's station identity, in precedence order: the
    /// `DM420_CALLSIGN` / `DM420_GRID` env vars → the `[station]` table in the
    /// config file ([`config_path`]) → unset. **There is no default** — a silent
    /// one risks transmitting as the wrong station. Operating is blocked until a
    /// call is set (typed into the unlocked top bar, or written to the config
    /// file). The config format/persistence is interim and TBD — see
    /// `joels-notes.md`.
    pub fn load() -> Self {
        let (toml_call, toml_grid) = read_station_config(&config_path());
        Station {
            call: env_nonempty("DM420_CALLSIGN")
                .or(toml_call)
                .unwrap_or_default()
                .to_uppercase(),
            grid: env_nonempty("DM420_GRID")
                .or(toml_grid)
                .unwrap_or_default()
                .to_uppercase(),
        }
    }

    /// Whether a callsign has been set. Operating (CQ/answer/TX/log) is gated on
    /// this, since without it we'd identify as a blank/incorrect station.
    pub fn is_set(&self) -> bool {
        !self.call.trim().is_empty()
    }

    /// Persist the current identity to the config file, preserving comments and
    /// any other content. Called on GUI re-lock so UI edits survive a restart;
    /// write errors are logged, not fatal.
    pub fn save(&self) {
        let path = config_path();
        let existing = std::fs::read_to_string(&path).ok();
        let text = update_station_config(existing.as_deref(), &self.call, &self.grid);
        write_config(&path, &text);
    }

    /// The identity the QSO engine builds outgoing messages from. The contest
    /// profile and Field Day exchange are placeholders until a contest/exchange
    /// UI exists — TODO: surface `ContestProfile` + the FD `<class> <section>`
    /// (the engine already sequences both profiles; only the picker is missing).
    pub fn to_qso_config(&self) -> qso::StationConfig {
        qso::StationConfig {
            call: types::Callsign(self.call.clone()),
            grid: types::GridSquare(self.grid.clone()),
            fd_class: "1B".into(),
            fd_section: types::Section("CO".into()),
            contest: types::ContestProfile::Standard,
        }
    }
}

/// Parsed startup configuration. Built once at launch by [`Settings::from_env`].
pub struct Settings {
    /// Run the real producers rather than the mocks. Real is the default; set
    /// `DM420_MOCK` to run the mocks instead.
    pub real: bool,
    /// Capture device name; `None` ⇒ system default input.
    pub audio_input: Option<String>,
    /// How to reach the rig.
    pub serial: SerialConfig,
    /// On-air protocol for the decoder.
    pub protocol: Protocol,
    /// If set, replay this WAV instead of opening the live capture device.
    pub wav: Option<PathBuf>,
    /// TX audio output device (the rig's data-in); `None` = system default.
    /// Persisted in the config file's `[audio]` table (no env var).
    pub audio_output: Option<String>,
    /// Linear TX audio gain (`[audio] tx_gain` in the config file; no env var).
    /// The synth emits at 0 dBFS, so this backs the drive off before the rig —
    /// see [`DEFAULT_TX_GAIN`]. Clamped to `[0.0, 1.0]` by `core`.
    pub tx_gain: f32,
}

impl Settings {
    /// Read the `DM420_*` environment into a `Settings`. Never fails: bad values
    /// log a warning and fall back to a sensible default.
    pub fn from_env() -> Self {
        // Persisted audio device selections (config file [audio]); the env var
        // still wins for the input, for quick overrides.
        let (toml_in, toml_out) = read_audio_config(&config_path());
        Settings {
            // Real producers are the default now; `DM420_MOCK` opts back into mocks.
            real: !env_flag("DM420_MOCK"),
            audio_input: env_nonempty("DM420_AUDIO_INPUT").or(toml_in),
            serial: serial_from_env(),
            protocol: protocol_from_env(),
            wav: wav_from_env(),
            audio_output: toml_out,
            tx_gain: read_tx_gain(&config_path()),
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
            audio_output: self.audio_output.clone(),
            serial: self.serial.clone(),
            protocol: self.protocol,
        }
    }

    /// Build the `core` config for real mode: the configured rig (TX permitted —
    /// the operator still keys explicitly per over) and either a WAV replay
    /// (`DM420_WAV`) or live capture of the configured device.
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
            allow_transmit: true,
            decode,
            serial: Some(self.serial.clone()),
            logbook: Some(logbook_path()),
            decode_archive: read_archive_config(&config_path()),
            tx_output: self.audio_output.clone(),
            tx_gain: self.tx_gain,
        }
    }
}

/// Where the real logbook persists its JSON. `DM420_LOGBOOK` overrides; otherwise
/// `~/.dm420/logbook.json`, falling back to the current directory if there's no
/// home. The logbook creates the parent directory on first write.
fn logbook_path() -> PathBuf {
    if let Some(p) = env_nonempty("DM420_LOGBOOK") {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".dm420").join("logbook.json");
    }
    PathBuf::from("dm420-logbook.json")
}

/// Read the raw decode/transmit archive path from the config file's
/// `[archive] decodes` key. Absent, blank, or no file ⇒ `None` (capture disabled —
/// the default). The path is used verbatim (no default location): the operator opts
/// in by naming an explicit file, the same "you name it" stance as `DM420_LOGBOOK`.
/// Config-only by design — there is intentionally no env-var override.
fn read_archive_config(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_table_value(&text, "archive", "decodes").map(PathBuf::from)
}

/// True if the var is set to a non-empty value other than `"0"`.
fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// The var's value if set and non-empty, else `None`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Read the interim station config file and return its `(callsign, grid)`.
fn read_station_config(path: &Path) -> (Option<String>, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_station_config(&text),
        Err(_) => (None, None),
    }
}

/// Read the persisted `(input, output)` audio device names from the config file.
fn read_audio_config(path: &Path) -> (Option<String>, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_audio_config(&text),
        Err(_) => (None, None),
    }
}

/// Read the TX audio gain from the config file's `[audio] tx_gain` key, clamped to
/// `[0.0, 1.0]`. Falls back to [`DEFAULT_TX_GAIN`] when the file/key is absent or
/// the value doesn't parse — so a missing or fat-fingered entry can never key the
/// rig hotter than the safe default.
fn read_tx_gain(path: &Path) -> f32 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_float(&text, "audio", "tx_gain"))
        .filter(|g| g.is_finite())
        .map(|g| g.clamp(0.0, 1.0))
        .unwrap_or(DEFAULT_TX_GAIN)
}

/// Read a single string value from `table`'s `key`. **Not** a full TOML parser —
/// it deliberately avoids a dependency for a format that is still TBD (see
/// `joels-notes.md`); swap in the `toml` crate when the config grows. An empty
/// value counts as unset.
fn parse_table_value(text: &str, table: &str, key: &str) -> Option<String> {
    let mut in_table = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(t) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_table = t.trim() == table;
            continue;
        }
        if in_table
            && let Some((k, val)) = line.split_once('=')
            && k.trim() == key
        {
            let val = val.trim().trim_matches('"').trim();
            return (!val.is_empty()).then(|| val.to_string());
        }
    }
    None
}

/// Pull `callsign` (or `call`) / `grid` from the `[station]` table.
fn parse_station_config(text: &str) -> (Option<String>, Option<String>) {
    let call = parse_table_value(text, "station", "callsign")
        .or_else(|| parse_table_value(text, "station", "call"));
    let grid = parse_table_value(text, "station", "grid");
    (call, grid)
}

/// Pull the `input` / `output` audio device names from the `[audio]` table.
fn parse_audio_config(text: &str) -> (Option<String>, Option<String>) {
    (
        parse_table_value(text, "audio", "input"),
        parse_table_value(text, "audio", "output"),
    )
}

/// The persisted `[serial]` rig-control settings. Any subset may be present; the
/// env vars and built-in defaults fill the gaps in [`serial_from_env`]. An empty
/// value (e.g. `port = ""`) counts as unset — same convention as `[audio]`.
#[derive(Default)]
struct SerialFile {
    port: Option<String>,
    /// Stable USB identity of the radio's CAT interface, captured when the device
    /// was picked. Resolved to the live path on connect (replug-proof).
    usb_serial: Option<String>,
    usb_vid: Option<u16>,
    usb_pid: Option<u16>,
    baud: Option<u32>,
    profile: Option<LineProfile>,
    autodetect: Option<bool>,
}

/// Pull the rig serial settings from the `[serial]` table. Unparseable
/// baud/profile/autodetect/vid/pid values are treated as unset (the caller's
/// default applies) rather than failing the whole read. vid/pid accept a `0x`
/// hex prefix (how they're written) or plain decimal.
fn parse_serial_config(text: &str) -> SerialFile {
    SerialFile {
        port: parse_table_value(text, "serial", "port"),
        usb_serial: parse_table_value(text, "serial", "usb_serial"),
        usb_vid: parse_table_value(text, "serial", "usb_vid").and_then(|s| parse_u16(&s)),
        usb_pid: parse_table_value(text, "serial", "usb_pid").and_then(|s| parse_u16(&s)),
        baud: parse_table_value(text, "serial", "baud").and_then(|s| s.parse().ok()),
        profile: parse_table_value(text, "serial", "profile").and_then(|s| LineProfile::parse(&s)),
        autodetect: parse_table_value(text, "serial", "autodetect").and_then(|s| s.parse().ok()),
    }
}

/// Parse a `u16` written as `0x10C4` (hex) or plain decimal.
fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Read the persisted `[serial]` rig settings from the config file.
fn read_serial_config(path: &Path) -> SerialFile {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_serial_config(&text),
        Err(_) => SerialFile::default(),
    }
}

/// Rewrite TOML `text` so `table` carries `kvs` (`key`, `value` pairs),
/// **preserving comments** and every other line: existing keys are updated in
/// place (inline comments kept), missing keys are appended to the table, and the
/// `[table]` is created if absent — leaving any other tables untouched. A real
/// `toml_edit` swap-in would subsume this (see `joels-notes.md`).
fn update_toml_table(text: &str, table: &str, kvs: &[(&str, &str)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    let mut seen = false;
    let mut written = vec![false; kvs.len()];
    let mut insert_at: Option<usize> = None; // after the last meaningful [table] line

    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("").trim();
        if let Some(t) = code.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_table = t.trim() == table;
            seen |= in_table;
            out.push(raw.to_string());
            if in_table {
                insert_at = Some(out.len());
            }
            continue;
        }
        if in_table {
            let key = code.split_once('=').map(|(k, _)| k.trim());
            match key.and_then(|k| kvs.iter().position(|(kk, _)| *kk == k)) {
                Some(i) => {
                    out.push(rewrite_kv(raw, kvs[i].1));
                    written[i] = true;
                }
                None => out.push(raw.to_string()),
            }
            if !raw.trim().is_empty() {
                insert_at = Some(out.len());
            }
            continue;
        }
        out.push(raw.to_string());
    }

    let mut missing = Vec::new();
    for (i, (k, v)) in kvs.iter().enumerate() {
        if !written[i] {
            missing.push(format!("{k} = \"{v}\""));
        }
    }
    if !missing.is_empty() {
        if let (true, Some(at)) = (seen, insert_at) {
            for (i, line) in missing.into_iter().enumerate() {
                out.insert(at + i, line);
            }
        } else {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{table}]"));
            out.extend(missing);
        }
    }

    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Comment-preserving `[station]` update, or a fresh commented file when none
/// exists yet.
fn update_station_config(existing: Option<&str>, call: &str, grid: &str) -> String {
    match existing {
        Some(text) => update_toml_table(text, "station", &[("callsign", call), ("grid", grid)]),
        None => default_station_toml(call, grid),
    }
}

/// Persist the editable hardware bindings to the config file: the audio device
/// selections (`[audio]`) and the rig serial settings (`[serial]` — port, baud,
/// line profile, autodetect). Preserves comments and the rest of the file (e.g.
/// `[station]`). Errors are logged, not fatal. Empty selections are written as
/// `""` (system default / autodetect).
pub fn save_hardware_config(cfg: &HardwareConfig) {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# DM420 config — written from the UI; safe to hand-edit.\n".to_string()
    });
    let baud = cfg.serial.baud.to_string();
    // The stable USB identity (captured in `SettingsForm::to_config`) is the
    // durable key; persist it so a replug that renumbers the path still resolves
    // the radio. vid/pid as `0x….`, serial verbatim; empty when not a USB device.
    let usb_vid = cfg.serial.usb_vid.map(|v| format!("0x{v:04X}")).unwrap_or_default();
    let usb_pid = cfg.serial.usb_pid.map(|v| format!("0x{v:04X}")).unwrap_or_default();
    let usb_serial = cfg.serial.usb_serial.as_deref().unwrap_or("");
    // Two passes over the same text, so both tables land in one file while every
    // other table and all comments are preserved.
    let text = update_toml_table(
        &existing,
        "audio",
        &[
            ("input", cfg.audio_input.as_deref().unwrap_or("")),
            ("output", cfg.audio_output.as_deref().unwrap_or("")),
        ],
    );
    let text = update_toml_table(
        &text,
        "serial",
        &[
            ("port", cfg.serial.port.as_deref().unwrap_or("")),
            ("usb_serial", usb_serial),
            ("usb_vid", &usb_vid),
            ("usb_pid", &usb_pid),
            ("baud", &baud),
            ("profile", cfg.serial.profile.label()),
            ("autodetect", bool_str(cfg.serial.autodetect)),
        ],
    );
    write_config(&path, &text);
}

/// The saved display theme: the `[display] dark` key, `true` for the dark
/// ("graphite") palette and `false` for light ("silver"). `None` when unset, so
/// the caller falls back to seeding from the OS appearance. Set the first time
/// the operator flips the DARK/LIGHT toggle ([`save_theme_dark`]).
pub fn read_theme_dark() -> Option<bool> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    parse_table_value(&text, "display", "dark").and_then(|s| s.parse().ok())
}

/// Persist the display theme to the `[display] dark` key, preserving every other
/// table and comment. Best-effort (called when the operator flips the DARK/LIGHT
/// toggle); errors are logged, not fatal.
pub fn save_theme_dark(dark: bool) {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# DM420 config — written from the UI; safe to hand-edit.\n".to_string()
    });
    let text = update_toml_table(&existing, "display", &[("dark", bool_str(dark))]);
    write_config(&path, &text);
}

/// The saved waterslide split: the `[display] waterslide_wide` key. `true` gives
/// the decode (text) side 2/3 of the panel and the spectrogram 1/3; `false` (the
/// default) keeps the centered 1:1 split. Both sides span the same amount of time
/// either way — see `draw_waterslide`. Defaults to `false` when unset/garbled, so
/// a fresh config opens centered. Toggled from the unlocked EDIT surface.
pub fn read_waterslide_wide() -> bool {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| parse_table_value(&t, "display", "waterslide_wide"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(false)
}

/// Persist the waterslide split to `[display] waterslide_wide`, preserving every
/// other table and comment. Best-effort (called when the operator flips it in the
/// EDIT surface); errors are logged, not fatal.
pub fn save_waterslide_wide(wide: bool) {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# DM420 config — written from the UI; safe to hand-edit.\n".to_string()
    });
    let text = update_toml_table(&existing, "display", &[("waterslide_wide", bool_str(wide))]);
    write_config(&path, &text);
}

/// `"true"`/`"false"` for a config bool — the string form `[serial] autodetect`
/// and `[display] dark` are written/read as.
fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// The persisted window inner size (logical points). Read at startup to seed the
/// `ViewportBuilder`, written on exit so the window reopens where it was left.
#[derive(Clone, Copy, PartialEq)]
pub struct WindowSize {
    pub width: f32,
    pub height: f32,
    /// Top-left position in OS screen points, if it was saved. `None` lets the
    /// window manager place the window (first run, or a file without `[window] x`).
    /// May be negative on a multi-monitor desktop, so it isn't sign-checked.
    pub pos: Option<(f32, f32)>,
    /// Whether the window was left in (native) fullscreen. Restored on launch so
    /// the app reopens the way it was closed. `width`/`height`/`pos` still carry
    /// the last *windowed* geometry (not the fullscreen rect), so leaving
    /// fullscreen returns to a sane size.
    pub fullscreen: bool,
}

/// The resizable tile-split proportions, as the raw `egui_tiles` linear shares.
/// Stored relative (not pixels), so they survive a window resize. `band` is
/// re-pinned to a fixed height each frame ([`crate::pin_band_height`]) so its
/// saved value is cosmetic, but kept for a complete record of the layout.
#[derive(Clone, Copy, PartialEq)]
pub struct LayoutShares {
    /// Root horizontal split: Waterfall column vs. the right-hand stack.
    pub waterfall: f32,
    pub right: f32,
    /// Right vertical split: Log Book / Band Scan / Call Sign / Contacts map.
    pub log: f32,
    pub band: f32,
    pub callsign: f32,
    pub contacts: f32,
}

/// Read the saved `[window]` inner size (plus the optional `fullscreen` flag), or
/// `None` if the size is absent/incomplete (then the caller uses the design
/// default). A non-positive or non-finite value is treated as unset so a corrupt
/// file can't open a zero-size window; `fullscreen` defaults off when not present.
pub fn read_window_size() -> Option<WindowSize> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let w = parse_float(&text, "window", "width")?;
    let h = parse_float(&text, "window", "height")?;
    // Position is optional and only honored if both coords are present & finite —
    // a half-written pair is dropped rather than placing the window off-screen.
    let pos = match (
        parse_float(&text, "window", "x"),
        parse_float(&text, "window", "y"),
    ) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => None,
    };
    // Fullscreen is an independent flag; absent/garbled ⇒ windowed.
    let fullscreen = parse_table_value(&text, "window", "fullscreen")
        .and_then(|s| s.parse().ok())
        .unwrap_or(false);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some(WindowSize {
        width: w,
        height: h,
        pos,
        fullscreen,
    })
}

/// Read the saved `[layout]` tile shares. Returns `None` unless every share is
/// present and a positive finite number — a partial/garbled table falls back to
/// the built-in layout rather than a lopsided one.
pub fn read_layout_shares() -> Option<LayoutShares> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let get = |key| parse_float(&text, "layout", key).filter(|v: &f32| v.is_finite() && *v > 0.0);
    Some(LayoutShares {
        waterfall: get("waterfall")?,
        right: get("right")?,
        log: get("log")?,
        band: get("band")?,
        // Default for layout files saved before the Call Sign pane existed, so an
        // older config still loads (shares are relative and re-normalized).
        callsign: get("callsign").unwrap_or(crate::panel_data::CALLSIGN_H),
        contacts: get("contacts")?,
    })
}

/// Persist the window size, fullscreen flag, and tile layout to the `[window]` /
/// `[layout]` tables, preserving every other table and comment. Best-effort:
/// called on exit (the macOS close path bypasses eframe's own save hook), errors
/// are logged.
pub fn save_window_layout(win: WindowSize, layout: LayoutShares) {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# DM420 config — written from the UI; safe to hand-edit.\n".to_string()
    });
    // Width/height always; x/y only when known (otherwise leave any prior value
    // in place rather than overwriting it with a bogus coordinate).
    let mut win_kvs = vec![
        ("width", format_f32(win.width)),
        ("height", format_f32(win.height)),
    ];
    if let Some((x, y)) = win.pos {
        win_kvs.push(("x", format_f32(x)));
        win_kvs.push(("y", format_f32(y)));
    }
    // Always recorded so a windowed close clears a previously-saved fullscreen.
    win_kvs.push(("fullscreen", bool_str(win.fullscreen).to_string()));
    let win_kvs: Vec<(&str, &str)> = win_kvs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let text = update_toml_table(&existing, "window", &win_kvs);
    let text = update_toml_table(
        &text,
        "layout",
        &[
            ("waterfall", &format_f32(layout.waterfall)),
            ("right", &format_f32(layout.right)),
            ("log", &format_f32(layout.log)),
            ("band", &format_f32(layout.band)),
            ("callsign", &format_f32(layout.callsign)),
            ("contacts", &format_f32(layout.contacts)),
        ],
    );
    write_config(&path, &text);
}

/// Parse a numeric value from `table`'s `key` (stored as a quoted string, like
/// every other config value — see [`parse_table_value`]).
fn parse_float(text: &str, table: &str, key: &str) -> Option<f32> {
    parse_table_value(text, table, key).and_then(|v| v.parse::<f32>().ok())
}

/// Format a size/share for the config file: one decimal place, trimmed — keeps
/// the file readable without spurious float noise (`612.0`, not `612.0000305`).
fn format_f32(v: f32) -> String {
    let s = format!("{v:.1}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Rewrite a `key = value` line with a new quoted value, preserving the key, its
/// spacing, and any trailing inline comment.
fn rewrite_kv(raw: &str, new_val: &str) -> String {
    let Some(eq) = raw.find('=') else {
        return raw.to_string();
    };
    let prefix = &raw[..=eq];
    let post = &raw[eq + 1..];
    match post.find('#') {
        Some(h) => format!("{prefix} \"{new_val}\"  {}", post[h..].trim_end()),
        None => format!("{prefix} \"{new_val}\""),
    }
}

/// A fresh config file with explanatory comments, when none exists yet.
fn default_station_toml(call: &str, grid: &str) -> String {
    format!(
        "# DM420 station identity — written from the UI; safe to hand-edit.\n\
         # No built-in default: DM420 won't call CQ or answer until a callsign is set.\n\n\
         [station]\n\
         callsign = \"{call}\"\n\
         grid = \"{grid}\"\n\n\
         # Raw decode/transmit archive (diagnostics): set `decodes` to a file path to\n\
         # append every heard + sent FT8/FT4 message as JSONL. Blank = disabled (default).\n\
         [archive]\n\
         decodes = \"\"\n"
    )
}

fn serial_from_env() -> SerialConfig {
    // Precedence per field: `DM420_SERIAL_*` env var (quick per-launch override) →
    // the persisted `[serial]` table (written from the unlocked UI) → built-in
    // default. `autodetect` has no env var; it comes from config, defaulting on.
    let file = read_serial_config(&config_path());

    let port = env_nonempty("DM420_SERIAL_PORT").or(file.port);

    let baud = match env_nonempty("DM420_SERIAL_BAUD") {
        Some(s) => s.parse::<u32>().unwrap_or_else(|_| {
            tracing::warn!(value = %s, "DM420_SERIAL_BAUD is not a number; using {DEFAULT_BAUD}");
            DEFAULT_BAUD
        }),
        None => file.baud.unwrap_or(DEFAULT_BAUD),
    };

    let profile = match env_nonempty("DM420_SERIAL_PROFILE") {
        Some(s) => LineProfile::parse(&s).unwrap_or_else(|| {
            tracing::warn!(
                value = %s,
                "DM420_SERIAL_PROFILE unknown (use none|dtr-rts|rtscts); using default"
            );
            LineProfile::Default
        }),
        None => file.profile.unwrap_or(LineProfile::Default),
    };

    SerialConfig {
        port,
        // The stable USB identity captured when the device was last picked. Tried
        // before `port`, so a replug that renumbers the path still finds the radio.
        usb_serial: file.usb_serial,
        usb_vid: file.usb_vid,
        usb_pid: file.usb_pid,
        baud,
        profile,
        // Default on so the operator isn't stuck guessing a port/baud; an explicit
        // port is still tried first when the autodetect sweep runs.
        autodetect: file.autodetect.unwrap_or(true),
    }
}

fn protocol_from_env() -> Protocol {
    match env_nonempty("DM420_MODE") {
        Some(s) => match s.trim().to_lowercase().as_str() {
            "ft8" => Protocol::Ft8,
            "ft4" => Protocol::Ft4,
            _ => {
                tracing::warn!(value = %s, "DM420_MODE unknown (use ft8|ft4); using ft8");
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
        tracing::warn!(path = %p.display(), "DM420_WAV does not exist; using live capture");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_station_table_with_comments() {
        let cfg = "# header\n[station]\ncallsign = \"w4ll\"  # my call\ngrid = \"EM73\"\n";
        let (call, grid) = parse_station_config(cfg);
        assert_eq!(call.as_deref(), Some("w4ll"));
        assert_eq!(grid.as_deref(), Some("EM73"));
    }

    #[test]
    fn ignores_other_tables_and_blank_values() {
        let cfg = "[other]\ncallsign = \"X\"\n\n[station]\ngrid = \"\"\n";
        let (call, grid) = parse_station_config(cfg);
        assert_eq!(call, None, "a callsign under [other] must not leak in");
        assert_eq!(grid, None, "an empty value is treated as unset");
    }

    #[test]
    fn empty_config_is_unset() {
        assert_eq!(parse_station_config(""), (None, None));
    }

    #[test]
    fn update_preserves_comments_and_round_trips() {
        let original = "# top\n[station]\ncallsign = \"OLD\"  # my call\ngrid = \"AA00\"\n";
        let updated = update_station_config(Some(original), "W4LL", "EM73");
        assert!(
            updated.contains("callsign = \"W4LL\"  # my call"),
            "inline comment kept: {updated}"
        );
        assert!(updated.contains("# top"), "header comment kept: {updated}");
        assert_eq!(
            parse_station_config(&updated),
            (Some("W4LL".to_string()), Some("EM73".to_string()))
        );
    }

    #[test]
    fn update_appends_missing_keys_and_table() {
        // Missing grid in an existing table → appended.
        let a = update_station_config(Some("[station]\ncallsign = \"X\"\n"), "X", "EM73");
        assert_eq!(
            parse_station_config(&a),
            (Some("X".to_string()), Some("EM73".to_string()))
        );
        // No file → fresh template.
        let b = update_station_config(None, "W4LL", "EM73");
        assert!(b.contains("[station]"));
        assert_eq!(
            parse_station_config(&b),
            (Some("W4LL".to_string()), Some("EM73".to_string()))
        );
        // File without a [station] table → table appended, existing note kept.
        let c = update_station_config(Some("# just a note\n"), "W4LL", "EM73");
        assert!(c.contains("# just a note"));
        assert_eq!(
            parse_station_config(&c),
            (Some("W4LL".to_string()), Some("EM73".to_string()))
        );
    }

    #[test]
    fn parses_serial_table_with_all_fields() {
        let cfg = "[serial]\nport = \"/dev/cu.usbserial-120\"\nbaud = \"9600\"\n\
                   profile = \"dtr-rts\"\nautodetect = \"false\"\n";
        let s = parse_serial_config(cfg);
        assert_eq!(s.port.as_deref(), Some("/dev/cu.usbserial-120"));
        assert_eq!(s.baud, Some(9600));
        assert_eq!(s.profile, Some(LineProfile::AssertDtrRts));
        assert_eq!(s.autodetect, Some(false));
    }

    #[test]
    fn serial_partial_and_garbage_values_are_unset() {
        // Missing keys → None; an empty port → None; a non-numeric baud → None.
        let cfg = "[serial]\nport = \"\"\nbaud = \"fast\"\n";
        let s = parse_serial_config(cfg);
        assert_eq!(s.port, None, "empty port is treated as unset (autodetect)");
        assert_eq!(s.baud, None, "unparseable baud falls back to the default");
        assert_eq!(s.profile, None);
        assert_eq!(s.autodetect, None);
    }

    #[test]
    fn serial_round_trips_through_the_toml_writer() {
        // Mirror what `save_hardware_config` writes, then read it back.
        let text = update_toml_table(
            "",
            "serial",
            &[
                ("port", "/dev/ttyUSB0"),
                ("usb_serial", "52238a72"),
                ("usb_vid", "0x10C4"),
                ("usb_pid", "0xEA60"),
                ("baud", "19200"),
                ("profile", LineProfile::Default.label()),
                ("autodetect", "true"),
            ],
        );
        let s = parse_serial_config(&text);
        assert_eq!(s.port.as_deref(), Some("/dev/ttyUSB0"));
        assert_eq!(s.usb_serial.as_deref(), Some("52238a72"));
        assert_eq!(s.usb_vid, Some(0x10C4));
        assert_eq!(s.usb_pid, Some(0xEA60));
        assert_eq!(s.baud, Some(19_200));
        assert_eq!(s.profile, Some(LineProfile::Default));
        assert_eq!(s.autodetect, Some(true));
    }

    #[test]
    fn display_theme_round_trips_and_preserves_other_tables() {
        // The writer adds [display] without disturbing [station]; the reader gets
        // the bool back. (Mirrors what `save_theme_dark` writes via the same path.)
        let text = update_toml_table("# top\n[station]\ncallsign = \"W4LL\"\n", "display", &[("dark", "false")]);
        assert!(text.contains("# top"), "header comment kept: {text}");
        assert_eq!(
            parse_station_config(&text),
            (Some("W4LL".to_string()), None),
            "station table survives: {text}"
        );
        assert_eq!(parse_table_value(&text, "display", "dark").as_deref(), Some("false"));
        // Flip it: the existing key is rewritten in place, not duplicated.
        let flipped = update_toml_table(&text, "display", &[("dark", "true")]);
        assert_eq!(flipped.matches("dark =").count(), 1, "key rewritten, not appended: {flipped}");
        assert_eq!(
            parse_table_value(&flipped, "display", "dark").and_then(|s| s.parse::<bool>().ok()),
            Some(true)
        );
    }

    #[test]
    fn window_fullscreen_flag_round_trips_with_size() {
        // Mirror what `save_window_layout` writes for the [window] table, then read
        // the pieces back the way `read_window_size` does.
        let text = update_toml_table(
            "",
            "window",
            &[("width", "1200"), ("height", "800"), ("fullscreen", "true")],
        );
        assert_eq!(parse_float(&text, "window", "width"), Some(1200.0));
        assert_eq!(parse_float(&text, "window", "height"), Some(800.0));
        assert_eq!(
            parse_table_value(&text, "window", "fullscreen").and_then(|s| s.parse::<bool>().ok()),
            Some(true)
        );
        // A windowed close flips it back to false in place (not duplicated).
        let windowed = update_toml_table(&text, "window", &[("fullscreen", "false")]);
        assert_eq!(windowed.matches("fullscreen =").count(), 1, "rewritten: {windowed}");
        assert_eq!(
            parse_table_value(&windowed, "window", "fullscreen").and_then(|s| s.parse::<bool>().ok()),
            Some(false)
        );
    }

    #[test]
    fn usb_ids_parse_hex_and_decimal() {
        assert_eq!(parse_u16("0x10C4"), Some(0x10C4));
        assert_eq!(parse_u16("0X10c4"), Some(0x10C4));
        assert_eq!(parse_u16("4292"), Some(4292));
        assert_eq!(parse_u16("nope"), None);
    }
}
