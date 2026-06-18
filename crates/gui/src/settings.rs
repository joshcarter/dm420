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

use std::path::{Path, PathBuf};

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
    /// Load the operator's station identity, in precedence order: the
    /// `DM420_CALLSIGN` / `DM420_GRID` env vars → the `[station]` table in
    /// `dm420.toml` (current dir) → unset. **There is no default** — a silent one
    /// risks transmitting as the wrong station. Operating is blocked until a call
    /// is set (typed into the unlocked top bar, or written to `dm420.toml`). The
    /// config format/persistence is interim and TBD — see `joels-notes.md`.
    pub fn load() -> Self {
        let (toml_call, toml_grid) = read_station_config(Path::new("dm420.toml"));
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

    /// Persist the current identity to `dm420.toml`, preserving comments and any
    /// other content. Called on GUI re-lock so UI edits survive a restart; write
    /// errors are logged, not fatal.
    pub fn save(&self) {
        let path = Path::new("dm420.toml");
        let existing = std::fs::read_to_string(path).ok();
        let text = update_station_config(existing.as_deref(), &self.call, &self.grid);
        if let Err(e) = std::fs::write(path, &text) {
            eprintln!("dm420: could not write {}: {e}", path.display());
        }
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

/// Pull `callsign` / `grid` from a `[station]` table. **Not** a full TOML parser —
/// it deliberately avoids adding a dependency for a format that is still TBD (see
/// `joels-notes.md`); swap in the `toml` crate when the config grows.
fn parse_station_config(text: &str) -> (Option<String>, Option<String>) {
    let (mut call, mut grid) = (None, None);
    let mut in_station = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(table) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_station = table.trim() == "station";
            continue;
        }
        if !in_station {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let val = val.trim().trim_matches('"').trim();
            if val.is_empty() {
                continue;
            }
            match key.trim() {
                "callsign" | "call" => call = Some(val.to_string()),
                "grid" => grid = Some(val.to_string()),
                _ => {}
            }
        }
    }
    (call, grid)
}

/// Rewrite `dm420.toml` content to carry `call`/`grid`, **preserving comments** and
/// every other line: existing `callsign`/`grid` in `[station]` are updated in place
/// (inline comments kept), any missing key is appended to the table, and a
/// `[station]` table — or the whole file — is created if absent. Pairs with
/// [`parse_station_config`]; a real `toml_edit` swap-in would subsume both.
fn update_station_config(existing: Option<&str>, call: &str, grid: &str) -> String {
    let Some(text) = existing else {
        return default_station_toml(call, grid);
    };
    let mut out: Vec<String> = Vec::new();
    let mut in_station = false;
    let mut seen_station = false;
    let mut wrote_call = false;
    let mut wrote_grid = false;
    let mut insert_at: Option<usize> = None; // after the last meaningful [station] line

    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("").trim();
        if let Some(table) = code.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_station = table.trim() == "station";
            seen_station |= in_station;
            out.push(raw.to_string());
            if in_station {
                insert_at = Some(out.len());
            }
            continue;
        }
        if in_station {
            match code.split_once('=').map(|(k, _)| k.trim()) {
                Some("callsign") | Some("call") => {
                    out.push(rewrite_kv(raw, call));
                    wrote_call = true;
                }
                Some("grid") => {
                    out.push(rewrite_kv(raw, grid));
                    wrote_grid = true;
                }
                _ => out.push(raw.to_string()),
            }
            if !raw.trim().is_empty() {
                insert_at = Some(out.len());
            }
            continue;
        }
        out.push(raw.to_string());
    }

    let mut missing = Vec::new();
    if !wrote_call {
        missing.push(format!("callsign = \"{call}\""));
    }
    if !wrote_grid {
        missing.push(format!("grid = \"{grid}\""));
    }
    if !missing.is_empty() {
        if let (true, Some(at)) = (seen_station, insert_at) {
            for (i, line) in missing.into_iter().enumerate() {
                out.insert(at + i, line);
            }
        } else {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push("[station]".to_string());
            out.extend(missing);
        }
    }

    let mut s = out.join("\n");
    s.push('\n');
    s
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

/// A fresh `dm420.toml` with explanatory comments, when no file exists yet.
fn default_station_toml(call: &str, grid: &str) -> String {
    format!(
        "# DM420 station identity — written from the UI; safe to hand-edit.\n\
         # No built-in default: DM420 won't call CQ or answer until a callsign is set.\n\n\
         [station]\n\
         callsign = \"{call}\"\n\
         grid = \"{grid}\"\n"
    )
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
        eprintln!(
            "dm420: DM420_WAV='{}' does not exist; using live capture",
            p.display()
        );
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
}
