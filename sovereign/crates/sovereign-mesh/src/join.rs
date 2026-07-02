// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Stable NodeId the joiner has persisted at
    /// `<data_dir>/node_id`. The founder honours this as the
    /// member's `node_id` in the mesh if it's either (a) not
    /// already taken, or (b) already present under the same name
    /// (rejoin path). Absent on pre-stable-identity builds; the
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// on the founder side keeps the wire format backward-compatible.
    #[serde(skip_serializing_if = "Option::is_none")]
    proposed_node_id: Option<NodeId>,
    /// This node's Ed25519 identity pubkey (see
    /// `commonwealth_core::ids::NodePubkey`). Pre-identity founders
    /// ignore it (serde default on their side); identity-aware
    /// founders record it after verifying `pubkey_proof`.
    #[serde(skip_serializing_if = "Option::is_none")]
    node_pubkey: Option<commonwealth_core::ids::NodePubkey>,
    /// Hex Ed25519 proof of possession over
    /// `"cwth-join-pubkey-binding:" || proposed_node_id || name`.
    /// Always sent together with `node_pubkey`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pubkey_proof: Option<String>,
}

/// Members transit as a flat Vec because `HashMap<NodeId, _>` doesn't
/// round-trip through JSON (NodeId is an array, not a string key).
/// See `commonwealth_api::routes_internal::MeshWire`.
#[derive(Debug, Deserialize)]
struct MeshWire {
    id: commonwealth_core::ids::MeshId,
    name: String,
    join_key_hash: [u8; 32],
    #[serde(default)]
    require_encryption: bool,
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
            require_encryption: self.require_encryption,
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
#[derive(Debug)]
pub struct JoinHandshakeResult {
    pub mesh: Mesh,
    pub assigned_node_id: NodeId,
}

#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// No peer on the LAN accepted the join (either none advertised
    /// the expected mesh_name in `timeout`, or every one rejected
    /// the join key as invalid).
    #[error(
        "no peer on this network accepted the join key for mesh '{mesh_name}'{direct_hint_msg}"
    )]
    NoPeerFound {
        mesh_name: String,
        /// Appended to the error when a direct-peer hint was
        /// provided AND failed — the user needs to see the precise
        /// TCP-level reason (especially "No route to host", which
        /// is the diagnostic signature of WiFi AP isolation).
        direct_hint_msg: String,
    },

    /// An accepting peer returned a malformed response. Rare; usually
    /// a version mismatch between the founder and joiner binaries.
    #[error("peer at {address} returned a malformed response: {reason}")]
    BadResponse { address: SocketAddr, reason: String },
}

/// Flatten an error's `source()` chain into a Vec of messages —
/// reqwest's top-level `Display` drops the underlying hyper/io
/// cause ("No route to host", "Connection refused") that actually
/// pinpoints the failure.
fn collect_error_chain(e: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = current {
        chain.push(err.to_string());
        current = err.source();
    }
    chain
}

/// Classify a reqwest error into a coarse failure mode so the log
/// line at the call site can render it at a glance without parsing
/// the cause chain. Worth the dozen lines because "connect vs
/// timeout vs body vs tls" drives very different user fixes.
fn classify_reqwest_error(e: &reqwest::Error) -> &'static str {
    if e.is_connect() {
        "connect_refused_or_unreachable"
    } else if e.is_timeout() {
        "timeout"
    } else if e.is_request() {
        "request_builder"
    } else if e.is_body() {
        "body_stream"
    } else if e.is_decode() {
        "decode"
    } else {
        "other"
    }
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
            let causes = collect_error_chain(&e);
            warn!(
                peer = %authority,
                kind = ?classify_reqwest_error(&e),
                causes = ?causes,
                "handshake: POST failed"
            );
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

