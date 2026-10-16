// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use commonwealth_core::ids::NodeId;
use commonwealth_core::{Error, Result};

/// Service type for Commonwealth mDNS advertisement.
const SERVICE_TYPE: &str = "_commonwealth._tcp.local.";

/// A discovered peer node on the local network.
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub node_id: NodeId,
    pub mesh_id_hex: String,
    /// The peer's *mesh* name (e.g. "Masonic Mesh") — what the
    /// joiner matches on to decide whether this is the right mesh
    /// to handshake with. May be empty for peers advertising via
    /// older builds that only broadcast `name` as the node name.
    pub mesh_name: String,
    /// The peer's *node* name — the human-readable label of the
    /// machine itself (usually the system hostname or the
    /// `node_name` the founder supplied). Used for display in
    /// diagnostics, not for mesh membership decisions.
    pub name: String,
    pub address: SocketAddr,
    /// The advertiser's node pubkey (hex), when it advertises one: then
    /// `address` is its iroh endpoint's UDP port, dialed by key rather than
    /// spoken to in plaintext. `None` from a daemon, which advertises its
    /// internal HTTP port.
    pub node_pubkey: Option<String>,
}

/// mDNS advertiser and browser for Commonwealth nodes.
pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    instance_name: String,
    discovered: Arc<Mutex<HashMap<String, DiscoveredPeer>>>,
}

impl MdnsDiscovery {
    /// Register this node on the local network via mDNS.
    ///
    /// `node_name` is the human label for this machine (hostname or
    /// the user-supplied node name). `mesh_name` is the name of the
    /// mesh this node belongs to — used by the joiner to filter
    /// candidates. They were historically conflated into a single
    /// `name` TXT field, which meant the joiner's filter
    /// `peer.name == mesh_name` never matched; `perform_join`
    /// unconditionally timed out with `NoPeerFound`.
    pub fn new(
        node_id: NodeId,
        mesh_id_hex: &str,
        mesh_name: &str,
        node_name: &str,
        internal_port: u16,
    ) -> Result<Self> {
        Self::new_keyed(
            node_id,
            mesh_id_hex,
            mesh_name,
            node_name,
            internal_port,
            None,
        )
    }

