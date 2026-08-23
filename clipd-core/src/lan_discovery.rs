//! Finding the other machine on the network, without being told where it is.
//!
//! Machines announce themselves over mDNS (Bonjour) as [`SERVICE_TYPE`] and
//! carry their clipd device id and name in TXT records, so a peer discovered
//! here lines up with the same identity the pairing and folder transports use.
//!
//! Discovery is *not* on the send path. Browsing takes a second or two, which
//! would undo the whole point of a LAN transport, so the daemon browses
//! continuously in the background and keeps a small cache on disk. Sending is
//! then a cache read and a TCP connect.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

pub use crate::lan::SERVICE_TYPE;

/// How long a cached peer stays usable after it was last seen.
///
/// Long enough to survive a browse hiccup or a laptop sleeping briefly, short
/// enough that a machine which left the network stops being offered.
pub const PEER_FRESH_SECS: i64 = 90;

/// A machine found on the local network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanPeer {
    pub device_id: String,
    pub name: String,
    pub addr: SocketAddr,
    pub seen_at: chrono::DateTime<chrono::Utc>,
}

impl LanPeer {
    pub fn is_fresh(&self) -> bool {
        chrono::Utc::now() - self.seen_at < chrono::Duration::seconds(PEER_FRESH_SECS)
    }
}

fn cache_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("lan-peers.json")
}

/// Machines seen on the network recently, newest first.
pub fn cached_peers() -> Vec<LanPeer> {
    let Ok(raw) = std::fs::read_to_string(cache_path()) else {
        return Vec::new();
    };
    let map: BTreeMap<String, LanPeer> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            log::debug!("ignoring unreadable LAN peer cache: {e}");
            return Vec::new();
        }
    };
    let mut peers: Vec<LanPeer> = map.into_values().filter(LanPeer::is_fresh).collect();
    peers.sort_by(|a, b| b.seen_at.cmp(&a.seen_at));
    peers
}

/// Look up one machine by device id, if it is on the network right now.
pub fn find_peer(device_id: &str) -> Option<LanPeer> {
    cached_peers().into_iter().find(|p| p.device_id == device_id)
}

fn write_cache(peers: &BTreeMap<String, LanPeer>) {
    match serde_json::to_vec_pretty(peers) {
        Ok(bytes) => {
            if let Err(e) = crate::devices::write_atomically(&cache_path(), &bytes) {
                log::debug!("couldn't write the LAN peer cache: {e}");
            }
        }
        Err(e) => log::debug!("couldn't encode the LAN peer cache: {e}"),
    }
}

/// Announce this machine, and keep the peer cache fresh, until `stop` is set.
///
/// Runs the browse loop on the calling thread — the daemon gives it one.
pub fn run(
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
    use std::sync::atomic::Ordering;

    let mdns = ServiceDaemon::new().map_err(|e| format!("Couldn't start mDNS: {e}"))?;

    let me = crate::devices::device_id();
    let my_name = crate::devices::device_name();
    let mut props = std::collections::HashMap::new();
    props.insert("device_id".to_string(), me.clone());
    props.insert("name".to_string(), my_name.clone());

    // The instance name must be unique on the network; the device id already is.
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &me,
        &format!("{me}.local."),
        (),
        port,
        Some(props),
    )
    .map_err(|e| format!("Couldn't describe this machine for mDNS: {e}"))?
    // Let mdns-sd track this host's addresses rather than us enumerating
    // interfaces — Wi-Fi and Ethernet come and go.
    .enable_addr_auto();

    mdns.register(service)
        .map_err(|e| format!("Couldn't announce this machine: {e}"))?;
    log::info!("📡 Announced this machine on the network as \"{my_name}\" (port {port})");

    let receiver = mdns
        .browse(SERVICE_TYPE)
        .map_err(|e| format!("Couldn't browse for other machines: {e}"))?;

    let mut peers: BTreeMap<String, LanPeer> = BTreeMap::new();
    while !stop.load(Ordering::Relaxed) {
        let event = match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(e) => e,
            Err(_) => continue, // timeout: just re-check `stop`
        };

        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(device_id) = info.get_property_val_str("device_id") else {
                    continue;
                };
                // Our own announcement comes back to us; it is not a peer.
                if device_id == me {
                    continue;
                }
                let Some(addr) = info.get_addresses().iter().next().copied() else {
                    continue;
                };
                let name = info
                    .get_property_val_str("name")
                    .unwrap_or(device_id)
                    .to_string();

                let peer = LanPeer {
                    device_id: device_id.to_string(),
                    name: name.clone(),
                    addr: SocketAddr::new(addr, info.get_port()),
                    seen_at: chrono::Utc::now(),
                };
                if peers.insert(device_id.to_string(), peer).is_none() {
                    log::info!("📡 Found {name} on the network");
                }
                write_cache(&peers);
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                // Instance name is the device id, so this identifies the peer.
                let leaving = fullname
                    .split('.')
                    .next()
                    .map(str::to_string)
                    .unwrap_or_default();
                if peers.remove(&leaving).is_some() {
                    log::info!("📡 {leaving} left the network");
                    write_cache(&peers);
                }
            }
            _ => {}
        }
    }

    let _ = mdns.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn peer(id: &str, secs_ago: i64) -> LanPeer {
        LanPeer {
            device_id: id.into(),
            name: format!("Mac {id}"),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 5555),
            seen_at: chrono::Utc::now() - chrono::Duration::seconds(secs_ago),
        }
    }

    #[test]
    fn a_recently_seen_peer_is_fresh() {
        assert!(peer("a", 0).is_fresh());
        assert!(peer("a", PEER_FRESH_SECS - 5).is_fresh());
    }

    #[test]
    fn a_machine_that_left_the_network_goes_stale() {
        // Otherwise sends keep being routed at an address nobody is listening on.
        assert!(!peer("a", PEER_FRESH_SECS + 5).is_fresh());
    }

    #[test]
    fn the_cache_round_trips_through_json() {
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), peer("a", 0));
        let json = serde_json::to_string(&map).expect("encode");
        let back: BTreeMap<String, LanPeer> = serde_json::from_str(&json).expect("decode");
        assert_eq!(back["a"].addr.port(), 5555);
        assert_eq!(back["a"].name, "Mac a");
    }
}
