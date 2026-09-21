// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who is asking — the ONE resolver from an HTTP request to a principal on the
//! INTERNAL surface (`:9742`), the port the iroh acceptor forwards to.
//!
//! ## The hazard this exists for
//!
//! `AcceptorRoutes::forward_for` now hands an internal dial to
//! `Forward::Http`, so every request that crossed iroh arrives carrying
//! `X-Mesh-Member` / `X-Mesh-Node` / `X-Mesh-Pubkey` written by the acceptor
//! from a key the QUIC handshake proved, with any client-supplied `x-mesh-*`
//! stripped first (`commonwealth_transport::iroh_identity_forward`).
//!
//! But the acceptor hands that request to a LOCAL PORT. A caller that reaches
//! that port *without the acceptor in front* can type `x-mesh-node: <C>` and
//! be believed, forging the verified identity exactly as it forges `x-node-id`
//! today. The headers alone cannot tell the two apart — by the time a handler
//! reads them, the acceptor's word and a stranger's typing are the same bytes.
//!
//! ## The tie, and why it is one condition and not two
//!
//! A request's `x-mesh-*` is an identity **only on a connection this daemon
//! can tie to its own acceptor**. The tie is:
//!
//! 1. the peer address is loopback — the acceptor always dials
//!    `127.0.0.1:<internal_port>` (`iroh_access.rs`'s `internal_addr`), so a
//!    non-loopback connection is not it; and
//! 2. the request carries this process's
//!    [`acceptor_mark`](commonwealth_transport::iroh_identity_forward::acceptor_mark),
//!    a 32-byte per-process secret the acceptor stamps on the internal arm
//!    alone, checked in constant time and stripped here before any handler
//!    sees it — never logged, never persisted, never handed to a foreign
//!    origin.
//!
//! The posture the internal listener binds under is deliberately NOT a second
//! branch. `internal_bind_addr` (`daemon.rs`) already binds loopback-only
//! whenever the mesh requires encryption or the node is local-only, which
//! closes the LAN half of the hazard on its own, and on a plaintext mesh it
//! does not. Making the tie depend on which posture is live would give one
//! question two answers (ARCH principle 8) and would still believe any LOCAL
//! process on the encrypted posture. The mark holds under both postures and
//! under neither assumption, so it is the whole tie. The loopback-only bind
//! remains defence in depth, not the decision.
//!
//! ## An untied connection resolves to a VALUE
//!
//! [`Principal::Unverified`], never `None` and never a local caller — when it
//! CLAIMED something. "I was asked to believe something and I cannot" is a
//! different answer from "nothing was presented" (ARCH principle 6), and it is
//! the answer a decider needs in order to refuse. An untied caller that
//! presented no `x-mesh-*` at all claimed nothing, so it is
//! [`Principal::Anonymous`] — the same principle read the other way, and what
//! keeps this port's perimeter-trusted local callers from being refused for a
//! claim they never made. Either way the `x-mesh-*` headers are stripped on
//! the way through, so no handler downstream can reach the claim this module
//! declined.
//!
//! ## The member comes from the KEY, not from `X-Mesh-Node`
//!
//! `X-Mesh-Node` carries `NodeId`'s `Display` form — `node-<16 hex>`, half the
//! id — because that header is written for a media origin to show a human. It
//! is not invertible, so it is not what a principal is built from. The
//! acceptor's `X-Mesh-Pubkey` is the full verified Ed25519 key, and
//! [`AppState::member_by_pubkey`] is the one roster read that turns it into a
//! member — the same read the acceptor's own `MemberCheck` does, called here
//! rather than re-spelled.
//!
//! A tied request whose key the roster does not name is [`Principal::Unverified`]
//! too: a joiner is not a member yet, and `/internal/join` is how it becomes
//! one. That is a reported absence, not a refusal — refusing is the route's
//! call.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_media::MemberIdentity;
use commonwealth_transport::iroh_identity_forward::{
    is_acceptor_mark, ACCEPTOR_MARK_HEADER, MESH_HEADER_PREFIX,
};
use sovereign_serving_host::admission::Principal;

use crate::state::AppState;

/// The header the acceptor writes the verified Ed25519 key into. Full
/// lowercase hex of 32 bytes (`commonwealth_media::verified_headers`).
const PUBKEY_HEADER: &str = "x-mesh-pubkey";