/// ENCRYPTED join: dial the founder by key over iroh and tunnel
/// `/internal/join` through the key-verified QUIC bridge, so the join
/// secret never crosses the wire in plaintext and the joiner
/// cryptographically verifies it reached the real founder. **Fail
/// closed** — there is NO mDNS / plaintext fallback; an encrypted mesh
/// refuses to join over plaintext.
///
/// `founder_dial` is the `<hex-pubkey>@<relay-or-addr>[,…]` string the
/// founder embedded in the invite (`format_dial_string`). `joiner_seed`
/// is this node's `node_key` seed — a valid iroh `SecretKey` — used to
/// build the one-shot dialing endpoint (dropped with the bridge on
/// return).
///
/// NOTE: the on-wire QUIC handshake is validated on two real machines
/// (multi-box-only); the in-process tests cover the surrounding logic
/// (dial-string parse, fail-closed on unreachable founder).
#[allow(clippy::too_many_arguments)]
pub async fn perform_encrypted_join(
    founder_dial: &str,
    join_key: &str,
    joining_node_name: &str,
    joining_node_addresses: Vec<SocketAddr>,
    joiner_seed: [u8; 32],
    relay_cfg: &commonwealth_transport::iroh::RelayConfig,
    proposed_node_id: Option<NodeId>,
    identity: Option<(commonwealth_core::ids::NodePubkey, String)>,
) -> Result<JoinHandshakeResult, JoinError> {
    let dummy_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

    let (node_pubkey, pubkey_proof) = match identity {
        Some((pk, proof)) => (Some(pk), Some(proof)),
        None => (None, None),
    };
    let body = JoinRequestWire {
        join_key: join_key.to_string(),
        joining_node_name: joining_node_name.to_string(),
        joining_node_addresses,
        proposed_node_id,
        node_pubkey,
        pubkey_proof,
    };

    info!(
        founder = %founder_dial,
        "handshake_sent: encrypted join over iroh, POST /internal/join"
    );
    match iroh_tunnel_handshake(founder_dial, joiner_seed, relay_cfg, &body).await {
        Ok(parsed) => Ok(JoinHandshakeResult {
            mesh: parsed.mesh.into_mesh(),
            assigned_node_id: parsed.assigned_node_id,
        }),
        Err(TunnelFailure::Setup(reason)) => Err(JoinError::BadResponse {
            address: dummy_addr,
            reason,
        }),
        Err(TunnelFailure::NotAccepted) => Err(JoinError::NoPeerFound {
            mesh_name: "(encrypted mesh)".to_string(),
            direct_hint_msg: " — the founder did not accept the encrypted join (expired invite, \
                 wrong key, or unreachable over iroh)"
                .to_string(),
        }),
    }
}

/// Why an iroh tunnel join attempt failed — the two callers differ
/// only in what they do with each case (encrypted: fail closed;
/// plaintext prefer-iroh: log and fall back to IP/mDNS).
enum TunnelFailure {
    /// Malformed dial string / endpoint bind / bridge setup — failed
    /// before any founder contact.
    Setup(String),
    /// The tunnel came up but the founder didn't accept the join
    /// (rejected key, expired invite, or unreachable over iroh).
    NotAccepted,
}

/// Dial `founder_dial` by key over a one-shot iroh endpoint built from
/// `joiner_seed` (this node's `node_key` seed IS a valid iroh
/// `SecretKey`) and POST `/internal/join` through a localhost
/// [`HttpBridge`]. The QUIC handshake to the founder IS the key
/// verification — it fails unless the responder holds the private key
/// for the pubkey embedded in the dial string. The endpoint and bridge
/// live until this returns; the POST rides the tunnel they own.
async fn iroh_tunnel_handshake(
    founder_dial: &str,
    joiner_seed: [u8; 32],
    relay_cfg: &commonwealth_transport::iroh::RelayConfig,
    body: &JoinRequestWire,
) -> Result<JoinResponseWire, TunnelFailure> {
    use commonwealth_transport::iroh::{
        build_relayed_endpoint, parse_dial_string, HttpBridge, SecretKey, ALPN,
    };

    let target = parse_dial_string(founder_dial).map_err(|e| {
        TunnelFailure::Setup(format!("invite has a malformed iroh dial string: {e}"))
    })?;

    let secret = SecretKey::from_bytes(&joiner_seed);
    let endpoint = build_relayed_endpoint(secret, vec![ALPN.to_vec()], relay_cfg)
        .await
        .map_err(|e| {
            TunnelFailure::Setup(format!("failed to build iroh endpoint for join: {e}"))
        })?;

    let bridge = HttpBridge::spawn(endpoint, target, ALPN)
        .await
        .map_err(|e| TunnelFailure::Setup(format!("failed to open iroh tunnel to founder: {e}")))?;
    let authority = bridge.local_addr().to_string();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client build");

    match try_single_peer(&http, &authority, body).await {
        Some(parsed) => Ok(parsed),
        None => Err(TunnelFailure::NotAccepted),
    }
}

