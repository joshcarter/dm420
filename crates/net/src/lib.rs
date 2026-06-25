//! LAN gossip between operators — the `net` crate.
//!
//! Mirrors the `core::spawn` / `mocks::spawn` service pattern: owns its UDP socket
//! and mDNS daemon on background tasks and talks to the rest of the app **only over
//! the bus**. See `docs/networking.md` for the full protocol (transport, merge
//! semantics, anti-entropy loop) and `docs/message-catalog.md §9` for the payloads.
//!
//! ## What's built (step 1)
//!
//! Transport + discovery + the periodic [`StationSnapshot`] beacon: two instances
//! find each other (mDNS or `DM420_PEERS`), exchange snapshots over UDP, and each
//! re-publishes peers' snapshots onto `station/{id}/snapshot`. The snapshots carry
//! an empty payload for now — wiring `working`/`heard`/`band_activity` from the bus
//! is step 3, and the log-G-set anti-entropy loop (`LogDigest`/`LogRequest`/
//! `LogReply`) is step 2. Those `Wire` variants are already declared so the wire
//! format is stable; they're logged-and-ignored until then.

#![forbid(unsafe_code)]

mod discovery;
mod peers;
pub mod wire;

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bus::{BusHandle, Topic};
use tokio::net::UdpSocket;
use types::{StationId, StationSnapshot};

use crate::peers::Peers;
use crate::wire::{Frame, Wire, PROTO_VERSION};

/// How often we beacon our [`StationSnapshot`] to known peers.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);
/// A peer not heard within this window drops from the live set (several missed
/// beacons). Its beacon target is kept — a quiet peer is still worth pinging.
const PEER_TTL: Duration = Duration::from_secs(30);
/// Receive buffer — one UDP datagram's hard maximum.
const RECV_BUF: usize = 64 * 1024;

/// Configuration for the gossip service.
pub struct NetConfig {
    /// This operator's stable id, used as the gossip key and mDNS instance name.
    pub station: StationId,
    /// UDP listen port; `0` binds an ephemeral port (advertised via mDNS).
    pub port: u16,
    /// Static peers for segments where mDNS is unavailable (`host:port`).
    pub manual_peers: Vec<SocketAddr>,
    /// Whether to run mDNS discovery (advertise + browse).
    pub enable_mdns: bool,
}

impl NetConfig {
    /// Build from env, with the operator's identity supplied by the caller.
    /// `station` is single-sourced from `CoreConfig.station_id` (the configured
    /// `[station] station_id`) — there is no second, per-process identity here.
    /// The rest of the gossip config (listen port, manual peers, opt-out) is still
    /// read from env, interim like the rest of the app (see CLAUDE.md). Returns
    /// `None` when gossip is disabled (`DM420_NET=0`).
    pub fn from_env(station: StationId) -> Option<Self> {
        let enabled = std::env::var("DM420_NET")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if !enabled {
            return None;
        }
        let port = std::env::var("DM420_NET_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let manual_peers = std::env::var("DM420_PEERS")
            .ok()
            .map(|v| parse_peers(&v))
            .unwrap_or_default();
        Some(NetConfig {
            station,
            port,
            manual_peers,
            enable_mdns: true,
        })
    }
}

fn parse_peers(s: &str) -> Vec<SocketAddr> {
    s.split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            match p.to_socket_addrs() {
                Ok(mut it) => it.next(),
                Err(e) => {
                    tracing::warn!(peer = p, error = %e, "net: bad DM420_PEERS entry");
                    None
                }
            }
        })
        .collect()
}

/// Launch the gossip service onto `bus`. Detached tokio task; call from within a
/// runtime (like `core::spawn`). Failures degrade to a logged warning — the rest
/// of the app runs fine without the network.
pub fn spawn(bus: &BusHandle, cfg: NetConfig) {
    let bus = bus.clone();
    tokio::spawn(async move {
        if let Err(e) = run(bus, cfg).await {
            tracing::error!(error = %e, "net: gossip service stopped");
        }
    });
}

async fn run(bus: BusHandle, cfg: NetConfig) -> std::io::Result<()> {
    let socket = Arc::new(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, cfg.port)).await?);
    let port = socket.local_addr()?.port();
    tracing::info!(station = %cfg.station.0, port, "net: LAN gossip up");

    let peers = Peers::default();
    for addr in &cfg.manual_peers {
        peers.add_target(*addr);
        tracing::info!(%addr, "net: manual peer");
    }
    if cfg.enable_mdns {
        if let Err(e) = discovery::spawn(cfg.station.clone(), port, peers.clone()) {
            tracing::warn!(error = %e, "net: mDNS unavailable; manual peers only");
        }
    }

    tokio::spawn(recv_loop(
        socket.clone(),
        bus.clone(),
        peers.clone(),
        cfg.station.clone(),
    ));

    // Beacon loop. Step-1 snapshots carry an empty payload — we're proving
    // transport + discovery; the bus-fed content lands in steps 2–3.
    let mut seq = 0u64;
    let mut tick = tokio::time::interval(SNAPSHOT_INTERVAL);
    loop {
        tick.tick().await;
        seq += 1;
        let snap = StationSnapshot {
            station: cfg.station.clone(),
            seq,
            working: None,
            band_activity: vec![],
            heard: vec![],
        };
        let frame = Frame {
            version: PROTO_VERSION,
            from: cfg.station.clone(),
            msg: Wire::Snapshot(snap),
        };
        match wire::encode(&frame) {
            Ok(bytes) => {
                for addr in peers.targets() {
                    if let Err(e) = socket.send_to(&bytes, addr).await {
                        tracing::debug!(%addr, error = %e, "net: beacon send failed");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "net: snapshot encode failed"),
        }
        for dropped in peers.expire(PEER_TTL, Instant::now()) {
            tracing::info!(station = %dropped.0, "net: peer timed out");
        }
    }
}

async fn recv_loop(socket: Arc<UdpSocket>, bus: BusHandle, peers: Peers, me: StationId) {
    let mut buf = vec![0u8; RECV_BUF];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "net: recv error");
                continue;
            }
        };
        let frame = match wire::decode(&buf[..n]) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(%from, error = %e, "net: undecodable datagram");
                continue;
            }
        };
        if frame.from == me {
            continue; // our own beacon, echoed back via mDNS-resolved self/broadcast
        }
        if frame.version != PROTO_VERSION {
            tracing::debug!(%from, theirs = frame.version, ours = PROTO_VERSION, "net: version mismatch");
            continue;
        }
        match frame.msg {
            Wire::Snapshot(snap) => {
                let station = frame.from.clone();
                if peers.observe(&station, from, snap.seq, Instant::now()) {
                    tracing::info!(
                        station = %station.0,
                        seq = snap.seq,
                        peers = peers.live_count(),
                        "net: snapshot from peer",
                    );
                    // Re-publish onto the bus; panels read `station/*/snapshot`.
                    let _ = bus.publish(&Topic::StationSnapshot(station), snap);
                }
            }
            // Log-sync (LogPush/LogDigest/LogRequest/LogReply) is step 2.
            other => tracing::trace!(?other, "net: log-sync frame (step 2)"),
        }
    }
}
