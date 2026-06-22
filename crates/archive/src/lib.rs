//! The decode/transmit archive: a raw, append-only JSONL capture of every heard
//! and sent FT8/FT4 message, for offline diagnostics and analysis.
//!
//! Opt-in and **off by default** — `core::spawn` starts it only when a path is
//! configured (`[archive] decodes` in the config TOML). One JSON object per line:
//! a heard [`Decode`] or a sent [`TxLogEntry`], wrapped in an envelope that stamps
//! the capture time and the current dial frequency / rig mode (the decode itself
//! carries only the *audio* offset, so absolute RF = `dial_hz + offset`).
//!
//! Mirrors the `logbook` crate's on-disk discipline — append-only, one line per
//! event, parent dir created on first write — so `tail -f` shows live traffic and a
//! crash mid-write loses at most a partial trailing line. Unlike the logbook it
//! does **not** dedup or replay: it's a firehose, and duplicates/ordering are
//! themselves diagnostic signal. Not grouped by QSO (that's a later analysis step).
//!
//! Heard messages ride `radio/{id}/decodes`; sent messages ride
//! `radio/{id}/tx_log` (published by `core::tx`, deliberately off the `Decodes`
//! topic the live QSO engine consumes). Both are `StreamLossless`, so the archive
//! sees every message in order.

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use bus::types::{AbsHz, Decode, RadioId, RigMode, RigState, TxLogEntry};
use bus::{BusError, BusHandle, Topic, TopicSelector};
use serde::Serialize;

/// Launch the archive producer onto `bus`, appending every heard + sent message
/// for `radio` to `path` (JSONL). Spawns a detached tokio task, so it must be
/// called from within a runtime context (like `core::spawn`).
pub fn spawn(bus: &BusHandle, radio: RadioId, path: PathBuf) {
    tokio::spawn(run(bus.clone(), radio, path));
}

/// One archived line: an envelope around a heard [`Decode`] or a sent
/// [`TxLogEntry`]. `direction` says which; `event` is the raw payload verbatim.
#[derive(Serialize)]
struct Row<'a, T: Serialize> {
    /// When this line was written: RFC3339 UTC with millisecond precision, e.g.
    /// `"2026-06-22T14:30:00.123Z"`.
    captured_at: String,
    /// `"heard"` or `"sent"`.
    direction: &'a str,
    /// Dial/VFO frequency at capture time (absolute Hz). The event carries only the
    /// audio offset, so absolute RF = `dial_hz + offset`. `None` until the first
    /// `RigState` arrives.
    dial_hz: Option<AbsHz>,
    /// Rig mode at capture time. `None` until the first `RigState` arrives.
    rig_mode: Option<RigMode>,
    /// The raw heard/sent payload.
    event: &'a T,
}

async fn run(bus: BusHandle, radio: RadioId, path: PathBuf) {
    let mut decodes =
        match bus.subscribe::<Decode>(TopicSelector::Exact(Topic::Decodes(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("archive: cannot subscribe decodes: {e:?}");
                return;
            }
        };
    let mut tx_log =
        match bus.subscribe::<TxLogEntry>(TopicSelector::Exact(Topic::TxLog(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("archive: cannot subscribe tx_log: {e:?}");
                return;
            }
        };
    let mut rig =
        match bus.subscribe::<RigState>(TopicSelector::Exact(Topic::RigState(radio.clone()))) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("archive: cannot subscribe rig_state: {e:?}");
                return;
            }
        };

    // Latest dial/mode, so each row can record the absolute-frequency context the
    // decode itself lacks (it carries only the audio offset).
    let mut latest_rig: Option<RigState> = None;
    // Once the rig stream closes we stop polling that branch (a closed channel would
    // otherwise busy-loop) but keep archiving decodes/TX without dial info.
    let mut rig_open = true;

    tracing::info!(
        radio = %radio.0,
        "archive: capturing heard + sent messages to {}",
        path.display()
    );

    loop {
        tokio::select! {
            r = decodes.recv() => match r {
                Ok(d) => write_row(&path, "heard", latest_rig.as_ref(), &d),
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = tx_log.recv() => match r {
                Ok(e) => write_row(&path, "sent", latest_rig.as_ref(), &e),
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            },
            r = rig.recv(), if rig_open => match r {
                Ok(s) => latest_rig = Some(s),
                Err(BusError::Lagged { .. }) => {} // missed a State update; the next one refreshes it
                Err(_) => rig_open = false,         // rig gone; keep archiving without dial info
            },
        }
    }

    tracing::info!("archive: capture stopped");
}

/// Build the envelope and append it. Best-effort: a write error is logged, never
/// fatal — the archive must never take down the operating path.
fn write_row<T: Serialize>(path: &Path, direction: &str, rig: Option<&RigState>, event: &T) {
    let row = Row {
        captured_at: now_rfc3339(),
        direction,
        dial_hz: rig.map(|r| r.vfo),
        rig_mode: rig.map(|r| r.rig_mode),
        event,
    };
    if let Err(e) = append_line(path, &row) {
        tracing::warn!("archive: append to {} failed: {e}", path.display());
    }
}

/// Append one JSON object as a single line. Append-only with a fresh open per line
/// (like the logbook): durable per line so `tail -f` shows it immediately, and a
/// crash leaves at most a partial trailing line.
fn append_line<T: Serialize>(path: &Path, row: &T) -> std::io::Result<()> {
    ensure_parent(path)?;
    let mut line = serde_json::to_string(row)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())
}

/// Create the file's parent directory if it has one.
fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// Current wall-clock time as RFC3339 UTC with millisecond precision, e.g.
/// `"2026-06-22T14:30:00.123Z"`.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
