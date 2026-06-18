//! The logbook: the persistent contact store.
//!
//! Owns `logbook/entries` in real mode. The QSO engine publishes a [`LogEntry`]
//! on `RR73` (received when we answered, sent when we called CQ — see
//! `docs/qso_flow.md` §7); this crate subscribes to that stream, dedups by
//! [`QsoId`], and persists the whole log to a JSON file so contacts survive a
//! restart (it matters for Field Day). On startup it loads the file and replays
//! each entry back onto the bus so panels that just subscribed render history.
//!
//! Dedup-by-`QsoId` makes the startup replay idempotent: we observe our own
//! replays through the same subscription, but a re-seen id is a no-op and is
//! never re-persisted.
//!
//! Not yet built: ADIF import/export and the G-set merge that folds in gossiped
//! peer contacts (OVERVIEW §7). The on-disk format is plain JSON for now; an
//! ADIF exporter can read the same store later.
//!
//! Specs: `docs/log_book.md`, `docs/message-catalog.md` §7.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bus::types::{LogEntry, QsoId};
use bus::{BusError, BusHandle, Topic, TopicSelector};

/// Launch the logbook producer onto `bus`, persisting to `path` (JSON). Spawns a
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

    let mut store = load(&path);
    let mut seen: HashSet<QsoId> = store.iter().map(|e| e.id.clone()).collect();
    tracing::info!(
        "logbook: loaded {} entries from {}",
        store.len(),
        path.display()
    );

    // Replay history so a panel that just subscribed renders past contacts.
    for entry in &store {
        let _ = bus.publish(&Topic::LogbookEntries, entry.clone());
    }

    loop {
        match sub.recv().await {
            Ok(entry) => {
                // New contact (not a replay, not a network dupe) → record + persist.
                if seen.insert(entry.id.clone()) {
                    store.push(entry);
                    if let Err(e) = save(&path, &store) {
                        tracing::warn!("logbook: save to {} failed: {e}", path.display());
                    }
                }
            }
            // Lossless stream, but be exhaustive: a closed channel ends the task.
            Err(BusError::Lagged { .. }) => continue,
            Err(_) => break,
        }
    }
}

/// Load the persisted log, or an empty log if the file is missing or unreadable.
/// A corrupt file is reported and treated as empty rather than crashing the app —
/// the operator keeps logging; only the unreadable history is lost.
fn load(path: &Path) -> Vec<LogEntry> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(
                "logbook: {} is not valid JSON ({e}); starting with an empty log",
                path.display()
            );
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// Persist the whole log atomically: write a sibling temp file, then rename over
/// the target, so a crash mid-write can't truncate or corrupt the log.
fn save(path: &Path, store: &[LogEntry]) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
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
        std::env::temp_dir().join(format!("dm420-logbook-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = scratch("roundtrip");
        let _ = std::fs::remove_file(&path);
        let log = vec![entry(1), entry(2), entry(3)];
        save(&path, &log).unwrap();
        assert_eq!(load(&path), log);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).is_empty());
    }

    #[test]
    fn load_corrupt_file_is_empty() {
        let path = scratch("corrupt");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_creates_missing_parent_dir() {
        let dir = std::env::temp_dir().join(format!("dm420-lb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("logbook.json");
        save(&path, &[entry(1)]).unwrap();
        assert_eq!(load(&path), vec![entry(1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
