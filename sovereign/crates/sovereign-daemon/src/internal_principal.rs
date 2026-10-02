// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who is asking — the ONE resolver from an HTTP request to a principal on the
//! INTERNAL surface (`:9742`), the port cw-rails forwards members to.
//!
//! ## The hazard this exists for
//!
//! cw-rails is the node's one mesh endpoint (pb-mesh-exit-transport). It
//! admits a member by the key its QUIC handshake proved and forwards the
//! request to svrn's registered origin on loopback, carrying `X-Mesh-Member` /
//! `X-Mesh-Node` / `X-Mesh-Pubkey` written from that key, with any
//! client-supplied `x-mesh-*` stripped first.
//!
//! But cw-rails hands that request to a LOCAL PORT. A caller that reaches the
//! port *without cw-rails in front* can type `x-mesh-pubkey: <C>` and be
//! believed, forging the verified identity. The headers alone cannot tell the
//! two apart — by the time a handler reads them, cw-rails' word and a
//! stranger's typing are the same bytes.
//!
//! ## The tie
//!
//! A request's `x-mesh-*` is an identity **only on a connection this daemon
//! can tie to its own registration** with cw-rails (`crate::peer_origin`):
//!
//! 1. the peer address is loopback — cw-rails always dials the registered
//!    port on `127.0.0.1`, so a non-loopback connection is not it; and
//! 2. the request carries the live claim's tie
//!    ([`ORIGIN_TIE_HEADER`]), the secret cw-rails was handed when svrn
//!    registered and stamps on every forward to that origin — checked here
//!    and stripped before any handler sees it.
//!
//! The internal listener binds loopback (`internal_bind_addr`, `daemon.rs`),
//! which closes the LAN half on its own; the tie is what separates cw-rails
//! from any other local process, so it is the whole decision.
//!
//! ## An untied connection resolves to a VALUE
//!
//! [`Principal::Unverified`], never `None` and never a local caller — when it
//! CLAIMED something. "I was asked to believe something and I cannot" is a
//! different answer from "nothing was presented" (ARCH principle 6), and it is
//! the answer a decider needs in order to refuse. An untied caller that
//! presented no `x-mesh-*` at all claimed nothing, so it is
//! [`Principal::Anonymous`] — what keeps this port's perimeter-trusted local
//! callers from being refused for a claim they never made. Either way the
//! `x-mesh-*` headers are stripped on the way through, so no handler
//! downstream can reach the claim this module declined.
//!
//! ## The member comes from the KEY, not from `X-Mesh-Node`
//!
//! `X-Mesh-Node` carries `NodeId`'s `Display` form — `node-<16 hex>`, half the
//! id — because that header is written for a media origin to show a human. It
//! is not invertible, so it is not what a principal is built from.
//! `X-Mesh-Pubkey` is the full verified Ed25519 key, and
//! [`AppState::member_by_pubkey`] is the one roster read that turns it into a
//! member, over cw-rails' roster through the membership port.
//!
//! A tied request whose key the roster does not name is
//! [`Principal::Unverified`] too: a reported absence, not a refusal —
//! refusing is the route's call.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use kernel_types::member::{MemberIdentity, MESH_HEADER_PREFIX, ORIGIN_TIE_HEADER};
use kernel_types::NodePubkey;
use sovereign_contracts::principal::Principal;

use crate::state::AppState;

/// The header cw-rails writes the verified Ed25519 key into. Full lowercase
/// hex of 32 bytes (`kernel_types::member::verified_headers`).
const PUBKEY_HEADER: &str = "x-mesh-pubkey";

/// Whether this connection is cw-rails forwarding to svrn's peer-route
/// registration (`crate::peer_origin`): loopback, as cw-rails always dials
/// the registered port on `127.0.0.1`, and carrying the live claim's tie.
fn tied_to_our_registration(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    tie: &crate::peer_origin::PeerOriginTie,
) -> bool {
    // A missing `ConnectInfo` is treated as NOT loopback — the stricter
    // reading, and the same one `client_auth` fails closed on.
    if !peer.is_some_and(|p| p.ip().is_loopback()) {
        return false;
    }
    headers
        .get(ORIGIN_TIE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|presented| tie.holds(presented))
}