/// Whether this connection is one this daemon's own acceptor made — see the
/// module docs for why both conditions, and why there is no third.
///
/// Takes the headers rather than the whole request so the decision is a pure
/// function of what was presented, testable without a listener.
fn tied_to_our_acceptor(headers: &HeaderMap, peer: Option<SocketAddr>) -> bool {
    // A missing `ConnectInfo` is treated as NOT loopback — the stricter
    // reading, and the same one `client_auth` fails closed on.
    if !peer.is_some_and(|p| p.ip().is_loopback()) {
        return false;
    }
    headers
        .get(ACCEPTOR_MARK_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_acceptor_mark)
}

/// Remove every header in the acceptor's namespace, including the mark.
///
/// Returns how many were dropped, so the caller can say so at debug: a
/// non-zero count on an untied connection is a forgery attempt or a
/// misconfigured caller, and either is worth seeing.
fn strip_mesh_headers(headers: &mut HeaderMap) -> usize {
    let doomed: Vec<_> = headers
        .keys()
        .filter(|name| name.as_str().starts_with(MESH_HEADER_PREFIX))
        .cloned()
        .collect();
    for name in &doomed {
        headers.remove(name);
    }
    doomed.len()
}

impl AppState {
    /// The one roster read from a verified key to the member it names.
    ///
    /// Reads the LIVE mesh rather than a snapshot — a node that left loses its
    /// identity with its membership, one that just joined gains it without a
    /// restart — and excludes `removed_at` tombstones. The acceptor's
    /// `MemberCheck` is this function; it is not a second copy of it.
    pub async fn member_by_pubkey(&self, dialer: NodePubkey) -> Option<MemberIdentity> {
        let mesh = self.inner.fabric.mesh.read().await;
        mesh.members
            .values()
            .find(|m| m.removed_at.is_none() && m.node_pubkey == Some(dialer))
            .map(|m| MemberIdentity {
                name: m.name.clone(),
                node_id: m.node_id,
            })
    }

    /// The same one membership read, the other way round: the verified key a
    /// member signs with, or `None` when membership does not name one.
    ///
    /// Its caller is the ring-sync route, which holds a
    /// [`Principal::Member`]'s `NodeId` and has to ask a ring's roster about
    /// it — and a roster's actor is a key, never a node id. Deliberately the
    /// inverse of [`Self::member_by_pubkey`] and not a second source of truth:
    /// same live mesh, same tombstone exclusion, so the two cannot disagree
    /// about who is a member.
    ///
    /// `None` is a reported absence — a member on a pre-identity build — and
    /// `ring_roster::roster_names` reads it as "not on the roster", which is
    /// the same answer a derived roster gives such a member.
    pub async fn member_pubkey(&self, node: NodeId) -> Option<NodePubkey> {
        let mesh = self.inner.fabric.mesh.read().await;
        mesh.members
            .get(&node)
            .filter(|m| m.removed_at.is_none())
            .and_then(|m| m.node_pubkey)
    }

    /// THE internal resolver: what the acceptor said + where the connection
    /// came from → the principal this request is charged to, with the
    /// acceptor's namespace stripped from `headers` either way.
    ///
    /// Mutates `headers` deliberately. A handler downstream must not be able
    /// to re-read a claim this function declined, and the only way to promise
    /// that is to take it off the request (ARCH principle 10 — structural, not
    /// remembered). On a tied connection the identity triple is left in place
    /// for the log fields that already read it; only the mark is removed.
    pub async fn resolve_internal(
        &self,
        headers: &mut HeaderMap,
        peer: Option<SocketAddr>,
    ) -> Principal {
        if !tied_to_our_acceptor(headers, peer) {
            let stripped = strip_mesh_headers(headers);
            // An untied caller that presented NOTHING claimed nothing, and
            // `Anonymous` is what "nothing was presented" means. Only a caller
            // that DID present an identity this daemon cannot tie to its own
            // acceptor is `Unverified` — that is the arm a decider refuses, so
            // widening it to every local process would refuse the daemon's own
            // perimeter-trusted callers for a claim they never made (ARCH
            // principle 6, both directions).
            let claimed = stripped > 0;
            tracing::debug!(
                target: "transport",
                peer = ?peer,
                stripped,
                claimed,
                "internal: connection not tied to this daemon's acceptor — \
                 any x-mesh-* was typed by the caller, not proved by a handshake, \
                 so it is stripped and the caller is unverified"
            );
            return if claimed {
                Principal::Unverified
            } else {
                Principal::Anonymous
            };
        }
        headers.remove(ACCEPTOR_MARK_HEADER);

        let dialer = headers
            .get(PUBKEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_pubkey);
        let Some(dialer) = dialer else {
            // The acceptor always writes the key it proved, so this is a
            // forward that did not come from `forward_for`'s internal arm.
            // Reported, never assumed harmless.
            tracing::warn!(
                target: "transport",
                "internal: the hop carries this daemon's acceptor mark but no \
                 readable verified key — resolving unverified"
            );
            return Principal::Unverified;
        };
        match self.member_by_pubkey(dialer).await {
            Some(who) => {
                tracing::debug!(
                    target: "transport",
                    member = %who.name,
                    node = %who.node_id,
                    "internal: request resolved to a verified member"
                );
                Principal::Member {
                    node_id: who.node_id,
                }
            }
            None => {
                tracing::debug!(
                    target: "transport",
                    dialer = %dialer,
                    "internal: the acceptor proved this key and the roster does \
                     not name it — a joiner, not a member"
                );
                Principal::Unverified
            }
        }
    }
}

