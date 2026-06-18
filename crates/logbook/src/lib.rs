//! The logbook: the persistent contact store.
//!
//! Owns `logbook/entries` in real mode. The QSO engine publishes a [`LogEntry`]
//! on `RR73` (received when we answered, sent when we called CQ — see
//! `docs/qso_flow.md` §7); this crate subscribes to that stream, dedups by
//! [`QsoId`], and persists the log so contacts survive a restart (it matters for
//! Field Day). On startup it loads the file and replays each entry back onto the
//! bus so panels that just subscribed render history.
//!
//! On-disk format is **JSONL** — one JSON `LogEntry` per line. Each new contact is
//! a single appended line, so a crash mid-write can at worst leave a partial
//! trailing line (which [`load`] skips); the rest of the log survives. A legacy
//! whole-array `.json` file is read and migrated to JSONL in place on startup.
//!
//! Dedup-by-`QsoId` makes the startup replay idempotent: we observe our own
//! replays through the same subscription, but a re-seen id is a no-op and is
//! never re-appended. (This requires `QsoId`s to be unique per contact across
//! sessions — see `qso::shell`, which seeds the sequence from the wall clock.)
//!
//! Not yet built: ADIF import/export and the G-set merge that folds in gossiped
//! peer contacts (OVERVIEW §7). An ADIF exporter can read the same store later.
//!
//! Specs: `docs/log_book.md`, `docs/message-catalog.md` §7.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use bus::types::{LogEntry, QsoId};
use bus::{BusError, BusHandle, Topic, TopicSelector};

/// Launch the logbook producer onto `bus`, persisting to `path` (JSONL). Spawns a
/// detached tokio task, so it must be called from within a runtime context (like
/// `core::spawn`). Loads any existing log and replays it onto `logbook/entries`.
pub fn spawn(bus: &BusHandle, path: PathBuf) {
    tokio::spawn(run(bus.clone(), path));
}

async fn run(bus: BusHandle, path: PathBuf) {
    // Subscribe *before* replaying history so the engine's live entries and our
    // own replays all arrive through one path; dedup makes the replay a no-op.
    let mut sub = match bus.subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("logbook: cannot subscribe entries: {e:?}");
            return;
        }
    };

    let entries = load(&path);
    let mut seen: HashSet<QsoId> = entries.iter().map(|e| e.id.clone()).collect();
    tracing::info!(
        "logbook: loaded {} entries from {}",
        entries.len(),
        path.display()
    );

    // Normalize on-disk storage to JSONL once at startup: migrates a legacy
    // whole-array file in place, and leaves an already-JSONL file equivalent — so
    // every later write can be a clean append.
    if !entries.is_empty()
        && let Err(e) = rewrite_jsonl(&path, &entries)
    {
        tracing::warn!("logbook: normalize to JSONL at {} failed: {e}", path.display());
    }

    // Replay history so a panel that just subscribed renders past contacts.
    for entry in &entries {
        let _ = bus.publish(&Topic::LogbookEntries, entry.clone());
    }
    drop(entries); // `seen` carries dedup state; new contacts append a line below.

    loop {
        match sub.recv().await {
            Ok(entry) => {
                // New contact (not a replay, not a network dupe) → append one line.
                if seen.insert(entry.id.clone())
                    && let Err(e) = append_entry(&path, &entry)
                {
                    tracing::warn!("logbook: append to {} failed: {e}", path.display());
                }
            }
            // Lossless stream, but be exhaustive: a closed channel ends the task.
            Err(BusError::Lagged { .. }) => continue,
            Err(_) => break,
        }
    }
}

/// Load the persisted log. Reads JSONL (one entry per line, the current format)
/// and a legacy single JSON array (migrated to JSONL on startup). A missing file
/// is an empty log; an unparseable line is skipped with a warning rather than
/// discarding the whole log, so one torn line can't lose every contact.
fn load(path: &Path) -> Vec<LogEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    // Legacy format: the whole log as one pretty-printed JSON array.
    if text.trim_start().starts_with('[') {
        return serde_json::from_str(text.trim()).unwrap_or_else(|e| {
            tracing::warn!(
                "logbook: {} is not valid JSON ({e}); starting with an empty log",
                path.display()
            );
            Vec::new()
        });
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<LogEntry>(l) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(
                    "logbook: skipping unparseable line in {}: {e}",
                    path.display()
                );
                None
            }
        })
        .collect()
}

/// Append one contact as a single JSON line. Append-only, so a crash mid-write
/// leaves at most a partial trailing line that [`load`] skips.
fn append_entry(path: &Path, entry: &LogEntry) -> std::io::Result<()> {
    ensure_parent(path)?;
    let mut line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())
}

/// Rewrite the whole log as JSONL atomically (temp file + rename), used once at
/// startup to normalize/migrate; steady-state writes are appends.
fn rewrite_jsonl(path: &Path, store: &[LogEntry]) -> std::io::Result<()> {
    ensure_parent(path)?;
    let mut buf = String::new();
    for e in store {
        buf.push_str(
            &serde_json::to_string(e)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        );
        buf.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use bus::types::{AbsHz, Band, Callsign, GridSquare, OverAirMode, StationId, Timestamp};

    fn entry(seq: u64) -> LogEntry {
        LogEntry {
            id: QsoId {
                origin: StationId("me".into()),
                seq,
            },
            origin: StationId("me".into()),
            radio: None,
            call: Callsign(format!("K{seq}ABC")),
            mode: OverAirMode::Ft8,
            band: Band::B20m,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000 + seq as i64),
            exchange_sent: "-10".into(),
            exchange_rcvd: "-12".into(),
            grid: Some(GridSquare("FN31".into())),
        }
    }

    /// Unique scratch path per test (process id + name); no global temp clashes.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dm420-logbook-{}-{name}.jsonl", std::process::id()))
    }

    #[test]
    fn append_then_load_round_trips() {
        let path = scratch("roundtrip");
        let _ = std::fs::remove_file(&path);
        let log = vec![entry(1), entry(2), entry(3)];
        for e in &log {
            append_entry(&path, e).unwrap();
        }
        assert_eq!(load(&path), log);
        // Each contact is its own line.
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).is_empty());
    }

    #[test]
    fn corrupt_trailing_line_keeps_prior_entries() {
        let path = scratch("corrupt-tail");
        let _ = std::fs::remove_file(&path);
        append_entry(&path, &entry(1)).unwrap();
        append_entry(&path, &entry(2)).unwrap();
        // Simulate a torn write: a partial, newline-less final line.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"id\":{\"orig").unwrap();
        assert_eq!(load(&path), vec![entry(1), entry(2)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_json_array_is_read_then_migrated() {
        let path = scratch("legacy");
        let _ = std::fs::remove_file(&path);
        let log = vec![entry(1), entry(2)];
        // Old on-disk format: one pretty-printed JSON array.
        std::fs::write(&path, serde_json::to_vec_pretty(&log).unwrap()).unwrap();
        assert_eq!(load(&path), log); // read the legacy array
        rewrite_jsonl(&path, &log).unwrap(); // migrate in place
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(load(&path), log); // now JSONL, same contents
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_creates_missing_parent_dir() {
        let dir = std::env::temp_dir().join(format!("dm420-lb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("logbook.jsonl");
        append_entry(&path, &entry(1)).unwrap();
        assert_eq!(load(&path), vec![entry(1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
