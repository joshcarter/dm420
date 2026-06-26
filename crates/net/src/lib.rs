//! LAN gossip between operators — the `net` crate.
//!
//! Mirrors the `core::spawn` / `mocks::spawn` service pattern: owns its UDP socket
//! and mDNS daemon on background tasks and talks to the rest of the app **only over
//! the bus**. See `docs/networking.md` for the full protocol (transport, merge
//! semantics, anti-entropy loop) and `docs/message-catalog.md §9` for the payloads.
//!
//! ## What's built (steps 1 & 2-MVP)
//!
//! Transport + discovery + the periodic [`StationSnapshot`] beacon: two instances
//! find each other (mDNS or `DM420_PEERS`), exchange snapshots over UDP, and each
//! re-publishes peers' snapshots onto `station/{id}/snapshot`. The beacon's
//! `working` field is populated from local bus state (the deconfliction
//! [`WorkingTarget`] — band + TX offset + the call being worked; see
//! [`beacon_loop`]); `heard`/`band_activity` are still empty.
//!
//! **Shared logbook (step 2 MVP).** The log G-set ([`gset::Gset`]) now syncs:
//! - **OUT:** we subscribe `logbook/entries`, hold every entry in the G-set, and
//!   proactively [`Wire::LogPush`] entries we *authored* (`origin == me`) to peers
//!   — peer entries we injected are held but never re-pushed (echo guard).
//! - **IN:** `LogPush`/`LogReply` entries not already held are re-published onto
//!   `logbook/entries`, where the logbook service persists them (curated boundary:
//!   peer data only ever lands on `logbook/entries`, never a local-authority topic).
//! - **Catch-up:** the first snapshot from a new peer triggers a one-shot
//!   MTU-chunked bulk `LogPush` of our own backlog, and a periodic ~15 s
//!   [`repush_loop`] re-pushes our authored entries to every peer as a
//!   convergence backstop — it closes the new-peer catch-up race (an empty
//!   catch-up when nothing is logged yet) and is independent of join order.
//!
//! The full anti-entropy loop (`LogDigest`/`LogRequest` range diffing) is step 2b;
//! the periodic re-push is a brute-force stand-in until it lands. Those `Wire`
//! variants stay declared-but-inert.

#![forbid(unsafe_code)]

mod discovery;
mod gset;
mod peers;
pub mod wire;

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bus::{BusError, BusHandle, BusMessage, Subscription, Topic, TopicSelector};
use tokio::net::UdpSocket;
use types::{
    Band, LogEntry, QsoState, RadioId, RigState, StationId, StationSnapshot, WorkingTarget,
};

use crate::gset::Gset;
use crate::peers::Peers;
use crate::wire::{Frame, PROTO_VERSION, Wire};

/// MTU-safe byte budget for a single gossip datagram. Held well under the ~1500 B
/// Ethernet MTU so a `LogPush`/`LogReply` never IP-fragments — a fragmented
/// datagram is lost *whole* if any one fragment drops, which would defeat the
/// G-set's reliability. Bulk catch-up is split across as many datagrams as needed
/// to stay under this.
const PUSH_MTU_BUDGET: usize = 1200;

/// Default radio id, matching `core::radio_id()` — there is exactly one radio today.
/// Kept as a local literal so `net` stays free of a `core` dependency; production
/// overrides it via `NetConfig.radio = core::radio_id()` when `core` wires the service.
const DEFAULT_RADIO_ID: &str = "rig0";

/// How often we beacon our [`StationSnapshot`] to known peers.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);
/// A peer not heard within this window drops from the live set (several missed
/// beacons). Its beacon target is kept — a quiet peer is still worth pinging.
const PEER_TTL: Duration = Duration::from_secs(30);
/// How often we re-push our own log G-set to every peer ([`repush_loop`]). A
/// brute-force convergence backstop standing in for the deferred anti-entropy
/// pull (step 2b): bounded traffic at Field-Day log sizes, idempotent on the
/// receiver (dedup by `QsoId`), and immune to join order / the new-peer
/// catch-up race.
const REPUSH_INTERVAL: Duration = Duration::from_secs(15);
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

