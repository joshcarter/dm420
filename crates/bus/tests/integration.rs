//! Acceptance criteria #3–#9 from `docs/bus-handoff.md` (criterion #1 lives in the
//! `types` crate, #2 in `topic.rs`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use bus::types::*;
use bus::{BusError, BusHandle, BusMessage, DeliveryClass, Envelope, Topic, TopicKind, TopicSelector};

// --------------------------------------------------------------------- helpers

fn decode(id: &RadioId, n: u64) -> Decode {
    Decode {
        radio: id.clone(),
        mode: OverAirMode::Ft8,
        t: Timestamp(n as i64),
        offset: OffsetHz(1500.0),
        snr_db: Some(-10),
        source: SignalSource::Received,
        content: DecodeContent::Slotted {
            slot: SlotId(n),
            dt: 0.1,
            message: ParsedMessage::Free(format!("msg{n}")),
            raw: format!("msg{n}"),
        },
    }
}

fn slot_of(d: &Decode) -> u64 {
    match &d.content {
        DecodeContent::Slotted { slot, .. } => slot.0,
        _ => u64::MAX,
    }
}

fn spectrum(id: &RadioId, n: i64) -> SpectrumRow {
    SpectrumRow {
        radio: id.clone(),
        mode: OverAirMode::Ft8,
        t: Timestamp(n),
        bin0_offset: OffsetHz(0.0),
        bin_hz: 6.25,
        mags: vec![1, 2, 3],
        source: SignalSource::Received,
    }
}

fn rig_state(id: &RadioId) -> RigState {
    RigState {
        radio: id.clone(),
        vfo: AbsHz(14_074_000),
        rig_mode: RigMode::UsbData,
        ptt: false,
        meters: Meters::default(),
    }
}

fn temp_path(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dm420_bus_{}_{}_{}.ndjson",
        tag,
        std::process::id(),
        n
    ))
}

fn read_envelopes(p: &Path) -> Vec<Envelope> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Envelope>(l).unwrap())
        .collect()
}

/// A Command reply type. The orphan rule allows `impl BusMessage` here because
/// the type is local to this (test) crate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct TestReply {
    ok: bool,
}
impl BusMessage for TestReply {
    const CLASS: DeliveryClass = DeliveryClass::Command;
}

// --------------------------------------------------------------------- #3 State

#[tokio::test]
async fn state_late_join() {
    let bus = BusHandle::new();
    bus.publish(
        &Topic::ClockStatus,
        ClockStatus {
            offset_ms: 1.0,
            slot_phase: 0.5,
            slot: SlotId(0),
            mode: OverAirMode::Ft8,
        },
    )
    .unwrap();
    // Subscribe AFTER publishing: first recv must yield the current value.
    let mut sub = bus
        .subscribe::<ClockStatus>(TopicSelector::Exact(Topic::ClockStatus))
        .unwrap();
    assert_eq!(sub.recv().await.unwrap().offset_ms, 1.0);
}

// ---------------------------------------- #3b wildcard State late-join prime (D1)

fn snapshot(id: &str, seq: u64) -> StationSnapshot {
    StationSnapshot {
        station: StationId(id.into()),
        seq,
        working: None,
        band_activity: vec![],
        heard: vec![],
    }
}

/// D1: a wildcard `State` subscription is LIVE-ONLY no more — it must be primed
/// with the current value of every exact topic of that kind that already exists,
/// then continue to receive live updates.
#[tokio::test]
async fn wildcard_state_late_join_primes_then_live() {
    let bus = BusHandle::new();
    let a = StationId("a".into());
    let b = StationId("b".into());

    // Two exact State topics of the same kind exist (published) BEFORE any
    // wildcard subscriber.
    bus.publish(&Topic::StationSnapshot(a.clone()), snapshot("a", 1))
        .unwrap();
    bus.publish(&Topic::StationSnapshot(b.clone()), snapshot("b", 2))
        .unwrap();

    // Late-joining wildcard-State subscriber: its first recvs must yield BOTH
    // current values (order-independent — collect into a set).
    let mut sub = bus
        .subscribe::<StationSnapshot>(TopicSelector::Wildcard(TopicKind::StationSnapshot))
        .unwrap();
    let mut primed = std::collections::HashSet::new();
    for _ in 0..2 {
        let snap = tokio::time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .expect("primed recv timed out")
            .unwrap();
        primed.insert((snap.station.0, snap.seq));
    }
    assert_eq!(
        primed,
        [("a".to_string(), 1), ("b".to_string(), 2)]
            .into_iter()
            .collect()
    );

    // A subsequent live publish — to a fresh exact topic of the kind — still
    // arrives over the broadcast.
    let c = StationId("c".into());
    bus.publish(&Topic::StationSnapshot(c.clone()), snapshot("c", 3))
        .unwrap();
    let live = tokio::time::timeout(Duration::from_secs(1), sub.recv())
        .await
        .expect("live recv timed out")
        .unwrap();
    assert_eq!((live.station.0, live.seq), ("c".to_string(), 3));
}

