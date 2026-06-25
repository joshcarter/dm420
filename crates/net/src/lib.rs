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
//! re-publishes peers' snapshots onto `station/{id}/snapshot`. The beacon's
//! `working` field is now populated from local bus state (the deconfliction
//! [`WorkingTarget`] — band + TX offset + the call being worked; see
//! [`beacon_loop`]); `heard`/`band_activity` are still empty, and the log-G-set
//! anti-entropy loop (`LogDigest`/`LogRequest`/`LogReply`) is step 2. Those `Wire`
//! variants are already declared so the wire format is stable; they're
//! logged-and-ignored until then.

#![forbid(unsafe_code)]

mod discovery;
mod peers;
pub mod wire;

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bus::{BusError, BusHandle, BusMessage, Subscription, Topic, TopicSelector};
use tokio::net::UdpSocket;
use types::{Band, QsoState, RadioId, RigState, StationId, StationSnapshot, WorkingTarget};

use crate::peers::Peers;
use crate::wire::{Frame, Wire, PROTO_VERSION};

/// Default radio id, matching `core::radio_id()` — there is exactly one radio today.
/// Kept as a local literal so `net` stays free of a `core` dependency; production
/// overrides it via `NetConfig.radio = core::radio_id()` when `core` wires the service.
const DEFAULT_RADIO_ID: &str = "rig0";

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
    /// IPv4 address to bind the UDP socket to. Production binds `UNSPECIFIED`
    /// (all interfaces); the loopback integration test binds `LOCALHOST` to keep
    /// traffic off the wire.
    pub bind: Ipv4Addr,
    /// UDP listen port; `0` binds an ephemeral port (advertised via mDNS).
    pub port: u16,
    /// Static peers for segments where mDNS is unavailable (`host:port`).
    pub manual_peers: Vec<SocketAddr>,
    /// Whether to run mDNS discovery (advertise + browse).
    pub enable_mdns: bool,
    /// Which radio's live state (`qso/{radio}/state`, `radio/{radio}/rig_state`) the
    /// beacon summarizes into its [`WorkingTarget`]. Supplied by the caller because
    /// `net` doesn't depend on `core`; production sets it to `core::radio_id()`.
    pub radio: RadioId,
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
            bind: Ipv4Addr::UNSPECIFIED,
            port,
            manual_peers,
            enable_mdns: true,
            radio: RadioId(DEFAULT_RADIO_ID.into()),
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
        match Service::bind(bus, cfg).await {
            Ok(svc) => {
                if let Err(e) = svc.run().await {
                    tracing::error!(error = %e, "net: gossip service stopped");
                }
            }
            Err(e) => tracing::error!(error = %e, "net: bind failed; gossip disabled"),
        }
    });
}

/// A bound-but-not-yet-running gossip service.
///
/// Splitting *bind* from *run* lets a caller learn the bound UDP port and wire
/// peers before any loop starts. Production goes straight through [`spawn`]; the
/// loopback integration test uses this to stand up two instances on ephemeral
/// `127.0.0.1` ports and cross-connect them deterministically, with mDNS off.
pub struct Service {
    bus: BusHandle,
    socket: Arc<UdpSocket>,
    peers: Peers,
    station: StationId,
    enable_mdns: bool,
    radio: RadioId,
}

