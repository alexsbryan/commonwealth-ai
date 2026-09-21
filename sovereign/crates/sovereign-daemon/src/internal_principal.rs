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
use commonwealth_core::Clock;
use commonwealth_media::MemberIdentity;
use commonwealth_transport::iroh_identity_forward::{
    is_acceptor_mark, ACCEPTOR_MARK_HEADER, MESH_HEADER_PREFIX,
};
use commonwealth_transport::mesh_proof::MESH_PROOF_HEADER;
use sovereign_serving_host::admission::Principal;

use crate::state::AppState;

/// The header the acceptor writes the verified Ed25519 key into. Full
/// lowercase hex of 32 bytes (`commonwealth_media::verified_headers`).
const PUBKEY_HEADER: &str = "x-mesh-pubkey";

/// A holder of this mesh's secret is calling — and that is ALL it says.
///
/// A mesh proof proves the GROUP: any holder of the secret can mint one
/// naming any sender (`Mesh::proof_for`). So this is a marker attached BESIDE
/// the principal, never a [`Principal`] arm and never a carrier of the sender
/// the proof named — a valid proof leaves the principal exactly what it would
/// have been without one. Reading the sender as an identity would be member B
/// naming C, the same forgery the `x-node-id` work closed, reopened on
/// plaintext meshes.
///
/// It exists for one decider: the internal port's gate can tell a member of
/// the group from a stranger on a plain-IP hop, where nothing proves WHICH
/// member is calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvedMeshMember;

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
    /// Whether `raw` is a `<sender-hex>.<proof>` this mesh's secret accepts.
    ///
    /// The sender is read back out of the value because
    /// [`Mesh::verify_mesh_proof`](commonwealth_core::mesh::Mesh::verify_mesh_proof)
    /// is keyed to it — not as an identity claim. Nothing here reads
    /// `x-node-id`, and nothing here builds a principal.
    async fn mesh_proof_holds(&self, raw: &str) -> bool {
        let Some((sender_hex, proof)) = raw.split_once('.') else {
            return false;
        };
        let Some(sender) = NodeId::from_hex(sender_hex) else {
            return false;
        };
        let now = self.clock().now_unix_secs();
        let mesh = self.inner.fabric.mesh.read().await;
        mesh.verify_mesh_proof(proof, sender, now)
    }

    pub async fn resolve_internal(
        &self,
        headers: &mut HeaderMap,
        peer: Option<SocketAddr>,
    ) -> (Principal, Option<ProvedMeshMember>) {
        if !tied_to_our_acceptor(headers, peer) {
            // BEFORE the strip, because the proof's name sits under
            // `MESH_HEADER_PREFIX` and `strip_mesh_headers` would take it —
            // and it must, so that no handler downstream can re-read a proof
            // this function judged. Taken off the request here either way,
            // and NOT counted as a claim: it claims membership of the group,
            // which this daemon can check for itself, rather than an identity
            // it would have to be talked into believing.
            let offered = headers.remove(MESH_PROOF_HEADER);
            let proved = match &offered {
                None => None,
                Some(raw) => {
                    let holds = match raw.to_str() {
                        Ok(raw) => self.mesh_proof_holds(raw).await,
                        Err(_) => false,
                    };
                    if holds {
                        tracing::debug!(
                            target: "transport",
                            peer = ?peer,
                            "internal: an untied caller proved it holds this mesh's \
                             secret — a member of the GROUP, not a named member: \
                             any holder can mint a proof naming any sender, so the \
                             principal is unchanged"
                        );
                        Some(ProvedMeshMember)
                    } else {
                        tracing::debug!(
                            target: "transport",
                            peer = ?peer,
                            "internal: an untied caller offered a mesh proof this \
                             daemon's secret does not accept — no marker, and the \
                             caller is unverified"
                        );
                        None
                    }
                }
            };
            // An offered-and-failed proof is a claim that failed, which is
            // `Unverified` however little else was presented (ARCH principle
            // 6). An accepted one leaves the answer exactly where it would
            // have been with no proof at all.
            if offered.is_some() && proved.is_none() {
                strip_mesh_headers(headers);
                return (Principal::Unverified, None);
            }
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
            return (
                if claimed {
                    Principal::Unverified
                } else {
                    Principal::Anonymous
                },
                proved,
            );
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
            return (Principal::Unverified, None);
        };
        // No marker on a tied hop, and no proof read: the acceptor strips
        // every client-supplied `x-mesh-*` before forwarding, so a proof
        // cannot arrive here — and would say less than the key already did.
        match self.member_by_pubkey(dialer).await {
            Some(who) => {
                tracing::debug!(
                    target: "transport",
                    member = %who.name,
                    node = %who.node_id,
                    "internal: request resolved to a verified member"
                );
                (
                    Principal::Member {
                        node_id: who.node_id,
                    },
                    None,
                )
            }
            None => {
                tracing::debug!(
                    target: "transport",
                    dialer = %dialer,
                    "internal: the acceptor proved this key and the roster does \
                     not name it — a joiner, not a member"
                );
                (Principal::Unverified, None)
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
    let (principal, proved) = state.resolve_internal(request.headers_mut(), peer).await;
    request
        .extensions_mut()
        .insert(crate::admission::AttachedPrincipal(principal));
    // Beside the principal, not inside it: a proof says a holder of the mesh
    // secret is calling and cannot say which one.
    if let Some(marker) = proved {
        request.extensions_mut().insert(marker);
    }
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
    use commonwealth_transport::mesh_proof::mesh_proof_stamp;
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

    /// This test mesh's gossip credential — a SET one, so a proof can be
    /// minted against it. `MESH_SECRET_UNSET` is the other case and the tests
    /// that want it say so.
    const MESH_SECRET: [u8; 32] = [5u8; 32];

    /// A daemon whose roster names one member, by the key `KEY`.
    fn state_with_member(node_id: NodeId) -> AppState {
        AppState::new(NodeId::from_u128(1), mesh_with_member(node_id, MESH_SECRET))
    }

    /// The mesh `state_with_member` is built from, so a test can mint a proof
    /// against the same secret the daemon will verify with.
    fn mesh_with_member(node_id: NodeId, secret: [u8; 32]) -> Mesh {
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
        Mesh {
            mesh_secret: secret,
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "Internal Principal Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: true,
            members,
            peers: vec![],
        }
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
        let (p, _) = state.resolve_internal(&mut h, loopback()).await;
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
        let (p, _) = state.resolve_internal(&mut h, loopback()).await;
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
        let (p, _) = state.resolve_internal(&mut h, loopback()).await;
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
        let (p, _) = state.resolve_internal(&mut h, loopback()).await;
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
        let (p, _) = state.resolve_internal(&mut h, lan()).await;
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
        let (p, _) = state.resolve_internal(&mut h, None).await;
        assert_eq!(p, Principal::Unverified);
    }

    /// A joiner: the handshake proved its key and the roster does not name it.
    /// Unverified, so `/internal/join` still has someone to serve, and no
    /// decider mistakes it for a member.
    #[tokio::test]
    async fn a_proved_key_the_roster_does_not_name_is_a_joiner_not_a_member() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = acceptor_headers(&hex::encode([9u8; 32]));
        let (p, _) = state.resolve_internal(&mut h, loopback()).await;
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
    // ── The plain-IP member, and what a mesh proof can and cannot say ──
    //
    // The two tests below are the OPENING CHECK for
    // `tg-2-plain-ip-members-prove-membership`: they pin what a plain-IP
    // member gets from this resolver with no proof in hand, which is the
    // state the rest of the row exists to change.

    /// A non-loopback caller that types only the peer header claims nothing
    /// in this module's namespace, so it is anonymous — indistinguishable
    /// here from a stranger, which is the gap.
    #[tokio::test]
    async fn a_non_loopback_caller_typing_only_the_peer_header_is_anonymous() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = headers(&[("x-node-id", &NodeId::from_u128(0xBEEF).to_hex())]);
        let (p, proved) = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(p, Principal::Anonymous);
        assert_eq!(proved, None, "no proof was offered");
    }

    /// The same caller typing anything in the acceptor's namespace claimed
    /// something this daemon cannot tie to its own acceptor: unverified.
    #[tokio::test]
    async fn a_non_loopback_caller_presenting_any_mesh_header_is_unverified() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = headers(&[("x-mesh-member", "LittleMac")]);
        let (p, proved) = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(p, Principal::Unverified);
        assert_eq!(proved, None);
    }

    /// A stamp minted under this mesh's secret: the caller holds the group's
    /// credential, so the marker is attached — and the principal is exactly
    /// what it would have been without it.
    #[tokio::test]
    async fn a_valid_proof_from_a_plain_ip_caller_carries_the_marker() {
        let id = NodeId::from_u128(0xBEEF);
        let mesh = mesh_with_member(id, MESH_SECRET);
        let state = AppState::new(NodeId::from_u128(1), mesh.clone());
        let now = state.clock().now_unix_secs();
        let stamp = mesh_proof_stamp(&mesh, id, now).expect("secret is set");
        let (name, value) = stamp.pair();
        let mut h = headers(&[(name, value)]);
        let (p, proved) = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(proved, Some(ProvedMeshMember));
        assert_eq!(
            p,
            Principal::Anonymous,
            "the proof names a group, not a caller"
        );
        assert!(
            h.get(name).is_none(),
            "the proof is stripped either way: {h:?}"
        );
    }

    /// THE forgery this arm must not reopen: any holder of the secret can
    /// mint a proof naming any sender, so a valid proof naming a member is
    /// still not that member. `mp-1` closed this on the verified plane and it
    /// must stay closed on the plaintext one.
    #[tokio::test]
    async fn a_valid_proof_naming_another_members_id_is_still_anonymous() {
        let victim = NodeId::from_u128(0xBEEF);
        let mesh = mesh_with_member(victim, MESH_SECRET);
        let state = AppState::new(NodeId::from_u128(1), mesh.clone());
        let now = state.clock().now_unix_secs();
        // Minted by SOMEBODY ELSE, naming the roster's member.
        let stamp = mesh_proof_stamp(&mesh, victim, now).unwrap();
        let (name, value) = stamp.pair();
        let mut h = headers(&[(name, value)]);
        let (p, _) = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(
            p,
            Principal::Anonymous,
            "a group proof must never resolve to a named member"
        );
        assert_eq!(member_of(&p), None);
    }

    /// A proof this daemon's secret does not accept is a claim that failed:
    /// no marker, and `Unverified` rather than the `Anonymous` a caller that
    /// offered nothing would get.
    #[tokio::test]
    async fn a_non_loopback_caller_with_a_wrong_proof_carries_no_marker_and_resolves_unverified() {
        let id = NodeId::from_u128(0xBEEF);
        let state = AppState::new(NodeId::from_u128(1), mesh_with_member(id, MESH_SECRET));
        let theirs = mesh_with_member(id, [9u8; 32]);
        let now = state.clock().now_unix_secs();
        let stamp = mesh_proof_stamp(&theirs, id, now).unwrap();
        let (name, value) = stamp.pair();
        for bad in [value, "not-a-proof", &format!("{}.", id.to_hex()), "zz.zz"] {
            let mut h = headers(&[(name, bad)]);
            let (p, proved) = state.resolve_internal(&mut h, lan()).await;
            assert_eq!(proved, None, "{bad} must not mark anyone");
            assert_eq!(p, Principal::Unverified, "{bad} is a claim that failed");
            assert!(h.get(name).is_none(), "stripped: {h:?}");
        }
    }

    /// A daemon with no gossip credential of its own verifies nothing — it
    /// refuses rather than keying every proof identically (`verify_mesh_proof`
    /// carries the same rule).
    #[tokio::test]
    async fn a_daemon_with_no_secret_accepts_no_proof() {
        let id = NodeId::from_u128(0xBEEF);
        let theirs = mesh_with_member(id, MESH_SECRET);
        let state = AppState::new(
            NodeId::from_u128(1),
            mesh_with_member(id, commonwealth_core::mesh::MESH_SECRET_UNSET),
        );
        let now = state.clock().now_unix_secs();
        let stamp = mesh_proof_stamp(&theirs, id, now).unwrap();
        let (name, value) = stamp.pair();
        let mut h = headers(&[(name, value)]);
        let (p, proved) = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(proved, None);
        assert_eq!(p, Principal::Unverified);
    }

    #[tokio::test]
    async fn a_malformed_verified_key_resolves_unverified() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        for bad in ["", "zz", &hex::encode([7u8; 16])] {
            let mut h = acceptor_headers(bad);
            let (p, _) = state.resolve_internal(&mut h, loopback()).await;
            assert_eq!(p, Principal::Unverified, "{bad} must not resolve");
        }
    }
}
