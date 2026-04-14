//! Same-LAN mesh join handshake.
//!
//! Replaces the previous placeholder in `EmbeddedDaemon::join_mesh`
//! that just created a local empty mesh and hoped for the best. The
//! real flow:
//!
//! 1. Wait for `MdnsDiscovery` to surface candidate peers — up to
//!    `timeout` — filtered by `mesh_name` (the only discriminator the
//!    joiner has; mesh_id is only known to the founder side).
//! 2. For each candidate, POST `/internal/join` to the founder's
//!    internal API (port 9742) with the raw `join_key` and the
//!    joining node's identity.
//! 3. First `200` wins: deserialise the returned authoritative
//!    `Mesh` snapshot and return it. The caller installs it as
//!    local state.
//! 4. `401` → wrong mesh (hash mismatch) → try the next candidate.
//! 5. Timeout with no accepting peer → `Error::NoPeerFound`.
//!
//! Plain HTTP is used deliberately — see the security note in
//! `commonwealth-api::routes_internal::join`.
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::mdns::MdnsDiscovery;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Matches the server-side `JoinRequest` in
/// `commonwealth-api::routes_internal`. Kept as a separate type here
/// (rather than importing) because commonwealth-api doesn't export it
/// publicly, and duplicating the shape is cheaper than the churn of
/// making it public.
#[derive(Debug, Serialize)]
struct JoinRequestWire {
    join_key: String,
    joining_node_name: String,
    joining_node_addresses: Vec<SocketAddr>,
}

/// Members transit as a flat Vec because `HashMap<NodeId, _>` doesn't
/// round-trip through JSON (NodeId is an array, not a string key).
/// See `commonwealth_api::routes_internal::MeshWire`.
#[derive(Debug, Deserialize)]
struct MeshWire {
    id: commonwealth_core::ids::MeshId,
    name: String,
    join_key_hash: [u8; 32],
    members: Vec<commonwealth_core::mesh::MemberRecord>,
    peers: Vec<commonwealth_core::mesh::MeshPeering>,
}

impl MeshWire {
    fn into_mesh(self) -> Mesh {
        use std::collections::HashMap;
        let members = self
            .members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>();
        Mesh {
            id: self.id,
            name: self.name,
            join_key_hash: self.join_key_hash,
            members,
            peers: self.peers,
        }
    }
}

/// Mirror of the server-side `JoinResponse`.
#[derive(Debug, Deserialize)]
struct JoinResponseWire {
    assigned_node_id: NodeId,
    mesh: MeshWire,
}

/// Outcome of a successful handshake. The caller replaces its local
/// placeholder mesh with `mesh` and records `assigned_node_id` as
/// "this node's id in the joined mesh".
pub struct JoinHandshakeResult {
    pub mesh: Mesh,
    pub assigned_node_id: NodeId,
}

#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// No peer on the LAN accepted the join (either none advertised
    /// the expected mesh_name in `timeout`, or every one rejected
    /// the join key as invalid).
    #[error("no peer on this network accepted the join key for mesh '{mesh_name}'")]
    NoPeerFound { mesh_name: String },

    /// An accepting peer returned a malformed response. Rare; usually
    /// a version mismatch between the founder and joiner binaries.
    #[error("peer at {address} returned a malformed response: {reason}")]
    BadResponse {
        address: SocketAddr,
        reason: String,
    },
}

/// Execute the join handshake. Polls `mdns` for peers advertising
/// `mesh_name` and POSTs each one until one accepts the key or the
/// timeout elapses.
pub async fn perform_join(
    mesh_name: &str,
    join_key: &str,
    joining_node_name: &str,
    joining_node_addresses: Vec<SocketAddr>,
    mdns: &MdnsDiscovery,
    timeout: Duration,
) -> Result<JoinHandshakeResult, JoinError> {
    // 3-second per-peer HTTP timeout. With a 5s overall budget this
    // leaves one retry with a fresh mDNS candidate if the first peer
    // is flaky.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("reqwest client build");

    let body = JoinRequestWire {
        join_key: join_key.to_string(),
        joining_node_name: joining_node_name.to_string(),
        joining_node_addresses,
    };

    let start = Instant::now();
    // Track attempted peer addresses so we don't spam the same node
    // when mDNS re-resolves it repeatedly.
    let mut attempted: Vec<SocketAddr> = Vec::new();

    while start.elapsed() < timeout {
        let peers = mdns.discovered_peers();
        debug!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            candidates = peers.len(),
            "join: polling mDNS for peers"
        );

        for peer in peers {
            if peer.name != mesh_name {
                continue;
            }
            if attempted.contains(&peer.address) {
                continue;
            }
            attempted.push(peer.address);

            info!(
                peer_name = %peer.name,
                peer_addr = %peer.address,
                "handshake_sent: POST /internal/join"
            );

            let url = format!("http://{}/internal/join", peer.address);
            let response = match http.post(&url).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(peer_addr = %peer.address, error = %e, "handshake: POST failed");
                    continue;
                }
            };

            let status = response.status();
            if status.is_success() {
                let parsed: JoinResponseWire = response
                    .json()
                    .await
                    .map_err(|e| JoinError::BadResponse {
                        address: peer.address,
                        reason: e.to_string(),
                    })?;
                info!(
                    peer_addr = %peer.address,
                    assigned_node_id = %parsed.assigned_node_id,
                    "handshake_accepted: joined mesh"
                );
                return Ok(JoinHandshakeResult {
                    mesh: parsed.mesh.into_mesh(),
                    assigned_node_id: parsed.assigned_node_id,
                });
            } else if status == reqwest::StatusCode::UNAUTHORIZED {
                debug!(peer_addr = %peer.address, "handshake_rejected: key didn't match, trying next");
            } else {
                warn!(
                    peer_addr = %peer.address,
                    status = %status,
                    "handshake: unexpected response status"
                );
            }
        }

        // Sleep briefly before re-polling mDNS — don't tight-loop.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(JoinError::NoPeerFound {
        mesh_name: mesh_name.to_string(),
    })
}