    /// [`MdnsDiscovery::new`] for a node reached by key: `port` is its iroh
    /// endpoint's UDP port and `node_pubkey` rides the `node_pubkey` TXT
    /// field, so a browser can dial `<node_pubkey>@<address>` over iroh.
    pub fn new_keyed(
        node_id: NodeId,
        mesh_id_hex: &str,
        mesh_name: &str,
        node_name: &str,
        internal_port: u16,
        node_pubkey: Option<&str>,
    ) -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| Error::Discovery(format!("failed to create mDNS daemon: {e}")))?;

        let instance_name = format!("{node_id}");
        let host = format!("{}.local.", hostname());

        let mut properties = HashMap::new();
        properties.insert("node_id".to_string(), format!("{node_id}"));
        properties.insert("mesh_id".to_string(), mesh_id_hex.to_string());
        properties.insert("mesh_name".to_string(), mesh_name.to_string());
        // Keep `name` for backwards compat with older peers that
        // treated it as the node name.
        properties.insert("name".to_string(), node_name.to_string());
        if let Some(key) = node_pubkey {
            properties.insert("node_pubkey".to_string(), key.to_string());
        }

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host,
            (),
            internal_port,
            properties,
        )
        .map_err(|e| Error::Discovery(format!("failed to create service info: {e}")))?;
        // With `()` for addresses and no addr-auto, mdns-sd announces on no
        // interface at all (its `prepare_announce`: "No valid addrs"), so a
        // browser never resolves the peer. The keyed path turns addr-auto
        // on; the unkeyed one keeps its behaviour, which the inference
        // daemon relies on until pb-mesh-exit-mesh retires its mDNS.
        let service = if node_pubkey.is_some() {
            service.enable_addr_auto()
        } else {
            service
        };

        daemon
            .register(service)
            .map_err(|e| Error::Discovery(format!("failed to register mDNS service: {e}")))?;

        info!(
            node_id = %node_id,
            service_type = SERVICE_TYPE,
            port = internal_port,
            mesh_name,
            node_name,
            keyed = node_pubkey.is_some(),
            "mDNS service registered"
        );

        Ok(Self {
            daemon,
            instance_name,
            discovered: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// A browser that advertises nothing: for a node that is not on a mesh
    /// yet and so has nothing to announce, only founders to find.
    pub fn browser() -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| Error::Discovery(format!("failed to create mDNS daemon: {e}")))?;
        info!("mDNS browser created — advertising nothing");
        Ok(Self {
            daemon,
            // Matches no advertisement, so the own-instance skip never fires.
            instance_name: format!("browser-{}", NodeId::generate()),
            discovered: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Start browsing for other Commonwealth nodes. Sends discovered peers to the channel.
    /// This spawns a background task that runs until the returned handle is dropped.
    pub fn browse(&self, tx: mpsc::Sender<DiscoveredPeer>) -> Result<BrowseHandle> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| Error::Discovery(format!("failed to browse mDNS: {e}")))?;

        let discovered = Arc::clone(&self.discovered);
        let own_instance = self.instance_name.clone();

        info!(
            service_type = SERVICE_TYPE,
            "mDNS browse subscribed — waiting for peer advertisements"
        );

        let handle = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::task::spawn_blocking({
                        let receiver = receiver.clone();
                        move || receiver.recv_timeout(Duration::from_secs(5))
                    }),
                )
                .await
                {
                    Ok(Ok(Ok(event))) => {
                        match event {
                            ServiceEvent::ServiceResolved(info) => {
                                let full_name = info.get_fullname().to_string();

                                // Skip our own advertisement.
                                if full_name.contains(&own_instance) {
                                    continue;
                                }

                                let props = info.get_properties();
                                let _node_id_str =
                                    props.get_property_val_str("node_id").unwrap_or_default();
                                let mesh_id_hex =
                                    props.get_property_val_str("mesh_id").unwrap_or_default();
                                let mesh_name =
                                    props.get_property_val_str("mesh_name").unwrap_or_default();
                                let name = props.get_property_val_str("name").unwrap_or_default();
                                let node_pubkey = props
                                    .get_property_val_str("node_pubkey")
                                    .map(str::to_string);

                                let addr = dialable_address(info.get_addresses());
                                let port = info.get_port();

                                if let Some(ip) = addr {
                                    let socket_addr = SocketAddr::new(ip, port);
                                    info!(
                                        peer_name = name,
                                        mesh_name = mesh_name,
                                        mesh_id = %mesh_id_hex,
                                        address = %socket_addr,
                                        keyed = node_pubkey.is_some(),
                                        "mDNS: discovered peer"
                                    );

                                    // We can't easily parse NodeId from display string,
                                    // so we store by full_name for dedup.
                                    let peer = DiscoveredPeer {
                                        // Generate a placeholder NodeId; real ID comes
                                        // from the gossip handshake.
                                        node_id: NodeId::generate(),
                                        mesh_id_hex: mesh_id_hex.to_string(),
                                        mesh_name: mesh_name.to_string(),
                                        name: name.to_string(),
                                        address: socket_addr,
                                        node_pubkey,
                                    };

                                    discovered.lock().unwrap().insert(full_name, peer.clone());

                                    if tx.send(peer).await.is_err() {
                                        break; // Receiver dropped.
                                    }
                                }
                            }
                            ServiceEvent::ServiceRemoved(_, full_name) => {
                                debug!(service = full_name, "peer removed from mDNS");
                                discovered.lock().unwrap().remove(&full_name);
                            }
                            _ => {}
                        }
                    }
                    Ok(Ok(Err(_))) => {
                        // recv_timeout timed out, just loop.
                    }
                    Ok(Err(e)) => {
                        warn!("mDNS browse task error: {e}");
                    }
                    Err(_) => {
                        // Outer timeout, just loop.
                    }
                }
            }
        });

        Ok(BrowseHandle { _task: handle })
    }

    /// Get all currently discovered peers.
    pub fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        self.discovered.lock().unwrap().values().cloned().collect()
    }

    /// Unregister this node from mDNS.
    pub fn unregister(&self) -> Result<()> {
        let full_name = format!("{}.{}", self.instance_name, SERVICE_TYPE);
        self.daemon
            .unregister(&full_name)
            .map_err(|e| Error::Discovery(format!("failed to unregister mDNS: {e}")))?;
        info!("mDNS service unregistered");
        Ok(())
    }

    /// Advertise a mesh app on the local network.
    ///
    /// The service type is `_cwapp-{sanitized_app_id}._tcp.local.`
    /// where the app_id is lowercased with dots replaced by dashes.
    pub fn advertise_app(&self, app_id: &str, app_port: u16) -> Result<()> {
        let service_type = app_service_type(app_id);
        let instance = format!("{}-{}", self.instance_name, sanitize_app_id(app_id));
        let host = format!("{}.local.", hostname());

        let mut properties = HashMap::new();
        properties.insert("app_id".to_string(), app_id.to_string());
        properties.insert("node_id".to_string(), self.instance_name.clone());

        let service =
            ServiceInfo::new(&service_type, &instance, &host, (), app_port, properties)
                .map_err(|e| Error::Discovery(format!("failed to create app service info: {e}")))?;

        self.daemon
            .register(service)
            .map_err(|e| Error::Discovery(format!("failed to advertise app: {e}")))?;

        info!(app_id, app_port, "app advertised via mDNS");
        Ok(())
    }

    /// Withdraw a previously advertised app from mDNS.
    pub fn withdraw_app(&self, app_id: &str) -> Result<()> {
        let service_type = app_service_type(app_id);
        let instance = format!("{}-{}", self.instance_name, sanitize_app_id(app_id));
        let full_name = format!("{}.{}", instance, service_type);
        self.daemon
            .unregister(&full_name)
            .map_err(|e| Error::Discovery(format!("failed to withdraw app: {e}")))?;
        info!(app_id, "app withdrawn from mDNS");
        Ok(())
    }

    /// Browse for mesh apps of a specific app_id across the network.
    pub fn browse_apps(
        &self,
        app_id: &str,
        tx: mpsc::Sender<DiscoveredApp>,
    ) -> Result<BrowseHandle> {
        let service_type = app_service_type(app_id);
        let receiver = self
            .daemon
            .browse(&service_type)
            .map_err(|e| Error::Discovery(format!("failed to browse apps: {e}")))?;

        let own_instance = self.instance_name.clone();
        let app_id_owned = app_id.to_string();

        let handle = tokio::spawn(async move {
            loop {
                if let Ok(Ok(Ok(mdns_sd::ServiceEvent::ServiceResolved(info)))) =
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        tokio::task::spawn_blocking({
                            let receiver = receiver.clone();
                            move || receiver.recv_timeout(std::time::Duration::from_secs(5))
                        }),
                    )
                    .await
                {
                    let full_name = info.get_fullname().to_string();
                    if full_name.contains(&own_instance) {
                        continue;
                    }

                    let props = info.get_properties();
                    let node_id_str = props.get_property_val_str("node_id").unwrap_or_default();
                    let _ = node_id_str; // node_id comes from gossip handshake

                    let port = info.get_port();
                    if let Some(ip) = dialable_address(info.get_addresses()) {
                        let app = DiscoveredApp {
                            app_id: app_id_owned.clone(),
                            node_id: commonwealth_core::ids::NodeId::generate(),
                            address: std::net::SocketAddr::new(ip, port),
                            port,
                        };
                        if tx.send(app).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(BrowseHandle { _task: handle })
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.unregister();
        if let Err(e) = self.daemon.shutdown() {
            warn!("mDNS daemon shutdown error: {e}");
        }
    }
}

/// A mesh app discovered via mDNS.
#[derive(Debug, Clone)]
pub struct DiscoveredApp {
    pub app_id: String,
    pub node_id: NodeId,
    pub address: std::net::SocketAddr,
    pub port: u16,
}

/// Handle for a running mDNS browse task. Dropping it cancels browsing.
pub struct BrowseHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Get the local hostname (best-effort).
///
/// `HOSTNAME` / `COMPUTERNAME` env vars are unreliable — macOS
/// doesn't export HOSTNAME to launched apps by default, so the old
/// fallback to "commonwealth-node" was hitting for every mDNS
/// registration. Use the cross-platform `hostname` crate (wraps
/// `gethostname(2)` / `GetComputerNameW`) instead.
fn hostname() -> String {
    ::hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "commonwealth-node".to_string())
}

/// Convert an app_id to a valid mDNS label (lowercase, dots → dashes).
fn sanitize_app_id(app_id: &str) -> String {
    app_id.to_lowercase().replace('.', "-")
}

/// Build the mDNS service type for a mesh app.
fn app_service_type(app_id: &str) -> String {
    format!("_cwapp-{}._tcp.local.", sanitize_app_id(app_id))
}

/// The one address of a resolved service to dial. mdns-sd hands back an
/// unordered set, and with addr-auto it holds IPv6 link-local addresses that
/// carry no scope id and so dial nothing: IPv4 first, then routable IPv6, and
/// never a link-local one. A resolution holding only link-local addresses is
/// no peer yet — mdns-sd's own " (2)" conflict rename of an advertiser that
/// hears itself on a second interface resolves exactly that way.
fn dialable_address(
    addrs: &std::collections::HashSet<std::net::IpAddr>,
) -> Option<std::net::IpAddr> {
    let rank = |ip: &std::net::IpAddr| match ip {
        std::net::IpAddr::V4(_) => Some(0),
        std::net::IpAddr::V6(v6) if !v6.is_unicast_link_local() => Some(1),
        std::net::IpAddr::V6(_) => None,
    };
    let chosen = addrs
        .iter()
        .filter_map(|ip| rank(ip).map(|r| (r, *ip)))
        .min()
        .map(|(_, ip)| ip);
    debug!(candidates = addrs.len(), chosen = ?chosen, "mDNS: chose the address to dial");
    chosen
}

/// The mDNS group (RFC 6762 §3), where mdns-sd sends every announcement.
const GROUP_V4: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
const MDNS_PORT: u16 = 5353;

/// Whether this host lets a send to the mDNS group leave on at least one
/// interface mdns-sd announces on (every non-loopback address): the first
/// address that accepted one, or why none did.
///
/// mdns-sd 0.12 cannot answer this. A refused send is an `info!` in its own
/// log (`send_packet` returns `()`), `DaemonEvent::Announce` fires once a
/// packet is built whether or not it left, and an IPv4 socket whose test send
/// fails is dropped at `debug!` — so `register` is Ok on a host where no
/// announcement ever leaves (measured 2026-10-16: errno 65 on all 14
/// addresses). The probe is that same test packet, an empty 12-byte header
/// no responder answers, sent the way mdns-sd sends (`IP_MULTICAST_IF`, or
/// the IPv6 scope id), and it stops at the first acceptance.
pub fn group_reach() -> Result<String> {
    let mut addrs = if_addrs::get_if_addrs().map_err(|e| {
        warn!(error = %e, "mDNS: cannot list this host's interfaces to probe the group");
        Error::Discovery(format!("cannot list this host's interfaces: {e}"))
    })?;
    // IPv4 first, so `via` names a LAN address rather than lo0's fe80::1,
    // which `is_loopback` keeps (it is link-local) and mdns-sd announces on.
    addrs.sort_by_key(|i| i.ip().is_ipv6());
    let probes = addrs.into_iter().filter(|i| !i.is_loopback()).map(|i| {
        let label = format!("{}/{}", i.name, i.ip());
        let sent = send_to_group(&i);
        debug!(via = %label, refused = ?sent.as_ref().err(), "mDNS: probed a send to the group");
        (label, sent)
    });
    match first_accepting(probes) {
        Ok(via) => {
            info!(via = %via, "mDNS: the group accepts a send");
            Ok(via)
        }
        Err(why) => {
            warn!(why = %why, "mDNS: no interface accepts a send to the group");
            Err(Error::Discovery(why))
        }
    }
}

fn send_to_group(intf: &if_addrs::Interface) -> std::io::Result<()> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let (sock, group) = match intf.ip() {
        IpAddr::V4(ip) => {
            let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            sock.set_multicast_if_v4(&ip)?;
            (sock, SocketAddr::from((GROUP_V4, MDNS_PORT)))
        }
        IpAddr::V6(_) => {
            // Not mdns-sd's `unwrap_or(0)`: index 0 lets the kernel pick, so
            // an acceptance would be some other interface's, not this one's.
            let index = intf
                .index
                .ok_or_else(|| std::io::Error::other("no interface index to pin the send to"))?;
            let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
            sock.set_multicast_if_v6(index)?;
            (
                sock,
                SocketAddr::V6(SocketAddrV6::new(GROUP_V6, MDNS_PORT, 0, index)),
            )
        }
    };
    sock.send_to(&[0u8; 12], &SockAddr::from(group)).map(|_| ())
}

/// The first address that accepted a send, or every refusal grouped by
/// cause. Lazy: nothing past the first acceptance is probed.
fn first_accepting(
    probes: impl IntoIterator<Item = (String, std::io::Result<()>)>,
) -> std::result::Result<String, String> {
    let mut refused: Vec<(String, Vec<String>)> = Vec::new();
    for (label, sent) in probes {
        let cause = match sent {
            Ok(()) => return Ok(label),
            Err(e) => e.to_string(),
        };
        match refused.iter_mut().find(|(c, _)| *c == cause) {
            Some((_, labels)) => labels.push(label),
            None => refused.push((cause, vec![label])),
        }
    }
    if refused.is_empty() {
        return Err(
            "this host has no non-loopback interface to send an mDNS announcement on".into(),
        );
    }
    let causes: Vec<String> = refused
        .iter()
        .map(|(cause, labels)| {
            format!(
                "{cause} on {} addresses ({})",
                labels.len(),
                labels.join(", ")
            )
        })
        .collect();
    Err(format!(
        "no interface accepted a send to the mDNS group, so no announcement would leave this host: {}",
        causes.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured 2026-09-26: a keyed founder's " (2)" record resolved to
    /// `fe80::` addresses only and the join timed out on it for 15 s.
    #[test]
    fn a_link_local_address_is_never_chosen() {
        let set = |a: &[&str]| a.iter().map(|s| s.parse().unwrap()).collect();
        let v4: std::net::IpAddr = "192.168.1.12".parse().unwrap();
        assert_eq!(
            dialable_address(&set(&["fe80::1", "192.168.1.12", "fe80::2"])),
            Some(v4)
        );
        assert_eq!(
            dialable_address(&set(&["fe80::1", "fd7a::1"])),
            Some("fd7a::1".parse().unwrap())
        );
        assert_eq!(dialable_address(&set(&["fe80::1", "fe80::2"])), None);
        assert_eq!(dialable_address(&set(&[])), None);
    }

    fn refused(label: &str) -> (String, std::io::Result<()>) {
        let host_said = std::io::Error::other("No route to host (os error 65)");
        (label.to_string(), Err(host_said))
    }

    /// Measured 2026-10-16: every send to the group failed with errno 65 and
    /// mdns-sd's `register` was Ok, so `cw-rails run --mdns` said it was
    /// advertising while no packet left the host.
    #[test]
    fn a_host_that_refuses_every_send_does_not_reach_the_group() {
        let why = first_accepting([refused("en0/192.168.1.4"), refused("en0/fe80::1")])
            .expect_err("no interface accepted a send");
        assert!(
            why.contains("No route to host (os error 65) on 2 addresses"),
            "{why}"
        );
        assert!(why.contains("en0/192.168.1.4, en0/fe80::1"), "{why}");
        let why = first_accepting([]).expect_err("no interface to send on");
        assert!(why.contains("no non-loopback interface"), "{why}");
    }

    /// One accepting interface is reach, and the probe stops there: a host
    /// whose multicast works sends one packet, not one per address.
    #[test]
    fn the_first_accepting_interface_is_the_reach() {
        let mut probed = 0;
        let probes = [
            refused("utun4/100.104.36.28"),
            ("en0/192.168.1.4".to_string(), Ok(())),
            refused("en0/fe80::1"),
        ]
        .into_iter()
        .inspect(|_| probed += 1);
        assert_eq!(first_accepting(probes).as_deref(), Ok("en0/192.168.1.4"));
        assert_eq!(probed, 2);
    }

    #[test]
    fn service_type_format() {
        assert!(SERVICE_TYPE.starts_with('_'));
        assert!(SERVICE_TYPE.ends_with('.'));
        assert!(SERVICE_TYPE.contains("._tcp."));
    }
}