/// Execute the plaintext-mesh join handshake, trying each discovery
/// path in order — every step is additive, any failure falls through
/// to the next (unlike the encrypted join, which is fail-closed):
///
/// 1. **`iroh`** — `(founder_dial, joiner_seed)` from a `dial=` invite:
///    dial the founder by key over a one-shot iroh tunnel (W2c, the
///    no-VPN path; works across networks with no shared IP route).
/// 2. **`direct_peer_hint`** — a `relay=` host:port POSTed directly
///    (overlay networks / VPNs where mDNS doesn't propagate).
/// 3. **mDNS** — poll for LAN peers advertising `mesh_name`.
///
/// First accepting peer wins. Times out after `timeout` (mDNS budget)
/// with `Error::NoPeerFound` carrying the earlier paths' failure
/// reasons if nothing accepts.
#[allow(clippy::too_many_arguments)]
pub async fn perform_join(
    mesh_name: &str,
    join_key: &str,
    joining_node_name: &str,
    joining_node_addresses: Vec<SocketAddr>,
    iroh: Option<(&str, [u8; 32])>,
    // Relay/discovery posture for the iroh tunnel attempt (default =
    // n0). Only consulted when `iroh` is `Some`.
    relay_cfg: &commonwealth_transport::iroh::RelayConfig,
    direct_peer_hint: Option<&str>,
    mdns: Option<&MdnsDiscovery>,
    timeout: Duration,
    proposed_node_id: Option<NodeId>,
    // (pubkey, hex proof-of-possession) — see `JoinRequestWire`.
    identity: Option<(commonwealth_core::ids::NodePubkey, String)>,
) -> Result<JoinHandshakeResult, JoinError> {
    // 3-second per-peer HTTP timeout. With a 5s overall budget this
    // leaves one retry with a fresh mDNS candidate if the first peer
    // is flaky.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("reqwest client build");

    let (node_pubkey, pubkey_proof) = match identity {
        Some((pk, proof)) => (Some(pk), Some(proof)),
        None => (None, None),
    };
    let body = JoinRequestWire {
        join_key: join_key.to_string(),
        joining_node_name: joining_node_name.to_string(),
        joining_node_addresses,
        proposed_node_id,
        node_pubkey,
        pubkey_proof,
    };

    // Prefer-iroh (W2c): a plaintext invite carrying a `dial=` connect
    // code gets the founder dialed BY KEY first — the path that works
    // with no shared IP route at all. Bounded so the IP/mDNS fallback
    // stays responsive; any failure logs, is kept for the final error,
    // and falls through — purely additive, mirroring the direct-hint
    // block below.
    let mut tunnel_failure: Option<String> = None;
    if let Some((dial, seed)) = iroh {
        info!(
            founder = %dial,
            "handshake_sent: prefer-iroh join, POST /internal/join over tunnel"
        );
        match tokio::time::timeout(
            Duration::from_secs(10),
            iroh_tunnel_handshake(dial, seed, relay_cfg, &body),
        )
        .await
        {
            Ok(Ok(parsed)) => {
                info!(
                    assigned_node_id = %parsed.assigned_node_id,
                    "handshake_accepted: joined mesh over iroh tunnel"
                );
                return Ok(JoinHandshakeResult {
                    mesh: parsed.mesh.into_mesh(),
                    assigned_node_id: parsed.assigned_node_id,
                });
            }
            Ok(Err(TunnelFailure::Setup(reason))) => {
                warn!(%reason, "join: iroh tunnel setup failed — falling back to IP/mDNS");
                tunnel_failure = Some(reason);
            }
            Ok(Err(TunnelFailure::NotAccepted)) => {
                warn!(
                    "join: founder did not accept over the iroh tunnel — falling back to IP/mDNS"
                );
                tunnel_failure =
                    Some("the founder did not accept the join over the iroh tunnel".into());
            }
            Err(_elapsed) => {
                warn!("join: iroh tunnel attempt timed out — falling back to IP/mDNS");
                tunnel_failure = Some("the iroh tunnel attempt timed out".into());
            }
        }
    }

    // Direct peer hint — tried before mDNS so overlay-network / VPN
    // users don't wait for an mDNS loop that will never find
    // anything. On success we're done; on failure we fall through to
    // the mDNS loop so the hint remains purely additive. We also keep
    // the failure reason to attach to the final error if everything
    // fails — "No route to host" is the user-visible signal for WiFi
    // AP isolation, which they'd otherwise have to dig out of the
    // terminal logs.
    let mut direct_failure: Option<String> = None;
    if let Some(raw) = direct_peer_hint {
        if let Some(authority) = normalise_peer_hint(raw) {
            info!(peer = %authority, "handshake_sent: direct-peer hint, POST /internal/join");
            match http
                .post(format!("http://{authority}/internal/join"))
                .json(&body)
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.json::<JoinResponseWire>().await {
                            Ok(parsed) => {
                                info!(
                                    peer = %authority,
                                    assigned_node_id = %parsed.assigned_node_id,
                                    "handshake_accepted: joined mesh via direct hint"
                                );
                                return Ok(JoinHandshakeResult {
                                    mesh: parsed.mesh.into_mesh(),
                                    assigned_node_id: parsed.assigned_node_id,
                                });
                            }
                            Err(e) => {
                                direct_failure = Some(format!(
                                    "peer at {authority} returned a malformed response: {e}"
                                ));
                            }
                        }
                    } else if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                        direct_failure = Some(format!(
                            "peer at {authority} rejected the join key (401) — \
                             check the URL matches the founder's mesh"
                        ));
                    } else {
                        direct_failure = Some(format!(
                            "peer at {authority} returned unexpected status {}",
                            response.status()
                        ));
                    }
                }
                Err(e) => {
                    let causes = collect_error_chain(&e);
                    let kind = classify_reqwest_error(&e);
                    warn!(
                        peer = %authority,
                        kind,
                        causes = ?causes,
                        "handshake: direct-hint POST failed"
                    );
                    // Prefer the deepest cause message for the
                    // user-facing error — that's the one that says
                    // "No route to host" / "Connection refused" etc.
                    let deepest = causes
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "unknown reason".into());
                    direct_failure = Some(format!(
                        "couldn't reach {authority}: {deepest} \
                         (kind: {kind}). If you see \"No route to host\" \
                         on the same WiFi, your router likely has client \
                         isolation enabled — ask the founder for a current \
                         invite link (it carries a no-VPN connect code) or \
                         use a different network."
                    ));
                }
            }
            debug!(
                peer = %authority,
                "direct-peer hint did not accept — falling back to mDNS"
            );
        }
    }

    // mDNS disabled (headless / VPC host): the paths above were our
    // only discovery options. If they didn't already return, we have no
    // way to locate a peer — surface the same not-found error the
    // timeout would.
    let Some(mdns) = mdns else {
        let mut direct_hint_msg = match direct_failure {
            Some(msg) => format!(". Direct seed also failed: {msg}"),
            None => ". mDNS is disabled and no reachable seed address was provided".to_string(),
        };
        if let Some(t) = &tunnel_failure {
            direct_hint_msg.push_str(&format!(". iroh tunnel also failed: {t}"));
        }
        return Err(JoinError::NoPeerFound {
            mesh_name: mesh_name.to_string(),
            direct_hint_msg,
        });
    };

    let start = Instant::now();
    // Track attempted peer addresses so we don't spam the same node
    // when mDNS re-resolves it repeatedly.
    let mut attempted: Vec<SocketAddr> = Vec::new();
    // Logged once the first time we see non-zero discovery, so the
    // user can tell mDNS is working even if no peer matches.
    let mut seen_any_peer = false;

    info!(
        mesh_name,
        timeout_secs = timeout.as_secs(),
        "join: starting mDNS discovery loop"
    );

    while start.elapsed() < timeout {
        let peers = mdns.discovered_peers();
        if !peers.is_empty() && !seen_any_peer {
            seen_any_peer = true;
            let names: Vec<String> = peers
                .iter()
                .map(|p| {
                    format!(
                        "{}@{} (mesh={:?}, node={:?})",
                        &p.mesh_id_hex[..p.mesh_id_hex.len().min(8)],
                        p.address,
                        p.mesh_name,
                        p.name
                    )
                })
                .collect();
            info!(peers = ?names, "join: mDNS has surfaced candidates");
        }

        for peer in peers {
            // Match the peer's advertised *mesh_name* (not the node
            // name). Historically these were conflated into a single
            // TXT field, which made this filter impossible to
            // satisfy; see the comment on `MdnsDiscovery::new`.
            //
            // Empty mesh_name → peer is running an older build that
            // only broadcast `name` as the node hostname. Fall back
            // to comparing against `peer.name` so a mid-rollout LAN
            // (one upgraded, one not) still connects if the mesh
            // names happen to match.
            let peer_mesh_label = if peer.mesh_name.is_empty() {
                peer.name.as_str()
            } else {
                peer.mesh_name.as_str()
            };
            if peer_mesh_label != mesh_name {
                debug!(
                    peer_addr = %peer.address,
                    peer_mesh_name = %peer.mesh_name,
                    peer_node_name = %peer.name,
                    expected = mesh_name,
                    "join: skipping peer — mesh_name doesn't match"
                );
                continue;
            }
            if attempted.contains(&peer.address) {
                continue;
            }
            attempted.push(peer.address);

            info!(
                peer_node_name = %peer.name,
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

    // Final log so the user sees *why* we timed out. If mDNS never
    // surfaced any peer, it's a network issue (firewall, different
    // subnet, or no shared network and an invite without a no-VPN
    // connect code). If peers were seen but none matched, it's a
    // mesh_name mismatch — bad URL.
    if seen_any_peer {
        warn!(
            mesh_name,
            tried = attempted.len(),
            "join timed out: found LAN peers but none accepted the join key \
             (check mesh_name in the URL matches the founder's mesh)"
        );
    } else {
        warn!(
            mesh_name,
            "join timed out: no mDNS peers on this network — check the \
             founder's app is running, you're on the same WiFi, and the \
             OS firewall permits _commonwealth._tcp on port 9742"
        );
    }

    let mut direct_hint_msg = match direct_failure {
        Some(msg) => format!(". Direct relay also failed: {msg}"),
        None => String::new(),
    };
    if let Some(t) = &tunnel_failure {
        direct_hint_msg.push_str(&format!(". iroh tunnel also failed: {t}"));
    }

    Err(JoinError::NoPeerFound {
        mesh_name: mesh_name.to_string(),
        direct_hint_msg,
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

    #[tokio::test]
    async fn encrypted_join_rejects_malformed_dial_string() {
        // A malformed founder dial string fails fast (before any iroh
        // endpoint is built or the network is touched) with a clear
        // BadResponse — the fail-closed guard on the encrypted path.
        let result = perform_encrypted_join(
            "this-is-not-a-valid-dial-string", // no `@`, no targets
            "cwth-0000-0000-0000",
            "joiner",
            vec![],
            [0u8; 32],
            &commonwealth_transport::iroh::RelayConfig::default(),
            None,
            None,
        )
        .await;
        match result {
            Err(JoinError::BadResponse { reason, .. }) => {
                assert!(
                    reason.contains("malformed iroh dial string"),
                    "got: {reason}"
                );
            }
            other => panic!("expected BadResponse for malformed dial, got {other:?}"),
        }
    }
}