impl Service {
    /// Bind the UDP socket (`cfg.bind:cfg.port`; port `0` = ephemeral) and seed
    /// the manual peers. Starts no task — call [`Service::run`] to drive the loops.
    pub async fn bind(bus: BusHandle, cfg: NetConfig) -> std::io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind((cfg.bind, cfg.port)).await?);
        let peers = Peers::default();
        for addr in &cfg.manual_peers {
            peers.add_target(*addr);
            tracing::info!(%addr, "net: manual peer");
        }
        Ok(Self {
            bus,
            socket,
            peers,
            station: cfg.station,
            enable_mdns: cfg.enable_mdns,
            radio: cfg.radio,
        })
    }

    /// The bound local UDP port — resolved even when `cfg.port` was `0`.
    pub fn local_port(&self) -> std::io::Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }

    /// Add a beacon target after binding, so a caller that has just learned a
    /// peer's ephemeral port (e.g. a test cross-wiring two instances) can register
    /// it before [`run`](Self::run). Idempotent.
    pub fn add_peer(&self, addr: SocketAddr) {
        self.peers.add_target(addr);
    }

    /// Drive the receive + beacon loops until the socket fails. Consumes `self`;
    /// spawn it onto the runtime if you need it in the background (what [`spawn`]
    /// does).
    pub async fn run(self) -> std::io::Result<()> {
        let port = self.socket.local_addr()?.port();
        tracing::info!(station = %self.station.0, port, "net: LAN gossip up");

        if self.enable_mdns {
            if let Err(e) = discovery::spawn(self.station.clone(), port, self.peers.clone()) {
                tracing::warn!(error = %e, "net: mDNS unavailable; manual peers only");
            }
        }

        tokio::spawn(recv_loop(
            self.socket.clone(),
            self.bus.clone(),
            self.peers.clone(),
            self.station.clone(),
        ));

        beacon_loop(self.socket, self.peers, self.station, self.bus, self.radio).await
    }
}

