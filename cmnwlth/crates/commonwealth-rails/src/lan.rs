// SPDX-License-Identifier: AGPL-3.0-or-later
//! mDNS on the LAN — a founder announces itself, a joiner finds it — still by
//! key, never in plaintext.
//!
//! Advertise and browse are `commonwealth_discovery::mdns`'s, the same
//! service type the inference daemon speaks. What a cw-rails advertisement
//! adds is its node pubkey (the `node_pubkey` TXT field) with its iroh
//! endpoint's UDP port as the service port, so a browser dials
//! `<pubkey>@<address>` over iroh: the QUIC handshake verifies the key, and
//! the join key decides admission. The advertisement is a hint and nothing
//! more (commonwealth-discovery's "discovery is a hint, never an
//! authorization").
//!
//! Opt-in: `cw-rails run --mdns` advertises; `cw-rails join` browses only for
//! an invite that carries no dial string.

use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};

use crate::RailsNode;

/// How long a joiner with no dial listens for a founder on the LAN.
pub const LAN_WAIT: Duration = Duration::from_secs(8);

/// A running advertisement and the browse beside it. Dropping it withdraws
/// the advertisement.
pub struct Lan {
    pub mdns: Arc<MdnsDiscovery>,
    _browse: BrowseHandle,
}

/// Announce `node` on the LAN as a member of `mesh`, and browse for the rest.
pub fn advertise(node: &RailsNode, mesh: &Mesh) -> Result<Lan, String> {
    let port = node
        .endpoint
        .bound_sockets()
        .into_iter()
        .find(|a| a.is_ipv4())
        .map(|a| a.port())
        .ok_or("the iroh endpoint bound no IPv4 socket to advertise")?;
    let pubkey = hex::encode(node.pubkey().0);
    let mdns = MdnsDiscovery::new_keyed(
        node.self_id,
        &hex::encode(mesh.id.as_bytes()),
        &mesh.name,
        &node.config.name,
        port,
        Some(&pubkey),
    )
    .map_err(|e| e.to_string())?;
    let (tx, _rx) = tokio::sync::mpsc::channel::<DiscoveredPeer>(32);
    let browse = mdns.browse(tx).map_err(|e| e.to_string())?;
    tracing::info!(target: "rails", mesh = %mesh.name, port, "lan: advertising on mDNS by key");
    Ok(Lan {
        mdns: Arc::new(mdns),
        _browse: browse,
    })
}

/// The iroh dial string a keyed advertisement names, or `None` for one that
/// carries no key (a daemon's plaintext port, which this process never
/// speaks to).
pub fn dial_of(peer: &DiscoveredPeer) -> Option<String> {
    peer.node_pubkey
        .as_ref()
        .map(|key| format!("{key}@{}", peer.address))
}

/// Listen for `wait` and return the dial of every keyed advertisement for
/// `mesh_name` (any mesh when the invite named none).
pub async fn founders(mesh_name: Option<&str>, wait: Duration) -> Result<Vec<String>, String> {
    let mdns = MdnsDiscovery::browser().map_err(|e| e.to_string())?;
    let (tx, _rx) = tokio::sync::mpsc::channel::<DiscoveredPeer>(32);
    let _browse = mdns.browse(tx).map_err(|e| e.to_string())?;
    tokio::time::sleep(wait).await;
    let seen = mdns.discovered_peers();
    let dials: Vec<String> = seen
        .iter()
        .filter(|p| mesh_name.is_none_or(|n| p.mesh_name == n))
        .filter_map(dial_of)
        .collect();
    tracing::info!(target: "rails", seen = seen.len(), keyed = dials.len(), mesh = ?mesh_name,
                   "lan: browsed for a founder");
    Ok(dials)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(key: Option<&str>) -> DiscoveredPeer {
        DiscoveredPeer {
            node_id: commonwealth_core::ids::NodeId::generate(),
            mesh_id_hex: String::new(),
            mesh_name: "Lab".into(),
            name: "founder".into(),
            address: "192.168.1.7:41000".parse().unwrap(),
            node_pubkey: key.map(str::to_string),
        }
    }

    /// A keyed advertisement is a dial string the transport parses; an
    /// unkeyed one (a daemon's plaintext port) is never dialed.
    #[test]
    fn only_a_keyed_advertisement_is_a_dial() {
        let key = hex::encode([3u8; 32]);
        let dial = dial_of(&peer(Some(&key))).expect("keyed");
        assert_eq!(dial, format!("{key}@192.168.1.7:41000"));
        assert!(commonwealth_transport::iroh::parse_dial_string(&dial).is_ok());
        assert_eq!(dial_of(&peer(None)), None);
    }
}