/// Whether a beacon target is reachable from our IPv4-only gossip socket.
///
/// `NetConfig.bind` is `Ipv4Addr::UNSPECIFIED`, so [`Service::bind`] creates an IPv4
/// `UdpSocket`. Sending to a non-IPv4 address — typically an IPv6 link-local
/// `fe80::…` that mDNS resolves alongside a host's IPv4 address — fails with
/// `Invalid argument` (EINVAL) and, on a 5 s beacon, floods the log. Both the mDNS
/// discovery filter (`discovery.rs`) and the beacon send loop gate on this single
/// predicate so an unusable address never reaches `send_to`.
pub(crate) fn is_sendable(addr: &SocketAddr) -> bool {
    addr.is_ipv4()
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

        // The log G-set is shared by the inbound (`recv_loop`) and outbound
        // (`outbound_log_loop`) halves of the shared-logbook sync: both fold
        // entries in, and the merge/echo guards key off "already held".
        let gset = Gset::default();

        tokio::spawn(recv_loop(
            self.socket.clone(),
            self.bus.clone(),
            self.peers.clone(),
            self.station.clone(),
            gset.clone(),
        ));
        tokio::spawn(outbound_log_loop(
            self.socket.clone(),
            self.bus.clone(),
            self.peers.clone(),
            self.station.clone(),
            gset.clone(),
        ));
        // Convergence backstop: periodically re-push our own backlog to every
        // peer, independent of join order and the one-shot new-peer catch-up.
        tokio::spawn(repush_loop(
            self.socket.clone(),
            self.peers.clone(),
            self.station.clone(),
            gset,
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
    // Seed the beacon `seq` from the wall clock, mirroring the log seq idiom in
    // `qso::shell`. A per-process `0`-counter looked *stale* to peers for up to
    // `PEER_TTL` after a restart: the restarted op's `seq 1,2,…` were ≤ the high-
    // water mark peers still held from the prior session, so `Peers::observe`
    // dropped them as duplicates. Seeding from `now_ms()` makes every new session's
    // beacons strictly exceed any prior session's. The `max(now, last + 1)` floor
    // keeps `seq` strictly increasing across same-millisecond beacons and immune to
    // a backward clock step mid-session. Only ever compared within one peer's own
    // stream, so the absolute value is irrelevant — only monotonicity matters.
    let mut last_seq = 0u64;
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
                let seq = (types::now_ms() as u64).max(last_seq + 1);
                last_seq = seq;
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
                            // Belt-and-suspenders: our gossip socket is IPv4-only, so
                            // never hand a non-IPv4 target to `send_to` — it would fail
                            // with EINVAL on every tick and flood the log. A non-IPv4
                            // target shouldn't reach here (discovery filters them), but a
                            // manual `DM420_PEERS` entry or future code could; drop it
                            // silently rather than spam per-iteration.
                            if !is_sendable(&addr) {
                                continue;
                            }
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
    let dial = last_rig.as_ref()?.vfo;
    let band = Band::from_hz(dial)?;
    // `partner` may be `None` (idle / armed-to-a-frequency): we still advertise the
    // tuned position so idle operators are visible, not just active QSOs.
    let call = last_qso.as_ref().and_then(|q| q.partner.clone());
    Some(WorkingTarget {
        radio: radio.clone(),
        band,
        offset,
        dial,
        call,
    })
}

async fn recv_loop(
    socket: Arc<UdpSocket>,
    bus: BusHandle,
    peers: Peers,
    me: StationId,
    gset: Gset,
) {
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
                let obs = peers.observe(&station, from, snap.seq, Instant::now());
                if obs.fresh {
                    tracing::info!(
                        station = %station.0,
                        seq = snap.seq,
                        peers = peers.live_count(),
                        "net: snapshot from peer",
                    );
                    // Re-publish onto the bus; panels read `station/*/snapshot`.
                    let _ = bus.publish(&Topic::StationSnapshot(station), snap);
                }
                if obs.new_station {
                    // First contact with this peer: one-shot bulk catch-up of OUR
                    // OWN log (mine-only, matching the push guard — in a full mesh
                    // every pair exchanges its own, so all logs converge). MTU-
                    // chunked so no datagram IP-fragments. Cheap stand-in for the
                    // full anti-entropy pull (step 2b).
                    let mine = gset.mine(&me);
                    // Logged unconditionally — an empty catch-up (the race where
                    // this fires before our backlog is in the G-set, or before any
                    // contact is logged) is now visible, not silent. The periodic
                    // `repush_loop` is what eventually closes that gap.
                    tracing::info!(
                        peer = %from,
                        entries = mine.len(),
                        "net: catch-up OUT to new peer",
                    );
                    if !mine.is_empty() {
                        let datagrams = pack_log_push(&mine, &me);
                        for dgram in datagrams {
                            if let Err(e) = socket.send_to(&dgram, from).await {
                                tracing::debug!(%from, error = %e, "net: catch-up send failed");
                            }
                        }
                    }
                }
            }
            // Inbound log merge. `LogReply` (a pull answer) is treated identically
            // to `LogPush` (a proactive push): both carry entries to merge.
            //
            // CURATED BOUNDARY (security-critical): a peer's `LogEntry` only ever
            // re-enters this instance on `logbook/entries` — never on any local-
            // authority topic (`rig/command`, `radio/*/audio_tx`, `qso/*/state`).
            // The logbook service is subscribed there and persists each new entry;
            // the GUI / worked-status update for free.
            Wire::LogPush(entries) | Wire::LogReply(entries) => {
                let received = entries.len();
                let mut new = 0usize;
                for entry in entries {
                    if gset.insert(entry.clone()) {
                        new += 1;
                        let _ = bus.publish(&Topic::LogbookEntries, entry);
                    }
                    // Already held → idempotent drop (no re-publish, no echo).
                }
                tracing::info!(from = %from, received, new, "net: log-sync IN");
            }
            // Anti-entropy (`LogDigest` digest exchange / `LogRequest` range serve)
            // is step 2b — deliberately inert in the MVP. We never advertise a
            // digest, so we never solicit a request; an unsolicited `LogRequest` is
            // a no-op rather than a serve.
            other @ (Wire::LogDigest(_) | Wire::LogRequest(_)) => {
                tracing::trace!(?other, "net: anti-entropy frame (step 2b — not built)");
            }
        }
    }
}