// ----------------------------------------------------- #4 Lossless order + late-join

#[tokio::test]
async fn lossless_order_and_late_join() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::Decodes(id.clone());

    for i in 0..5 {
        bus.publish(&topic, decode(&id, i)).unwrap();
    }
    let mut sub = bus
        .subscribe::<Decode>(TopicSelector::Exact(topic.clone()))
        .unwrap();
    // Retained ring replays in order...
    for i in 0..5 {
        assert_eq!(slot_of(&sub.recv().await.unwrap()), i);
    }
    // ...then live messages continue with no gaps.
    for i in 5..8 {
        bus.publish(&topic, decode(&id, i)).unwrap();
    }
    for i in 5..8 {
        assert_eq!(slot_of(&sub.recv().await.unwrap()), i);
    }
}

// ----------------------------------------------------------------- #5 Lossy load

#[tokio::test]
async fn lossy_lagged_and_isolation() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::Spectrum(id.clone());

    let mut slow = bus
        .subscribe::<SpectrumRow>(TopicSelector::Exact(topic.clone()))
        .unwrap();
    // Flood far beyond the broadcast capacity without draining `slow`. The
    // publisher must never block: every publish returns immediately.
    for i in 0..1000 {
        bus.publish(&topic, spectrum(&id, i)).unwrap();
    }
    // The slow subscriber is told it lagged.
    let mut saw_lag = false;
    for _ in 0..1000 {
        match slow.recv().await {
            Ok(_) => {}
            Err(BusError::Lagged { .. }) => {
                saw_lag = true;
                break;
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert!(saw_lag, "expected a Lagged error under flood");

    // A healthy subscriber on the same topic is unaffected.
    let mut healthy = bus
        .subscribe::<SpectrumRow>(TopicSelector::Exact(topic.clone()))
        .unwrap();
    bus.publish(&topic, spectrum(&id, 12345)).unwrap();
    assert_eq!(healthy.recv().await.unwrap().t.0, 12345);
}

// --------------------------------------------- #6 Lossless overflow → lag, not evict

/// The lossless overflow contract is **lag-not-evict**: a subscriber that stops
/// draining and overflows the live tail is signalled `Lagged` and STAYS subscribed,
/// never disconnected. This asserts the exact opposite of the old policy (which
/// returned `Closed` and deleted the subscriber here), so it is the regression guard
/// for the reversed contract. See `docs/bus-handoff.md` (acceptance #6).
#[tokio::test]
async fn lossless_overflow_lags_not_disconnects() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::Decodes(id.clone());

    let mut slow = bus
        .subscribe::<Decode>(TopicSelector::Exact(topic.clone()))
        .unwrap();

    // The Decodes live-tail lag budget is lossless_live_cap = ring_capacity(16).max(1024) = 1024.
    const LIVE_CAP: u64 = 1024;
    let n = LIVE_CAP + 500;

    // (1) The publisher never blocks or fails, even flooded far past the live tail
    //     with nobody draining: every publish returns Ok. (Under the OLD policy the
    //     subscriber was deleted mid-flood, so the publisher returning Ok didn't
    //     catch it — the drain in (2) is what distinguishes the contracts.)
    for i in 0..n {
        assert!(
            bus.publish(&topic, decode(&id, i)).is_ok(),
            "publisher must never block or fail on lossless overflow"
        );
    }

    // (2) Draining now yields a Lagged signal, and NEVER Closed. This is the
    //     reversed contract: v1 disconnected the overflowing subscriber (→ Closed).
    //     (tokio surfaces the lag on the first recv, ahead of the retained tail, so
    //     we don't require an Ok strictly *before* the Lagged — see the Ok proof below.)
    let mut saw_lag = false;
    for _ in 0..n {
        match slow.recv().await {
            Ok(_) => {}
            Err(BusError::Lagged { skipped }) => {
                assert!(skipped > 0, "Lagged must report a positive skip count");
                saw_lag = true;
                break;
            }
            Err(BusError::Closed) => {
                panic!("lossless overflow must not disconnect the subscriber (got Closed)")
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert!(saw_lag, "overflow must surface as Lagged, not eviction");

    // (3) The clincher: the SAME handle — never re-subscribed — still receives a
    //     brand-new live message. Proof it was lagged, not evicted. Draining past the
    //     retained backlog also proves real messages (Ok) still flow, not only lag.
    bus.publish(&topic, decode(&id, 999_999)).unwrap();
    let mut got_live = false;
    let mut saw_ok = false;
    for _ in 0..(2 * n) {
        match slow.recv().await {
            Ok(d) => {
                saw_ok = true;
                if slot_of(&d) == 999_999 {
                    got_live = true;
                    break;
                }
            }
            Err(BusError::Lagged { .. }) => {}
            Err(e) => panic!("unexpected error after lag: {e:?}"),
        }
    }
    assert!(
        got_live,
        "a lagged lossless subscriber stays subscribed and receives new live messages"
    );
    assert!(
        saw_ok,
        "lossless delivery still yields real messages (Ok), not only lag signals"
    );
}

// ------------------------------------------------------------- #7 Request/reply

#[tokio::test]
async fn request_reply_paths() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::RigCommand(id.clone());

    // NoHandler before any server is registered.
    let r = bus
        .request::<RigCommand, TestReply>(
            &topic,
            RigCommand::SetFreq(AbsHz(14_074_000)),
            Duration::from_millis(100),
        )
        .await;
    assert_eq!(r, Err(BusError::NoHandler));

    // Register a server; a second serve on the same topic is rejected.
    let mut server = bus.serve::<RigCommand, TestReply>(&topic).unwrap();
    assert!(matches!(
        bus.serve::<RigCommand, TestReply>(&topic),
        Err(BusError::ServerExists)
    ));

    tokio::spawn(async move {
        while let Some((cmd, responder)) = server.next().await {
            responder.reply(TestReply {
                ok: matches!(cmd, RigCommand::SetFreq(_)),
            });
        }
    });

    let rep = bus
        .request::<RigCommand, TestReply>(
            &topic,
            RigCommand::SetFreq(AbsHz(7_074_000)),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert!(rep.ok);

    // Timeout: a served topic whose server never reads → no reply in time.
    let topic2 = Topic::SessionCommand(id.clone());
    let _held = bus.serve::<SessionCommand, TestReply>(&topic2).unwrap();
    let r = bus
        .request::<SessionCommand, TestReply>(
            &topic2,
            SessionCommand::SetMode(OverAirMode::Ft4),
            Duration::from_millis(80),
        )
        .await;
    assert_eq!(r, Err(BusError::Timeout));
}

// -------------------------------------------------------- #8 Record → replay

#[tokio::test]
async fn record_replay_golden() {
    let id = RadioId("k1".into());

    // Record a scripted session.
    let path1 = temp_path("rec1");
    let bus1 = BusHandle::new();
    let rec1 = bus1.attach_recorder(&path1).unwrap();
    bus1.publish(
        &Topic::ClockStatus,
        ClockStatus {
            offset_ms: 1.0,
            slot_phase: 0.1,
            slot: SlotId(0),
            mode: OverAirMode::Ft8,
        },
    )
    .unwrap();
    bus1.publish(&Topic::Decodes(id.clone()), decode(&id, 1))
        .unwrap();
    bus1.publish(&Topic::Decodes(id.clone()), decode(&id, 2))
        .unwrap();
    bus1.publish(&Topic::RigState(id.clone()), rig_state(&id))
        .unwrap();
    rec1.stop().await;

    let env1 = read_envelopes(&path1);
    assert_eq!(env1.len(), 4);

    // Replay at very high speed onto a fresh bus, recording the re-published stream.
    let path2 = temp_path("rec2");
    let bus2 = BusHandle::new();
    let rec2 = bus2.attach_recorder(&path2).unwrap();
    bus::replay(&bus2, &path1, 1e9).await.unwrap();
    rec2.stop().await;

    let env2 = read_envelopes(&path2);

    // The (topic, payload) sequence must match (recorded_at timestamps differ).
    let seq = |e: &[Envelope]| {
        e.iter()
            .map(|x| (x.topic.clone(), x.payload.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(seq(&env1), seq(&env2));

    let _ = std::fs::remove_file(&path1);
    let _ = std::fs::remove_file(&path2);
}

// ------------------------------------------------------------- #9 Concurrency

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_smoke() {
    let bus = BusHandle::new();
    let id = RadioId("k1".into());
    let topic = Topic::Decodes(id.clone());

    let mut sub = bus
        .subscribe::<Decode>(TopicSelector::Exact(topic.clone()))
        .unwrap();

    let n_pub: u64 = 4;
    let per: u64 = 50;
    let mut tasks = Vec::new();
    for t in 0..n_pub {
        let b = bus.clone();
        let tp = topic.clone();
        let rid = id.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..per {
                b.publish(&tp, decode(&rid, t * per + i)).unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    // Lossless: every message is delivered (total < the live-tail lag budget, so
    // nothing is dropped) with no deadlock.
    let total = n_pub * per;
    let mut count = 0u64;
    while count < total {
        match tokio::time::timeout(Duration::from_millis(500), sub.recv()).await {
            Ok(Ok(_)) => count += 1,
            _ => break,
        }
    }
    assert_eq!(count, total, "received {count} of {total}");
}
