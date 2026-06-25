//! End-to-end proof of the deconfliction feature: station A's 5 s beacon carries a
//! populated [`WorkingTarget`] (band + TX offset + the call being worked), derived
//! from A's *local* bus state, and station B receives it on `station/A/snapshot`.
//!
//! Deterministic and offline — two ephemeral `127.0.0.1` UDP sockets, cross-wired by
//! port; no mDNS, no `DM420_PEERS`. A's `QsoState` + `RigState` are published BEFORE
//! A starts beaconing; State topics retain their latest value (watch `send_replace`),
//! so the beacon loop's subscriptions are primed with them.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use bus::{BusHandle, Topic, TopicSelector};
use net::{NetConfig, Service};
use types::{
    AbsHz, Band, Callsign, Meters, OffsetHz, QsoPhase, QsoState, RadioId, RigMode, RigState,
    StationId, StationSnapshot,
};

fn loopback_cfg(station: &str, radio: &str) -> NetConfig {
    NetConfig {
        station: StationId(station.into()),
        bind: Ipv4Addr::LOCALHOST,
        port: 0,
        manual_peers: vec![],
        enable_mdns: false,
        radio: RadioId(radio.into()),
    }
}

#[tokio::test]
async fn beacon_carries_working_target() {
    let a = StationId("A".into());
    let radio = RadioId("rig0".into());

    let bus_a = BusHandle::new();
    let bus_b = BusHandle::new();

    // A's live operating state, published BEFORE A beacons: a QSO with a TX offset +
    // partner, and a rig dial freq squarely on 20 m.
    bus_a
        .publish(
            &Topic::QsoState(radio.clone()),
            QsoState {
                radio: radio.clone(),
                phase: QsoPhase::Calling,
                partner: Some(Callsign("N0JDC".into())),
                next_tx: None,
                tx_offset: Some(OffsetHz(1234.0)),
                offset_locked: false,
            },
        )
        .unwrap();
    bus_a
        .publish(
            &Topic::RigState(radio.clone()),
            RigState {
                radio: radio.clone(),
                vfo: AbsHz(14_074_000), // 20 m
                rig_mode: RigMode::UsbData,
                ptt: false,
                meters: Meters::default(),
            },
        )
        .unwrap();

    // Bind both, read back ephemeral ports, cross-wire, then run.
    let svc_a = Service::bind(bus_a.clone(), loopback_cfg("A", "rig0"))
        .await
        .unwrap();
    let svc_b = Service::bind(bus_b.clone(), loopback_cfg("B", "rig0"))
        .await
        .unwrap();
    let port_a = svc_a.local_port().unwrap();
    let port_b = svc_b.local_port().unwrap();
    svc_a.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_b)));
    svc_b.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_a)));

    tokio::spawn(svc_a.run());
    tokio::spawn(svc_b.run());

    // B republishes every beacon it receives onto the exact `station/A/snapshot`.
    let mut exact = bus_b
        .subscribe::<StationSnapshot>(TopicSelector::Exact(Topic::StationSnapshot(a.clone())))
        .unwrap();

    // The first beacon (t=0, immediate interval tick) may race the cache fill and
    // carry `working: None`; the cache is populated within the first loop turns and
    // the next tick (one SNAPSHOT_INTERVAL later) carries it. Read beacons until one
    // arrives with `working` populated — each recv bounded so the test can't hang.
    let working = loop {
        let snap = tokio::time::timeout(Duration::from_secs(10), exact.recv())
            .await
            .expect("B never republished A's snapshot")
            .unwrap();
        assert_eq!(snap.station, a, "republished under A's StationId");
        if let Some(w) = snap.working {
            break w;
        }
    };

    assert_eq!(working.radio, radio, "working target addresses A's radio");
    assert_eq!(working.band, Band::B20m, "14.074 MHz → 20 m");
    assert_eq!(working.offset, OffsetHz(1234.0), "carries A's TX offset");
    assert_eq!(
        working.call,
        Some(Callsign("N0JDC".into())),
        "carries the call A is working"
    );
}
