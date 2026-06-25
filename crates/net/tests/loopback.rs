//! Deterministic two-instance loopback proof of the gossip bridge **and** the D1
//! wildcard-State late-join fix — no mDNS, no `DM420_PEERS`.
//!
//! Two independent buses ("A" and "B") each run a [`net::Service`] bound to an
//! ephemeral `127.0.0.1` port; we read each port back and cross-wire the peers
//! programmatically (the env var is global/racy under parallel tests). Station A
//! beacons a `StationSnapshot`; we assert B republishes it onto
//! `station/A/snapshot`, then — critically — that a wildcard subscriber created
//! *after* the republish still receives it (the D1 late-join prime).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use bus::{BusHandle, Topic, TopicKind, TopicSelector};
use net::{NetConfig, Service};
use types::{RadioId, StationId, StationSnapshot};

/// A loopback-only config: bind `127.0.0.1:0` (ephemeral), no discovery, no peers
/// (cross-wired by the test once both ports are known).
fn loopback_cfg(station: &str) -> NetConfig {
    NetConfig {
        station: StationId(station.into()),
        bind: Ipv4Addr::LOCALHOST,
        port: 0,
        manual_peers: vec![],
        enable_mdns: false,
        radio: RadioId("rig0".into()),
    }
}

#[tokio::test]
async fn loopback_bridge_and_wildcard_late_join() {
    let a = StationId("A".into());

    let bus_a = BusHandle::new();
    let bus_b = BusHandle::new();

    // Bind both services first (ephemeral loopback ports), then cross-wire by the
    // ports we read back — no env vars, no mDNS, no fixed ports to collide on.
    let svc_a = Service::bind(bus_a.clone(), loopback_cfg("A")).await.unwrap();
    let svc_b = Service::bind(bus_b.clone(), loopback_cfg("B")).await.unwrap();
    let port_a = svc_a.local_port().unwrap();
    let port_b = svc_b.local_port().unwrap();
    svc_a.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_b)));
    svc_b.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_a)));

    // Run both in the background. `tokio::time::interval`'s first tick fires
    // immediately, so A beacons right away — the test never waits a full
    // SNAPSHOT_INTERVAL.
    tokio::spawn(svc_a.run());
    tokio::spawn(svc_b.run());

    // 1) The bridge. B must republish A's snapshot onto the EXACT topic
    //    `station/A/snapshot`, carrying A's StationId and a real seq. A fresh
    //    exact-State subscription blocks until the value lands (bounded by the
    //    timeout, so the test can never hang).
    let mut exact = bus_b
        .subscribe::<StationSnapshot>(TopicSelector::Exact(Topic::StationSnapshot(a.clone())))
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), exact.recv())
        .await
        .expect("B never republished A's snapshot")
        .unwrap();
    assert_eq!(got.station, a, "republished under A's StationId");
    assert!(got.seq >= 1, "carries A's beacon seq, got {}", got.seq);

    // 2) D1. NOW that A's snapshot is already on B's bus, a LATE wildcard
    //    subscriber (`station/*/snapshot`) must still receive it via the late-join
    //    prime. Before the D1 fix this recv would hang (wildcard State was
    //    live-only) and the timeout would fire.
    let mut wild = bus_b
        .subscribe::<StationSnapshot>(TopicSelector::Wildcard(TopicKind::StationSnapshot))
        .unwrap();
    let late = tokio::time::timeout(Duration::from_secs(5), wild.recv())
        .await
        .expect("late wildcard subscriber received nothing (D1 regression)")
        .unwrap();
    assert_eq!(late.station, a, "wildcard primed with A's snapshot");
}
