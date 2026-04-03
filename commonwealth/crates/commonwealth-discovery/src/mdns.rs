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
    pub fn new(
        node_id: NodeId,
        mesh_id_hex: &str,
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
                                let name = props.get_property_val_str("name").unwrap_or_default();

                                // Pick the first address.
                                let addr = info.get_addresses().iter().next().copied();
                                let port = info.get_port();

                                if let Some(ip) = addr {
                                    let socket_addr = SocketAddr::new(ip, port);
                                    debug!(
                                        peer_name = name,
                                        address = %socket_addr,
                                        "discovered peer via mDNS"
                                    );

                                    // We can't easily parse NodeId from display string,
                                    // so we store by full_name for dedup.
                                    let peer = DiscoveredPeer {
                                        // Generate a placeholder NodeId; real ID comes
                                        // from the gossip handshake.
                                        node_id: NodeId::generate(),
                                        mesh_id_hex: mesh_id_hex.to_string(),
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
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.unregister();
        if let Err(e) = self.daemon.shutdown() {
            warn!("mDNS daemon shutdown error: {e}");
        }
    }
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
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "commonwealth-node".to_string())
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