/// The outbound half of the shared logbook: fold every entry on `logbook/entries`
/// into the G-set, and proactively [`Wire::LogPush`] the ones we *authored* to
/// every known peer.
///
/// **Push guard (echo-storm prevention):** only `origin == me` entries are pushed.
/// Entries with `origin != me` are peer contacts *we* injected back onto
/// `logbook/entries` from [`recv_loop`]; they still populate the G-set (so we can
/// relay them later and our catch-up dedups correctly), but re-pushing them would
/// bounce them around an N-peer mesh forever. The guard is on **push only**.
///
/// At startup the logbook replays its full backlog onto this (StreamLossless)
/// topic; those replays populate the G-set the same way. Because mDNS hasn't found
/// peers yet at that instant, the resulting pushes target an empty peer set (a
/// no-op) — history reaches peers via the new-peer catch-up in [`recv_loop`].
async fn outbound_log_loop(
    socket: Arc<UdpSocket>,
    bus: BusHandle,
    peers: Peers,
    me: StationId,
    gset: Gset,
) {
    let mut sub = match bus.subscribe::<LogEntry>(TopicSelector::Exact(Topic::LogbookEntries)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "net: cannot subscribe logbook/entries; outbound log sync disabled");
            return;
        }
    };
    loop {
        match sub.recv().await {
            Ok(entry) => {
                let mine = entry.id.origin == me;
                // Insert *before* the guard so peer entries still join the G-set.
                let is_new = gset.insert(entry.clone());
                if mine && is_new {
                    // Snapshot the sendable targets once: it's both the send list
                    // and the `peers` count we log.
                    let targets: Vec<SocketAddr> =
                        peers.targets().into_iter().filter(is_sendable).collect();
                    tracing::info!(
                        origin = %entry.id.origin.0,
                        seq = entry.id.seq,
                        peers = targets.len(),
                        "net: log-push OUT (live)",
                    );
                    let frame = Frame {
                        version: PROTO_VERSION,
                        from: me.clone(),
                        msg: Wire::LogPush(vec![entry]),
                    };
                    match wire::encode(&frame) {
                        Ok(bytes) => {
                            for addr in targets {
                                if let Err(e) = socket.send_to(&bytes, addr).await {
                                    tracing::debug!(%addr, error = %e, "net: log-push send failed");
                                }
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "net: log-push encode failed"),
                    }
                }
            }
            // StreamLossless: a lag is impossible in practice, but stay exhaustive.
            Err(BusError::Lagged { .. }) => continue,
            Err(_) => break, // producer/bus gone — end the task.
        }
    }
}

