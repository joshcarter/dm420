//! Logging setup — the one place a `tracing` subscriber is installed.
//!
//! Every crate emits through the `tracing` facade (`trace!`…`error!`); this
//! module wires those events to a file. Output goes to **`dm420.log`** in the
//! launch directory, **appended across runs**; each run opens with a timestamped
//! `DM420 starting` line, so sessions are easy to tell apart (and to hand to a
//! developer — or to Claude — to read back).
//!
//! ## Level
//!
//! - `RUST_LOG`, if set, wins outright (standard [`EnvFilter`] syntax, e.g.
//!   `RUST_LOG=core::tx=debug,info`).
//! - otherwise the `[logging] level` from the config file (default `info`) sets the
//!   level for **DM420's own crates**, while third-party crates (egui, winit,
//!   wgpu, tokio, cpal…) are pinned at `warn` so the log stays readable.
//!
//! So the day-to-day knob is one TOML line; `RUST_LOG` is the escape hatch for
//! targeted, per-module debugging.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

/// Log file, relative to the launch directory.
const LOG_FILE: &str = "dm420.log";

/// DM420's own crates — the ones the `[logging] level` applies to. Everything
/// not listed stays at `warn`, keeping the log free of framework chatter.
const OUR_CRATES: &[&str] = &[
    "dm420", "gui", "core", "rig", "audio", "dsp", "modes", "qso", "bus", "mocks", "types",
];

/// Install the file logger. Returns a [`WorkerGuard`] that **must be held for the
/// program's lifetime** — dropping it flushes the background writer. Never
/// panics: if `dm420.log` can't be opened it falls back to stderr so logging
/// still works (just not to file).
#[must_use]
pub fn init() -> Option<WorkerGuard> {
    let rust_log = std::env::var("RUST_LOG").ok();
    let directive = build_directive(&crate::settings::log_level(), rust_log.as_deref());

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
    {
        Ok(file) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
            tracing_subscriber::fmt()
                .with_env_filter(make_filter(&directive))
                .with_writer(writer)
                .with_ansi(false) // a file, not a terminal — no colour escapes
                .with_target(true) // show the emitting module, e.g. `core::tx`
                .with_thread_names(true) // tell audio-play / rig / tokio threads apart
                .with_timer(UtcTime)
                .init();
            Some(guard)
        }
        Err(e) => {
            tracing_subscriber::fmt()
                .with_env_filter(make_filter(&directive))
                .with_timer(UtcTime)
                .init();
            tracing::warn!("could not open {LOG_FILE}: {e}; logging to stderr instead");
            None
        }
    }
}

/// Build the `EnvFilter` directive string: `RUST_LOG` verbatim if set and valid,
/// otherwise `warn` globally with DM420's crates raised to `level`.
fn build_directive(level: &str, rust_log: Option<&str>) -> String {
    if let Some(s) = rust_log {
        let s = s.trim();
        if !s.is_empty() && EnvFilter::try_new(s).is_ok() {
            return s.to_string();
        }
    }
    let level = normalize_level(level);
    let mut directive = String::from("warn");
    for crate_name in OUR_CRATES {
        directive.push_str(&format!(",{crate_name}={level}"));
    }
    directive
}

/// Parse a directive, falling back to a plain `info` filter if it's somehow
/// invalid (it shouldn't be — we build it ourselves).
fn make_filter(directive: &str) -> EnvFilter {
    EnvFilter::try_new(directive).unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Map a user-supplied level word to a canonical level, defaulting unknown
/// values to `info` rather than erroring.
fn normalize_level(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "warn" | "warning" => "warn",
        "error" => "error",
        _ => "info",
    }
}

/// Compact UTC wall-clock timestamps (`HH:MM:SS.mmm`). FT8 is UTC-centric and the
/// app's clock reads UTC, so the log lines line up with on-air slot timing.
struct UtcTime;

impl FormatTime for UtcTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Utc::now().format("%H:%M:%S%.3f"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_level_defaults_to_info() {
        assert_eq!(normalize_level("debug"), "debug");
        assert_eq!(normalize_level("DEBUG"), "debug");
        assert_eq!(normalize_level("warning"), "warn");
        assert_eq!(normalize_level("nonsense"), "info");
        assert_eq!(normalize_level(""), "info");
    }

    #[test]
    fn directive_scopes_our_crates_and_pins_the_rest() {
        let d = build_directive("debug", None);
        assert!(d.starts_with("warn"), "third-party pinned at warn: {d}");
        assert!(d.contains("core=debug"), "our crates raised: {d}");
        assert!(d.contains("dm420=debug"));
        // The default level keeps our crates at info.
        assert!(build_directive("info", None).contains("audio=info"));
        // An unknown level still yields a valid, info-scoped directive.
        assert!(build_directive("bogus", None).contains("rig=info"));
    }

    #[test]
    fn rust_log_overrides_when_valid() {
        // A valid RUST_LOG passes through verbatim…
        assert_eq!(
            build_directive("info", Some("core::tx=debug")),
            "core::tx=debug"
        );
        // …but a blank one falls back to the scoped default.
        assert!(build_directive("info", Some("  ")).contains("core=info"));
    }

    /// Prove the actual file-writing mechanics (fmt + our timer + filter →
    /// formatted lines on disk), using a thread-local subscriber so it doesn't
    /// install or conflict with the global one that `init` sets.
    #[test]
    fn writes_formatted_lines_to_a_file() {
        use std::io::Read;
        let path =
            std::env::temp_dir().join(format!("dm420-logtest-{}.log", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let make = move || file.try_clone().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(make)
            .with_ansi(false)
            .with_timer(UtcTime)
            .with_env_filter(make_filter(&build_directive("debug", None)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "core", marker = 42, "tx test line");
        });
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(s.contains("tx test line"), "message written: {s:?}");
        assert!(s.contains("marker=42"), "fields written: {s:?}");
        assert!(s.contains("core"), "target written: {s:?}");
    }
}