/// Beacon our [`StationSnapshot`] to every known peer on a fixed interval, and
/// age out peers we've stopped hearing. Step 3: the snapshot's `working` field
/// carries our live [`WorkingTarget`] (band + TX offset + the call being worked),
/// summarized from two **local-authority** State topics we only ever READ —
/// `qso/{radio}/state` and `radio/{radio}/rig_state`. We never republish peer data
/// onto those topics; the beacon is a one-way curated view of our own tuning.
///
/// The interval tick and the two State subscriptions are multiplexed with
/// `tokio::select!`: a QsoState/RigState message refreshes the cache; the tick
/// builds + sends the beacon from whatever is cached. A lagged sub is tolerated
/// (State watches don't lag, but it's harmless to keep reading); a closed sub is
/// retired (`None`) so a gone producer neither busy-spins nor stops the beacon.
async fn beacon_loop(
    socket: Arc<UdpSocket>,
    peers: Peers,
    station: StationId,
    bus: BusHandle,
    radio: RadioId,
) -> std::io::Result<()> {
    let mut seq = 0u64;
    let mut tick = tokio::time::interval(SNAPSHOT_INTERVAL);

    // Subscribe (State, Exact) to the two inputs. `.ok()` so a subscribe failure
    // degrades to "no working target" rather than killing the beacon.
    let mut qso_sub: Option<Subscription<QsoState>> = bus
        .subscribe::<QsoState>(TopicSelector::Exact(Topic::QsoState(radio.clone())))
        .ok();
    let mut rig_sub: Option<Subscription<RigState>> = bus
        .subscribe::<RigState>(TopicSelector::Exact(Topic::RigState(radio.clone())))
        .ok();

    let mut last_qso: Option<QsoState> = None;
    let mut last_rig: Option<RigState> = None;

    loop {
        tokio::select! {
            _ = tick.tick() => {
                seq += 1;
                let snap = StationSnapshot {
                    station: station.clone(),
                    seq,
                    working: assemble_working(&last_qso, &last_rig, &radio),
                    band_activity: vec![],
                    heard: vec![],
                };
                let frame = Frame {
                    version: PROTO_VERSION,
                    from: station.clone(),
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
            r = recv_cached(&mut qso_sub) => match r {
                Ok(v) => last_qso = Some(v),
                Err(BusError::Lagged { .. }) => {} // keep reading
                Err(_) => qso_sub = None,          // producer gone — stop polling, keep beaconing
            },
            r = recv_cached(&mut rig_sub) => match r {
                Ok(v) => last_rig = Some(v),
                Err(BusError::Lagged { .. }) => {}
                Err(_) => rig_sub = None,
            },
        }
    }
}

/// Await the next value from an optional State subscription. When the sub is
/// `None` (never created, or retired after a close) this never resolves, so the
/// `select!` arm stays inert instead of busy-spinning on a dead source.
async fn recv_cached<M: BusMessage>(sub: &mut Option<Subscription<M>>) -> Result<M, BusError> {
    match sub {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

/// Fold the latest cached `QsoState` (TX offset + partner) and `RigState` (dial
/// freq → band) into the beacon's [`WorkingTarget`]. Pure; no I/O. Returns `None`
/// until we have *both* a TX offset and a derivable band — we can't advertise a
/// tuned position without them.
///
/// INTERIM: the band is derived here from `RigState.vfo` via the canonical
/// [`Band::from_hz`]. Once the single-owner `OperatingState` producer (rework
/// slice 2c) lands, this should read `OperatingState.band` directly instead of
/// re-deriving the band from the rig's dial frequency.
fn assemble_working(
    last_qso: &Option<QsoState>,
    last_rig: &Option<RigState>,
    radio: &RadioId,
) -> Option<WorkingTarget> {
    // `tx_offset` is effectively always `Some` once the QSO engine is up, but treat
    // a missing offset as "nothing to advertise yet".
    let offset = last_qso.as_ref()?.tx_offset?;
    let band = Band::from_hz(last_rig.as_ref()?.vfo)?;
    // `partner` may be `None` (idle / armed-to-a-frequency): we still advertise the
    // tuned position so idle operators are visible, not just active QSOs.
    let call = last_qso.as_ref().and_then(|q| q.partner.clone());
    Some(WorkingTarget {
        radio: radio.clone(),
        band,
        offset,
        call,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use types::{AbsHz, Callsign, Meters, OffsetHz, QsoPhase, RigMode};

    fn qso(partner: Option<&str>, offset: Option<f32>) -> QsoState {
        QsoState {
            radio: RadioId(DEFAULT_RADIO_ID.into()),
            phase: QsoPhase::Calling,
            partner: partner.map(|c| Callsign(c.into())),
            next_tx: None,
            tx_offset: offset.map(OffsetHz),
            offset_locked: false,
        }
    }

    fn rig(vfo_hz: u64) -> RigState {
        RigState {
            radio: RadioId(DEFAULT_RADIO_ID.into()),
            vfo: AbsHz(vfo_hz),
            rig_mode: RigMode::UsbData,
            ptt: false,
            meters: Meters::default(),
        }
    }

    // (a) offset + a derivable band present → Some with the right offset/band/call.
    #[test]
    fn assemble_some_when_offset_and_band() {
        let radio = RadioId(DEFAULT_RADIO_ID.into());
        let w = assemble_working(
            &Some(qso(Some("N0JDC"), Some(1500.0))),
            &Some(rig(14_074_000)), // 20 m
            &radio,
        )
        .expect("offset + derivable band → Some");
        assert_eq!(w.radio, radio);
        assert_eq!(w.band, Band::B20m);
        assert_eq!(w.offset, OffsetHz(1500.0));
        assert_eq!(w.call, Some(Callsign("N0JDC".into())));
    }

    // (b) missing rig (no derivable band) → None.
    #[test]
    fn assemble_none_without_rig() {
        let radio = RadioId(DEFAULT_RADIO_ID.into());
        assert!(assemble_working(&Some(qso(Some("N0JDC"), Some(1500.0))), &None, &radio).is_none());
    }

    // (c) missing offset → None.
    #[test]
    fn assemble_none_without_offset() {
        let radio = RadioId(DEFAULT_RADIO_ID.into());
        assert!(
            assemble_working(&Some(qso(Some("N0JDC"), None)), &Some(rig(14_074_000)), &radio)
                .is_none()
        );
    }

    // (d) partner None (idle / armed to a frequency) → Some with call: None.
    #[test]
    fn assemble_some_with_no_partner() {
        let radio = RadioId(DEFAULT_RADIO_ID.into());
        let w = assemble_working(&Some(qso(None, Some(1500.0))), &Some(rig(14_074_000)), &radio)
            .expect("offset + band present even when idle → Some");
        assert_eq!(w.call, None);
        assert_eq!(w.band, Band::B20m);
        assert_eq!(w.offset, OffsetHz(1500.0));
    }
}
