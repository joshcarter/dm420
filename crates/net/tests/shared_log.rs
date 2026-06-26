//! Shared-logbook MVP proof: a `LogEntry` authored on instance A reaches instance
//! B's `logbook/entries` over the UDP gossip transport — no mDNS, no `DM420_PEERS`.
//!
//! Mirrors `loopback.rs`: two independent buses, two [`net::Service`]s on ephemeral
//! `127.0.0.1` ports, cross-wired by the ports we read back. We inject a contact
//! (`origin == A`) onto A's `logbook/entries`; A's outbound log loop `LogPush`es it
//! to B; B's `recv_loop` republishes it onto B's `logbook/entries`. We subscribe
//! there and assert the entry arrives under A's `QsoId` (origin preserved).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use bus::{BusHandle, Topic, TopicSelector};
use net::{NetConfig, Service};
use types::{AbsHz, Band, Callsign, LogEntry, OverAirMode, QsoId, RadioId, StationId, Timestamp};

/// A loopback-only config: bind `127.0.0.1:0` (ephemeral), no discovery, no peers
/// (the test cross-wires them once both ports are known).
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

fn sample_entry(origin: &str, seq: u64) -> LogEntry {
    LogEntry {
        id: QsoId {
            origin: StationId(origin.into()),
            seq,
        },
        radio: None,
        call: Callsign("W4LL".into()),
        mode: OverAirMode::Ft8,
        band: Band::B20m,
        freq: AbsHz(14_074_000),
        time: Timestamp(1_700_000_000_000),
        exchange_sent: "-10".into(),
        exchange_rcvd: "-12".into(),
        grid: None,
        section: None,
        contest: None,
    }
}

#[tokio::test]
async fn log_entry_authored_on_a_reaches_b() {
    let a_id = StationId("A".into());

    let bus_a = BusHandle::new();
    let bus_b = BusHandle::new();

    let svc_a = Service::bind(bus_a.clone(), loopback_cfg("A"))
        .await
        .unwrap();
    let svc_b = Service::bind(bus_b.clone(), loopback_cfg("B"))
        .await
        .unwrap();
    let port_a = svc_a.local_port().unwrap();
    let port_b = svc_b.local_port().unwrap();
    svc_a.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_b)));
    svc_b.add_peer(SocketAddr::from((Ipv4Addr::LOCALHOST, port_a)));

    // Subscribe on B *before* the entry can arrive — StreamLossless also replays the
    // retained ring, so the order isn't load-bearing, but this is the explicit read.
    let mut b_log = bus_b
        .subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries))
        .unwrap();

    tokio::spawn(svc_a.run());
    tokio::spawn(svc_b.run());

    // Inject one of A's own contacts. A's outbound loop folds it into the G-set and
    // pushes it to B (the only peer). The lossless ring covers the race where the
    // loop hasn't finished subscribing yet.
    let entry = sample_entry("A", 42);
    bus_a
        .publish(&Topic::LogbookEntries, entry.clone())
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(5), b_log.recv())
        .await
        .expect("B never received A's pushed LogEntry")
        .unwrap();

    assert_eq!(
        got.id, entry.id,
        "B sees A's contact under its original QsoId"
    );
    assert_eq!(
        got.id.origin, a_id,
        "author origin is preserved across the wire"
    );
    assert_eq!(
        got.call, entry.call,
        "the contact body survives the round-trip"
    );
}