/// Pack `entries` into as few `Wire::LogPush` datagrams as fit under
/// [`PUSH_MTU_BUDGET`], each framed with `from` as the sender. Greedy: grow a
/// batch until adding the next entry would exceed budget, then flush. A single
/// entry that alone exceeds budget still ships on its own (better fragmented than
/// dropped). Used only for the one-shot new-peer catch-up, so the re-encode cost
/// of size-probing is irrelevant.
fn pack_log_push(entries: &[LogEntry], from: &StationId) -> Vec<Vec<u8>> {
    let encode_batch = |batch: &[LogEntry]| -> Option<Vec<u8>> {
        if batch.is_empty() {
            return None;
        }
        let frame = Frame {
            version: PROTO_VERSION,
            from: from.clone(),
            msg: Wire::LogPush(batch.to_vec()),
        };
        match wire::encode(&frame) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "net: catch-up encode failed");
                None
            }
        }
    };

    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut batch: Vec<LogEntry> = Vec::new();
    for entry in entries {
        batch.push(entry.clone());
        // Only probe once the batch holds >1 entry — a lone entry always ships,
        // even if oversized.
        if batch.len() > 1
            && let Some(bytes) = encode_batch(&batch)
            && bytes.len() > PUSH_MTU_BUDGET
        {
            // This entry tipped us over: flush the prior batch, start a new one
            // carrying just this entry.
            batch.pop();
            if let Some(bytes) = encode_batch(&batch) {
                out.push(bytes);
            }
            batch = vec![entry.clone()];
        }
    }
    if let Some(bytes) = encode_batch(&batch) {
        out.push(bytes);
    }
    out
}

/// Periodically re-push our own log G-set to every peer — a brute-force
/// convergence backstop standing in for the deferred anti-entropy pull (step 2b).
///
/// **Why it's needed.** The only other backlog mechanism is the one-shot new-peer
/// catch-up in [`recv_loop`], which silently delivers nothing if it fires before
/// our backlog is in the G-set or before any contact is logged. This loop re-pushes
/// regardless of join order, so a peer eventually receives our log no matter how the
/// catch-up race resolves. Bounded at Field-Day log sizes, and the receiver dedups by
/// `QsoId`, so each round is idempotent.
///
/// **Mine-only**, the same guard as the live push and catch-up: we re-push only
/// entries we *authored* (`origin == me`), never peer-authored ones — that's the
/// echo-storm guard. The inbound merge/inject path and the curated boundary are
/// untouched; peer entries still only ever republish onto `Topic::LogbookEntries`.
async fn repush_loop(socket: Arc<UdpSocket>, peers: Peers, me: StationId, gset: Gset) {
    let mut tick = tokio::time::interval(REPUSH_INTERVAL);
    // `interval` fires the first tick immediately; skip it so the first re-push
    // waits a full interval (t=0 is already covered by the live push + catch-up).
    tick.tick().await;
    loop {
        tick.tick().await;
        repush_once(&socket, &peers, &me, &gset).await;
    }
}

