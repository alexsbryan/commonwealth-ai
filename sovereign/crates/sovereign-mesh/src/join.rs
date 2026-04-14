//! Mesh join handshake — two discovery paths.
//!
//! Replaces the previous placeholder in `EmbeddedDaemon::join_mesh`
//! that just created a local empty mesh and hoped for the best.
//!
//! **Path 1 — same LAN via mDNS** (default):
//!
//! 1. Wait for `MdnsDiscovery` to surface candidate peers — up to
//!    `timeout` — filtered by `mesh_name` (the only discriminator the
//!    joiner has; mesh_id is only known to the founder side).
//! 2. For each candidate, POST `/internal/join` to the founder's
//!    internal API (port 9742) with the raw `join_key`.
//! 3. First `200` wins: deserialise the returned authoritative
//!    `Mesh` snapshot and return it.
//! 4. `401` → wrong mesh (hash mismatch) → try the next candidate.
//! 5. Timeout with no accepting peer → `Error::NoPeerFound`.
//!
//! **Path 2 — direct peer address** (for overlay networks like
//! Tailscale / Headscale that don't forward mDNS multicast):
//!
//! When the join URL carries `?relay=<host[:port]>`, we try that
//! address *before* entering the mDNS loop. A bare hostname gets
//! `:9742` appended. Success ends the handshake; failure (network
//! error or 401) falls through to the mDNS loop so the direct hint
//! remains purely additive — a LAN-only peer still works.
//!
//! Plain HTTP throughout — see the security note in
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

/// Normalise a URL-provided peer hint to a `host:port` string that
/// can be stuck straight into an `http://{…}/internal/join` URL.
/// Bare hosts (IP or hostname) get the default internal port `9742`
/// appended. Returns `None` if the hint is empty.
fn normalise_peer_hint(hint: &str) -> Option<String> {
    let s = hint.trim();
    if s.is_empty() {
        return None;
    }
    // Tolerate bracketed IPv6 (`[::1]:9742`) — if it starts with `[`
    // and has `]`, assume fully-qualified. Otherwise detect a port
    // by the last `:` with an all-digits suffix. Bare hostnames and
    // IPv4 without a port get `:9742` appended.
    let has_port = if s.starts_with('[') {
        s.contains("]:")
    } else {
        s.rsplit_once(':')
            .map(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    };
    Some(if has_port {
        s.to_string()
    } else {
        format!("{s}:9742")
    })
}

/// Try a single `/internal/join` POST against `authority` (a
/// `host:port` string). Returns the parsed response on 200, `None`
/// on 401 / network errors / non-success — the caller decides
/// whether to fall back to other candidates.
async fn try_single_peer(
    http: &reqwest::Client,
    authority: &str,
    body: &JoinRequestWire,
) -> Option<JoinResponseWire> {
    let url = format!("http://{authority}/internal/join");
    let response = match http.post(&url).json(body).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(peer = %authority, error = %e, "handshake: POST failed");
            return None;
        }
    };
    let status = response.status();
    if status.is_success() {
        match response.json::<JoinResponseWire>().await {
            Ok(parsed) => {
                info!(
                    peer = %authority,
                    assigned_node_id = %parsed.assigned_node_id,
                    "handshake_accepted: joined mesh"
                );
                Some(parsed)
            }
            Err(e) => {
                warn!(peer = %authority, error = %e, "handshake: bad response body");
                None
            }
        }
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
        debug!(peer = %authority, "handshake_rejected: key didn't match");
        None
    } else {
        warn!(peer = %authority, %status, "handshake: unexpected status");
        None
    }
}

/// Execute the join handshake. First tries `direct_peer_hint` if
/// supplied (useful on overlay networks like Tailscale where mDNS
/// doesn't propagate), then polls `mdns` for peers advertising
/// `mesh_name`. First accepting peer wins. Times out after
/// `timeout` with `Error::NoPeerFound` if nothing accepts.
pub async fn perform_join(
    mesh_name: &str,
    join_key: &str,
    joining_node_name: &str,
    joining_node_addresses: Vec<SocketAddr>,
    direct_peer_hint: Option<&str>,
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

    // Direct peer hint — tried first so overlay-network (Tailscale /
    // Headscale / VPN) users don't wait for an mDNS loop that will
    // never find anything. On success we're done; on failure we fall
    // through to the mDNS loop so the hint remains purely additive.
    if let Some(raw) = direct_peer_hint {
        if let Some(authority) = normalise_peer_hint(raw) {
            info!(peer = %authority, "handshake_sent: direct-peer hint, POST /internal/join");
            if let Some(parsed) = try_single_peer(&http, &authority, &body).await {
                return Ok(JoinHandshakeResult {
                    mesh: parsed.mesh.into_mesh(),
                    assigned_node_id: parsed.assigned_node_id,
                });
            }
            debug!(
                peer = %authority,
                "direct-peer hint did not accept — falling back to mDNS"
            );
        }
    }

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
            let authority = peer.address.to_string();
            if let Some(parsed) = try_single_peer(&http, &authority, &body).await {
                return Ok(JoinHandshakeResult {
                    mesh: parsed.mesh.into_mesh(),
                    assigned_node_id: parsed.assigned_node_id,
                });
            }
        }

        // Sleep briefly before re-polling mDNS — don't tight-loop.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(JoinError::NoPeerFound {
        mesh_name: mesh_name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_appends_default_port_for_bare_host() {
        assert_eq!(
            normalise_peer_hint("100.64.0.5").as_deref(),
            Some("100.64.0.5:9742")
        );
        assert_eq!(
            normalise_peer_hint("my-machine.tailnet.ts.net").as_deref(),
            Some("my-machine.tailnet.ts.net:9742")
        );
    }

    #[test]
    fn normalise_preserves_explicit_port() {
        assert_eq!(
            normalise_peer_hint("100.64.0.5:4242").as_deref(),
            Some("100.64.0.5:4242")
        );
    }

    #[test]
    fn normalise_handles_ipv6_bracketed_form() {
        assert_eq!(
            normalise_peer_hint("[fd00::1]:9742").as_deref(),
            Some("[fd00::1]:9742")
        );
        // Bare unbracketed IPv6 is ambiguous (colons in the address
        // look like a port); treat as "already has a port" rather
        // than mangle it. If the user gets this wrong, reqwest will
        // refuse and we fall through to mDNS.
        assert!(normalise_peer_hint("fd00::1").is_some());
    }

    #[test]
    fn normalise_rejects_empty() {
        assert!(normalise_peer_hint("").is_none());
        assert!(normalise_peer_hint("   ").is_none());
    }
}
