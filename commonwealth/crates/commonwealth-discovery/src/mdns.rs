use std::collections::HashMap;
use std::net::SocketAddr;
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

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host,
            (),
            internal_port,
            properties,
        )
        .map_err(|e| Error::Discovery(format!("failed to create service info: {e}")))?;

        daemon
            .register(service)
            .map_err(|e| Error::Discovery(format!("failed to register mDNS service: {e}")))?;

        info!(
            node_id = %node_id,
            service_type = SERVICE_TYPE,
            port = internal_port,
            mesh_name,
            node_name,
            "mDNS service registered"
        );

        Ok(Self {
            daemon,
            instance_name,
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

                                // Pick the first address.
                                let addr = info.get_addresses().iter().next().copied();
                                let port = info.get_port();

                                if let Some(ip) = addr {
                                    let socket_addr = SocketAddr::new(ip, port);
                                    info!(
                                        peer_name = name,
                                        mesh_name = mesh_name,
                                        mesh_id = %mesh_id_hex,
                                        address = %socket_addr,
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

        let service = ServiceInfo::new(&service_type, &instance, &host, (), app_port, properties)
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
    pub fn browse_apps(&self, app_id: &str, tx: mpsc::Sender<DiscoveredApp>) -> Result<BrowseHandle> {
        let service_type = app_service_type(app_id);
        let receiver = self
            .daemon
            .browse(&service_type)
            .map_err(|e| Error::Discovery(format!("failed to browse apps: {e}")))?;

        let own_instance = self.instance_name.clone();
        let app_id_owned = app_id.to_string();

        let handle = tokio::spawn(async move {
            loop {
                if let Ok(Ok(Ok(event))) = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    tokio::task::spawn_blocking({
                        let receiver = receiver.clone();
                        move || receiver.recv_timeout(std::time::Duration::from_secs(5))
                    }),
                )
                .await {
                    if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                        let full_name = info.get_fullname().to_string();
                        if full_name.contains(&own_instance) {
                            continue;
                        }

                        let props = info.get_properties();
                        let node_id_str =
                            props.get_property_val_str("node_id").unwrap_or_default();
                        let _ = node_id_str; // node_id comes from gossip handshake

                        let port = info.get_port();
                        if let Some(ip) = info.get_addresses().iter().next().copied() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_format() {
        assert!(SERVICE_TYPE.starts_with('_'));
        assert!(SERVICE_TYPE.ends_with('.'));
        assert!(SERVICE_TYPE.contains("._tcp."));
    }
}