/// Full-hex `NodePubkey`, or `None` on anything else. Exactly 32 bytes; a
/// short or malformed value is not narrowed to a prefix.
fn parse_pubkey(raw: &str) -> Option<NodePubkey> {
    let bytes: [u8; 32] = hex::decode(raw.trim()).ok()?.try_into().ok()?;
    Some(NodePubkey(bytes))
}

/// `from_fn_with_state`-compatible layer for the internal router. Apply as its
/// OUTERMOST layer: it must run before any handler can read `x-mesh-*`.
///
/// It resolves once and attaches the value as
/// [`AttachedPrincipal`](crate::admission::AttachedPrincipal), the same
/// extension `client_auth_layer` attaches on the client surface, so a
/// downstream reader has one place to look on either port.
///
/// It refuses nothing. Which routes an [`Principal::Unverified`] caller may
/// reach is each route's own question; this layer's whole job is to make sure
/// the question can be asked truthfully.
pub async fn internal_principal_layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let mut request = request;
    let principal = state.resolve_internal(request.headers_mut(), peer).await;
    request
        .extensions_mut()
        .insert(crate::admission::AttachedPrincipal(principal));
    next.run(request).await
}

/// Test-only: the node id a member principal carries, or `None`.
#[cfg(test)]
fn member_of(p: &Principal) -> Option<NodeId> {
    p.node_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
    use commonwealth_core::ids::MeshId;
    use commonwealth_core::mesh::Mesh;
    use std::collections::HashMap;

    const KEY: [u8; 32] = [7u8; 32];

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    fn loopback() -> Option<SocketAddr> {
        Some("127.0.0.1:51000".parse().unwrap())
    }

    fn lan() -> Option<SocketAddr> {
        Some("192.168.1.13:51000".parse().unwrap())
    }

    /// A daemon whose roster names one member, by the key `KEY`.
    fn state_with_member(node_id: NodeId) -> AppState {
        let mut members = HashMap::new();
        members.insert(
            node_id,
            commonwealth_core::mesh::MemberRecord {
                node_id,
                name: "LittleMac".into(),
                invited_by: node_id,
                joined_at: 0,
                last_seen: 0,
                status: commonwealth_core::mesh::NodeStatus::Online,
                capabilities: NodeCapabilities {
                    hardware: HardwareProfile {
                        gpus: vec![],
                        system_ram_gb: 0,
                        cpu_cores: 0,
                        total_storage_gb: 0,
                        free_storage_gb: 0,
                        network_bandwidth_mbps: None,
                    },
                    available: AvailableResources::default(),
                    active_processes: vec![],
                    hosted_corpora: vec![],
                    reported_at: 0,
                    inference_availability: 1.0,
                    inference_capable: false,
                    loaded_models: vec![],
                    origins: Vec::new(),
                    media_allow: Vec::new(),
                    media_available: None,
                    embed_model: None,
                    benchmark: None,
                    current_in_flight: None,
                    anchor: None,
                },
                addresses: vec![],
                node_pubkey: Some(NodePubkey(KEY)),
                relay_url: None,
                iroh_direct_addrs: vec![],
                dial_info_version: 0,
                dial_info_sig: None,
                removed_at: None,
            },
        );
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "Internal Principal Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: true,
            members,
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    /// What the acceptor's internal arm actually puts on the wire.
    fn acceptor_headers(pubkey: &str) -> HeaderMap {
        headers(&[
            ("x-mesh-member", "LittleMac"),
            ("x-mesh-node", "node-0000000000000000"),
            ("x-mesh-pubkey", pubkey),
            (ACCEPTOR_MARK_HEADER, acceptor_mark_for_test()),
        ])
    }

    fn acceptor_mark_for_test() -> &'static str {
        commonwealth_transport::iroh_identity_forward::acceptor_mark()
    }

    /// The whole point: the acceptor's own hop is believed.
    #[tokio::test]
    async fn the_acceptors_own_hop_resolves_to_the_member_the_roster_names() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        let mut h = acceptor_headers(&hex::encode(KEY));
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(member_of(&p), Some(id), "got {p:?}");
    }

    /// THE failing input this module exists for. A caller that reaches the
    /// internal port without the acceptor in front types the whole identity
    /// triple — and is not believed, nor is the claim left on the request for
    /// a handler to find.
    #[tokio::test]
    async fn a_forged_identity_from_a_direct_connection_is_stripped_and_unverified() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        // Everything the acceptor would send, minus the one thing it cannot
        // forge — and from loopback, which a local process gets for free.
        let mut h = headers(&[
            ("x-mesh-member", "LittleMac"),
            ("x-mesh-node", "node-0000000000000000"),
            ("x-mesh-pubkey", &hex::encode(KEY)),
        ]);
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(p, Principal::Unverified, "a typed key is not a proved one");
        assert!(
            h.get("x-mesh-member").is_none()
                && h.get("x-mesh-node").is_none()
                && h.get("x-mesh-pubkey").is_none(),
            "the declined claim must not survive for a handler to read: {h:?}"
        );
    }

    /// An untied caller that claimed NOTHING is anonymous, not unverified.
    /// The peer gate refuses `Unverified`, so widening that arm to every
    /// local process would close this perimeter-trusted port to the daemon's
    /// own callers for a claim they never made.
    #[tokio::test]
    async fn an_untied_caller_that_presented_nothing_is_anonymous() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = HeaderMap::new();
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(p, Principal::Anonymous, "nothing was presented");
    }

    /// A wrong mark is no mark. This is the branch a leaked-then-rotated
    /// secret lands on, and the one a guessing caller lands on.
    #[tokio::test]
    async fn a_wrong_acceptor_mark_is_not_a_tie() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        let mut h = acceptor_headers(&hex::encode(KEY));
        h.insert(ACCEPTOR_MARK_HEADER, "00".repeat(32).parse().unwrap());
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(p, Principal::Unverified);
    }

    /// The plaintext-mesh half: the internal listener binds `0.0.0.0`, so a
    /// LAN caller can reach it. Holding the mark would not save it either —
    /// but it does not have the mark, and it is not loopback, and EITHER
    /// alone is enough to decline.
    #[tokio::test]
    async fn a_non_loopback_caller_is_never_tied_even_holding_the_mark() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        let mut h = acceptor_headers(&hex::encode(KEY));
        let p = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(p, Principal::Unverified);
        assert!(h.get("x-mesh-pubkey").is_none(), "stripped: {h:?}");
    }

    /// A listener that forgot `into_make_service_with_connect_info` cannot
    /// identify anyone, so it identifies nobody — the stricter reading, the
    /// same one `client_auth` fails closed on.
    #[tokio::test]
    async fn a_missing_connect_info_is_not_loopback() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        let mut h = acceptor_headers(&hex::encode(KEY));
        let p = state.resolve_internal(&mut h, None).await;
        assert_eq!(p, Principal::Unverified);
    }

    /// A joiner: the handshake proved its key and the roster does not name it.
    /// Unverified, so `/internal/join` still has someone to serve, and no
    /// decider mistakes it for a member.
    #[tokio::test]
    async fn a_proved_key_the_roster_does_not_name_is_a_joiner_not_a_member() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = acceptor_headers(&hex::encode([9u8; 32]));
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(p, Principal::Unverified);
    }

    /// The mark never reaches a handler — a route that forwarded a request
    /// onward would otherwise hand this process's secret to whoever it called.
    #[tokio::test]
    async fn the_acceptor_mark_is_stripped_on_a_tied_hop_too() {
        let id = NodeId::from_u128(0xBEEF);
        let state = state_with_member(id);
        let mut h = acceptor_headers(&hex::encode(KEY));
        let _ = state.resolve_internal(&mut h, loopback()).await;
        assert!(h.get(ACCEPTOR_MARK_HEADER).is_none(), "{h:?}");
        assert!(
            h.get("x-mesh-member").is_some(),
            "the identity triple stays for the readers that already log it"
        );
    }

    /// A truncated or malformed key is not narrowed to a prefix match.
    #[tokio::test]
    async fn a_malformed_verified_key_resolves_unverified() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        for bad in ["", "zz", &hex::encode([7u8; 16])] {
            let mut h = acceptor_headers(bad);
            let p = state.resolve_internal(&mut h, loopback()).await;
            assert_eq!(p, Principal::Unverified, "{bad} must not resolve");
        }
    }
}
