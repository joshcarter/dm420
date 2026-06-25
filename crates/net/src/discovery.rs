//! mDNS / DNS-SD peer discovery on the LAN.
//!
//! Advertises this instance under `_dm420._udp.local.` and browses for others,
//! feeding every resolved address into the [`Peers`] target set. We don't bother
//! filtering our *own* advertisement here — the receive loop already drops frames
//! whose `from` is our own [`StationId`], so beaconing to ourselves is at worst a
//! little wasted loopback traffic.
//!
//! Discovery is best-effort: if the daemon can't start (no multicast, locked-down
//! network), the caller logs it and falls back to manual `DM420_PEERS`.

use std::collections::HashMap;
use std::net::SocketAddr;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use types::StationId;

use crate::peers::Peers;

const SERVICE_TYPE: &str = "_dm420._udp.local.";

/// Register our service and start browsing. Spawns a task that owns the daemon
/// (dropping it would shut mDNS down) and folds resolved peers into `peers`.
pub fn spawn(station: StationId, udp_port: u16, peers: Peers) -> Result<(), mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    register(&daemon, &station, udp_port)?;

    let receiver = daemon.browse(SERVICE_TYPE)?;
    tokio::spawn(async move {
        // Hold the daemon for the life of the browse so mDNS stays up.
        let _daemon = daemon;
        while let Ok(event) = receiver.recv_async().await {
            if let ServiceEvent::ServiceResolved(info) = event {
                let port = info.get_port();
                for ip in info.get_addresses() {
                    let addr = SocketAddr::new(*ip, port);
                    // The gossip socket is IPv4 (`NetConfig.bind` is
                    // `Ipv4Addr::UNSPECIFIED`), so non-IPv4 resolved addresses — mDNS
                    // hands back IPv6 link-local `fe80::…` too — can't be sent to and
                    // would fail every beacon with EINVAL. Skip them at the source.
                    if !crate::is_sendable(&addr) {
                        tracing::debug!(%addr, service = info.get_fullname(), "net: skipping non-IPv4 mDNS address");
                        continue;
                    }
                    tracing::info!(%addr, service = info.get_fullname(), "net: mDNS resolved peer");
                    peers.add_target(addr);
                }
            }
        }
        tracing::warn!("net: mDNS browse channel closed");
    });
    Ok(())
}

fn register(daemon: &ServiceDaemon, station: &StationId, port: u16) -> Result<(), mdns_sd::Error> {
    let instance = sanitize(&station.0);
    let host = format!("{instance}.local.");
    let mut props = HashMap::new();
    props.insert("sid".to_string(), station.0.clone());
    // Empty `ip` + `enable_addr_auto` lets the daemon advertise (and keep current)
    // this host's own addresses, so we don't have to enumerate interfaces.
    let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", port, props)?.enable_addr_auto();
    daemon.register(info)
}

/// mDNS instance/host labels must be DNS-safe; map anything else to `-`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
