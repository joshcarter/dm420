//! End-to-end regression for the lossless **lag-not-evict** contract, from the
//! logbook's point of view.
//!
//! The logbook subscribes to `logbook/entries`, then replays the whole on-disk log
//! back onto the bus (so late-subscribed panels render history). A large log bursts
//! that republish past the bus's live-tail budget. Under the OLD "evict an
//! overflowing lossless subscriber" policy this deleted TWO healthy subscribers
//! mid-burst — the logbook's OWN loopback (so it stopped persisting new contacts)
//! and any non-draining consumer (e.g. a momentarily-frozen GUI pump). Under the
//! lag-not-evict policy both merely lag and stay subscribed.
//!
//! This asserts: after a burst larger than the live tail, (a) a freshly-worked
//! contact is still appended to disk (the writer survived) and (b) a consumer that
//! did NOT drain during the burst still receives that post-burst contact.

use std::path::PathBuf;
use std::time::Duration;

use bus::types::{AbsHz, Band, Callsign, GridSquare, LogEntry, OverAirMode, QsoId, StationId, Timestamp};
use bus::{BusError, BusHandle, Topic, TopicSelector};

/// LogbookEntries live-tail budget = ring_capacity(4096).max(1024) = 4096; go past
/// it (and far past the old per-subscriber cap of 1024) to force the overflow.
const REPLAY_N: u64 = 5000;
/// Unique callsign for the post-burst contact — no replay entry uses it, so a plain
/// substring check over the JSONL file is enough to detect it landing on disk.
const FRESH_CALL: &str = "N0FRESH";

fn make_entry(seq: u64, call: &str) -> LogEntry {
    LogEntry {
        id: QsoId {
            origin: StationId("me".into()),
            seq,
        },
        radio: None,
        call: Callsign(call.into()),
        mode: OverAirMode::Ft8,
        band: Band::B20m,
        freq: AbsHz(14_074_000),
        time: Timestamp(1_700_000_000_000 + seq as i64),
        exchange_sent: "-10".into(),
        exchange_rcvd: "-12".into(),
        grid: Some(GridSquare("FN31".into())),
        section: None,
        contest: None,
    }
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dm420-logbook-it-{}-{name}.jsonl",
        std::process::id()
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_replay_burst_keeps_writer_and_consumers_alive() {
    let path = scratch("startup-replay-overflow");
    let _ = std::fs::remove_file(&path);

    // A big on-disk log: REPLAY_N entries (seq 0..REPLAY_N), JSONL, one per line.
    let mut contents = String::new();
    for seq in 0..REPLAY_N {
        contents.push_str(&serde_json::to_string(&make_entry(seq, &format!("K{seq}AA"))).unwrap());
        contents.push('\n');
    }
    std::fs::write(&path, &contents).unwrap();

    let bus = BusHandle::new();

    // Non-draining consumer (simulates a frozen GUI pump): subscribe now, then do
    // NOT read until after the startup burst.
    let mut idle = bus
        .subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries))
        .unwrap();
    // Sync consumer: used only to detect when the replay has fully published, so the
    // fresh contact below is the newest message and can't be evicted before either
    // survivor drains to it.
    let mut sync = bus
        .subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries))
        .unwrap();

    logbook::spawn(&bus, path.clone());

    // Wait until the whole replay is on the wire. The last replayed entry
    // (seq REPLAY_N-1) is the newest message until we publish `fresh`, so it is
    // retained and `sync` is guaranteed to reach it (possibly after a Lagged).
    let last_seq = REPLAY_N - 1;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), sync.recv()).await {
            Ok(Ok(e)) if e.id.seq == last_seq => break,
            Ok(Ok(_)) => {}
            Ok(Err(BusError::Lagged { .. })) => {}
            Ok(Err(e)) => panic!("sync consumer errored during replay: {e:?}"),
            Err(_) => panic!("timed out waiting for the startup replay to publish"),
        }
    }

    // A freshly-worked contact, published exactly as the QSO engine would on RR73.
    let fresh = make_entry(9_999_999, FRESH_CALL);
    let fresh_id = fresh.id.clone();
    bus.publish(&Topic::LogbookEntries, fresh).unwrap();

    // (a) Writer survived the burst: the new contact is appended to disk. Under the
    //     old evict policy the logbook's loopback was deleted at ~1024 replays, the
    //     task broke, and this contact was never persisted (→ this would time out).
    let mut persisted = false;
    for _ in 0..100 {
        if std::fs::read_to_string(&path)
            .unwrap_or_default()
            .contains(FRESH_CALL)
        {
            persisted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        persisted,
        "the writer must survive the startup-replay burst and append the new contact"
    );

    // (b) The non-draining consumer merely lagged (was not evicted) and still
    //     receives the post-burst contact. Under the old policy it was disconnected
    //     mid-burst and its recv would return Closed here.
    let mut idle_got = false;
    for _ in 0..(2 * REPLAY_N as usize + 100) {
        match tokio::time::timeout(Duration::from_secs(5), idle.recv()).await {
            Ok(Ok(e)) if e.id == fresh_id => {
                idle_got = true;
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(BusError::Lagged { .. })) => {}
            Ok(Err(e)) => panic!("non-draining consumer was evicted (unexpected error): {e:?}"),
            Err(_) => panic!("timed out draining the non-draining consumer"),
        }
    }
    assert!(
        idle_got,
        "a non-draining lossless consumer must lag (not be evicted) and still receive \
         post-burst entries"
    );

    let _ = std::fs::remove_file(&path);
}