/// Remove every header in cw-rails' namespace, including the tie.
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
    /// The one roster read from a verified key to the member it names:
    /// cw-rails' roster through the membership port, read live, tombstones
    /// excluded (`active`).
    pub async fn member_by_pubkey(&self, dialer: NodePubkey) -> Option<MemberIdentity> {
        self.membership()
            .members()
            .await
            .into_iter()
            .find(|m| m.active && m.dial.node_pubkey == Some(dialer))
            .map(|m| MemberIdentity {
                name: m.name,
                node_id: m.node_id,
            })
    }

    /// THE internal resolver: what cw-rails said + where the connection came
    /// from → the principal this request is charged to, with cw-rails'
    /// namespace stripped from `headers` either way.
    ///
    /// Mutates `headers` deliberately. A handler downstream must not be able
    /// to re-read a claim this function declined, and the only way to promise
    /// that is to take it off the request (ARCH principle 10 — structural, not
    /// remembered). On a tied connection the identity triple is left in place
    /// for the log fields that already read it; only the tie is removed.
    pub async fn resolve_internal(
        &self,
        headers: &mut HeaderMap,
        peer: Option<SocketAddr>,
    ) -> Principal {
        if !tied_to_our_registration(headers, peer, &self.inner.node.peer_origin_tie) {
            let stripped = strip_mesh_headers(headers);
            // An untied caller that presented NOTHING claimed nothing, and
            // `Anonymous` is what "nothing was presented" means. Only a caller
            // that DID present an identity this daemon cannot tie to cw-rails
            // is `Unverified` — the arm a decider refuses (ARCH principle 6,
            // both directions).
            let claimed = stripped > 0;
            tracing::debug!(
                target: "transport",
                peer = ?peer,
                stripped,
                claimed,
                "internal: connection not tied to this daemon's cw-rails registration — \
                 any x-mesh-* was typed by the caller, not proved by a handshake, \
                 so it is stripped"
            );
            return if claimed {
                Principal::Unverified
            } else {
                Principal::Anonymous
            };
        }
        headers.remove(ORIGIN_TIE_HEADER);
        tracing::debug!(
            target: "transport",
            "internal: connection tied to cw-rails forwarding for this daemon"
        );

        let dialer = headers
            .get(PUBKEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(NodePubkey::from_hex);
        let Some(dialer) = dialer else {
            // cw-rails always writes the key it proved, so this is a forward
            // that did not come from its member arm. Reported, never assumed
            // harmless.
            tracing::warn!(
                target: "transport",
                "internal: the hop is tied but carries no readable verified key — \
                 resolving unverified"
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
                    "internal: cw-rails proved this key and the roster does not name it"
                );
                Principal::Unverified
            }
        }
    }
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
fn member_of(p: &Principal) -> Option<kernel_types::NodeId> {
    p.node_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::NodeId;

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

    /// A daemon whose roster (cw-rails', through the port) names one member,
    /// by the key `KEY`.
    fn state_with_member(node_id: NodeId) -> AppState {
        let seed = crate::state::FabricSeed {
            membership: Some(crate::double::roster(
                "Internal Principal Test",
                vec![crate::double::keyed_member(node_id, "LittleMac", KEY)],
            )),
            ..Default::default()
        };
        AppState::new_with_platform_and_engine_and_gauge_and_fabric(
            NodeId::from_u128(1),
            None,
            None,
            seed,
        )
    }

    /// THE failing input this module exists for. A caller that reaches the
    /// internal port without cw-rails in front types the whole identity
    /// triple — and is not believed, nor is the claim left on the request for
    /// a handler to find.
    #[tokio::test]
    async fn a_forged_identity_from_a_direct_connection_is_stripped_and_unverified() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
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
    #[tokio::test]
    async fn an_untied_caller_that_presented_nothing_is_anonymous() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = HeaderMap::new();
        let p = state.resolve_internal(&mut h, loopback()).await;
        assert_eq!(p, Principal::Anonymous, "nothing was presented");
    }

    /// A non-loopback caller that types only the peer header claims nothing
    /// in this module's namespace, so it is anonymous.
    #[tokio::test]
    async fn a_non_loopback_caller_typing_only_the_peer_header_is_anonymous() {
        let state = state_with_member(NodeId::from_u128(0xBEEF));
        let mut h = headers(&[("x-node-id", &NodeId::from_u128(0xBEEF).to_hex())]);
        let p = state.resolve_internal(&mut h, lan()).await;
        assert_eq!(p, Principal::Anonymous);
    }

    #[path = "tie.rs"]
    mod tie;
}