/// One re-push pass: send our authored backlog to every sendable peer. Factored out
/// of [`repush_loop`] so it's directly testable without waiting on the timer. A
/// no-op (no datagram, no log) when we've authored nothing yet or no peer is
/// reachable. Returns the number of peers it sent to.
async fn repush_once(socket: &UdpSocket, peers: &Peers, me: &StationId, gset: &Gset) -> usize {
    let mine = gset.mine(me);
    if mine.is_empty() {
        return 0;
    }
    let targets: Vec<SocketAddr> = peers.targets().into_iter().filter(is_sendable).collect();
    if targets.is_empty() {
        return 0;
    }
    let datagrams = pack_log_push(&mine, me);
    tracing::info!(
        entries = mine.len(),
        peers = targets.len(),
        "net: periodic log re-push",
    );
    for addr in &targets {
        for dgram in &datagrams {
            if let Err(e) = socket.send_to(dgram, addr).await {
                tracing::debug!(%addr, error = %e, "net: re-push send failed");
            }
        }
    }
    targets.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv6Addr};
    use types::{
        AbsHz, Callsign, Meters, OffsetHz, OverAirMode, QsoId, QsoPhase, RigMode, Timestamp,
    };

    // The IPv4 / IPv6 filter that keeps IPv6 link-local mDNS addresses off our
    // IPv4-only socket: IPv4 is sendable, anything else (here a `fe80::…`) is not.
    #[test]
    fn is_sendable_accepts_ipv4_rejects_ipv6() {
        let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), 4040);
        let v6_link_local =
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 4040);
        assert!(
            is_sendable(&v4),
            "IPv4 target is sendable from the IPv4 socket"
        );
        assert!(
            !is_sendable(&v6_link_local),
            "IPv6 link-local target must be filtered out"
        );
    }

    // Reproduce the discovery resolver's per-address loop: given a host resolved to
    // both an IPv4 and an IPv6 link-local address, only the IPv4 one is registered as
    // a beacon target (the IPv6 one would have failed `send_to` with EINVAL).
    #[test]
    fn only_ipv4_resolved_addresses_become_targets() {
        let peers = crate::peers::Peers::default();
        let port = 4040;
        let resolved = [
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
        ];
        for ip in resolved {
            let addr = SocketAddr::new(ip, port);
            if is_sendable(&addr) {
                peers.add_target(addr);
            }
        }
        assert_eq!(
            peers.targets(),
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)),
                port
            )],
            "only the IPv4 address becomes a beacon target",
        );
    }

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
            assemble_working(
                &Some(qso(Some("N0JDC"), None)),
                &Some(rig(14_074_000)),
                &radio
            )
            .is_none()
        );
    }

    // (d) partner None (idle / armed to a frequency) → Some with call: None.
    #[test]
    fn assemble_some_with_no_partner() {
        let radio = RadioId(DEFAULT_RADIO_ID.into());
        let w = assemble_working(
            &Some(qso(None, Some(1500.0))),
            &Some(rig(14_074_000)),
            &radio,
        )
        .expect("offset + band present even when idle → Some");
        assert_eq!(w.call, None);
        assert_eq!(w.band, Band::B20m);
        assert_eq!(w.offset, OffsetHz(1500.0));
    }

    fn log_entry(seq: u64) -> LogEntry {
        LogEntry {
            id: QsoId {
                origin: StationId("me".into()),
                seq,
            },
            radio: None,
            call: Callsign(format!("K{seq}XYZ")),
            mode: OverAirMode::Ft8,
            band: Band::B20m,
            freq: AbsHz(14_074_000),
            time: Timestamp(1_700_000_000_000 + seq as i64),
            exchange_sent: "-10".into(),
            exchange_rcvd: "-12".into(),
            grid: None,
            section: None,
        }
    }

    // Same shape as `log_entry` but authored by a peer (a different `origin`) —
    // the kind of entry the re-push guard must never put back on the wire.
    fn peer_entry(origin: &str, seq: u64) -> LogEntry {
        let mut e = log_entry(seq);
        e.id.origin = StationId(origin.into());
        e
    }

    // Decode a produced datagram back to the entries it carries.
    fn unpack(bytes: &[u8]) -> Vec<LogEntry> {
        match wire::decode(bytes).unwrap().msg {
            Wire::LogPush(es) => es,
            other => panic!("expected LogPush, got {other:?}"),
        }
    }

    // The bulk catch-up packs *every* entry across MTU-bounded datagrams, with no
    // datagram exceeding the budget (a multi-entry one would IP-fragment otherwise)
    // and no entry lost or duplicated.
    #[test]
    fn pack_log_push_chunks_under_mtu_and_preserves_all() {
        let from = StationId("me".into());
        let entries: Vec<LogEntry> = (0..50).map(log_entry).collect();
        let datagrams = pack_log_push(&entries, &from);

        assert!(
            datagrams.len() > 1,
            "50 entries must span several datagrams, got {}",
            datagrams.len()
        );

        let mut seqs = Vec::new();
        for dgram in &datagrams {
            // A datagram carrying >1 entry must fit the budget; a lone oversized
            // entry is allowed to exceed it (better fragmented than dropped).
            let carried = unpack(dgram);
            if carried.len() > 1 {
                assert!(
                    dgram.len() <= PUSH_MTU_BUDGET,
                    "multi-entry datagram {} B exceeds budget {PUSH_MTU_BUDGET}",
                    dgram.len()
                );
            }
            seqs.extend(carried.into_iter().map(|e| e.id.seq));
        }
        seqs.sort_unstable();
        assert_eq!(
            seqs,
            (0..50).collect::<Vec<_>>(),
            "every entry is delivered exactly once, none lost or duplicated"
        );
    }

    // A single entry always produces exactly one datagram.
    #[test]
    fn pack_log_push_single_entry_one_datagram() {
        let from = StationId("me".into());
        let datagrams = pack_log_push(&[log_entry(1)], &from);
        assert_eq!(datagrams.len(), 1);
        assert_eq!(unpack(&datagrams[0]).len(), 1);
    }

    // No entries → no datagrams (the empty-backlog catch-up is a no-op).
    #[test]
    fn pack_log_push_empty_is_empty() {
        let from = StationId("me".into());
        assert!(pack_log_push(&[], &from).is_empty());
    }

    // The periodic re-push delivers our authored backlog to a peer, and ONLY our
    // own entries — a peer-authored entry held in the G-set is never re-pushed
    // (the echo-storm guard). Drives `repush_once` directly so there's no wait on
    // the 15 s timer.
    #[tokio::test]
    async fn repush_once_delivers_mine_only_to_peer() {
        let me = StationId("me".into());

        // A receiver socket stands in for the peer; `sender` is our gossip socket.
        let peer_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_addr = peer_sock.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();

        // Two of our own contacts plus one peer-authored contact in the G-set.
        let gset = Gset::default();
        assert!(gset.insert(log_entry(1)));
        assert!(gset.insert(log_entry(2)));
        assert!(gset.insert(peer_entry("w4ll", 9)));

        let peers = crate::peers::Peers::default();
        peers.add_target(peer_addr);

        let sent_to = repush_once(&sender, &peers, &me, &gset).await;
        assert_eq!(sent_to, 1, "the re-push reaches the single peer");

        // Drain datagrams until both of our entries are accounted for (mine fit
        // one datagram, but stay robust to MTU chunking); assert the peer's entry
        // never appears.
        let mut got: Vec<u64> = Vec::new();
        let mut buf = vec![0u8; RECV_BUF];
        while got.len() < 2 {
            let (n, _) =
                tokio::time::timeout(Duration::from_secs(5), peer_sock.recv_from(&mut buf))
                    .await
                    .expect("peer never received the re-push")
                    .unwrap();
            for e in unpack(&buf[..n]) {
                assert_eq!(e.id.origin, me, "only our own entries are re-pushed");
                got.push(e.id.seq);
            }
        }
        got.sort_unstable();
        assert_eq!(
            got,
            vec![1, 2],
            "both authored entries delivered, the peer's entry excluded",
        );
    }
}
