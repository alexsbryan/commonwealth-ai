// SPDX-License-Identifier: AGPL-3.0-or-later
//! Holding a node's iroh dial string is not a credential — proven on the wire.
//!
//! The dial string is PUBLIC. It rides in every mesh invite's `dial=` and is
//! gossiped as `MemberRecord.node_pubkey`, so anyone who has ever seen an
//! invite, or any peer of a peer, has it. Until 2026-08-27 the acceptor routed
//! purely by ALPN and forwarded `CLIENT_ALPN` to the daemon's own client
//! listener — which admits a loopback caller before reading a bearer, and the
//! acceptor's forward hop IS loopback. Anyone holding the string reached the
//! whole client API with no credential.
//!
//! `AcceptorRoutes::forward_for` fixes that by consulting the ONE thing a QUIC
//! handshake actually proves: the dialer's Ed25519 key, the same key the mesh
//! gossips. These tests drive it through a real `IrohAcceptor`, real
//! `commonwealth_api` client routers, and real `HttpBridge` dials — and the
//! `..._is_the_hole_this_closes` twin reproduces the old behaviour so the fix
//! is watched succeeding against a failure that is watched failing.
//!
//! Hermetic: `EndpointBuilder::empty()` on both sides, loopback UDP sockets in
//! the dial string, no relay and no n0 contact.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_api::server::{client_router, client_router_for, ClientSurface};
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_transport::iroh::{
    Endpoint, EndpointAddr, EndpointBuilder, HttpBridge, IrohAcceptor, SecretKey, CLIENT_ALPN,
    RPC_ALPN,
};
use sovereign_mesh::iroh_access::{AcceptorRoutes, MemberCheck};

mod common;
use common::{client_app_state, spawn_router};

const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const MEMBER_SEED: u8 = 41;
const STRANGER_SEED: u8 = 42;

fn key(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
}

/// The dialer key the lender's mesh has gossiped as a member.
fn member_pubkey() -> NodePubkey {
    NodePubkey(*key(MEMBER_SEED).public().as_bytes())
}

fn only_the_member() -> MemberCheck {
    let member = member_pubkey();
    Arc::new(move |k| Box::pin(std::future::ready(k == member)))
}

/// iroh binds the wildcard, which is not dialable as-is — rewrite to loopback.
fn dialable(endpoint: &Endpoint) -> EndpointAddr {
    let mut addr = EndpointAddr::new(endpoint.id());
    for mut a in endpoint.bound_sockets() {
        if a.ip().is_unspecified() {
            if a.is_ipv4() {
                a.set_ip("127.0.0.1".parse().unwrap());
            } else {
                a.set_ip("::1".parse().unwrap());
            }
        }
        addr = addr.with_ip_addr(a);
    }
    addr
}

async fn lender_endpoint(alpns: Vec<Vec<u8>>) -> Endpoint {
    EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(key(3))
        .alpns(alpns)
        .bind()
        .await
        .expect("lender endpoint binds")
}

async fn dialer_endpoint(seed: u8) -> Endpoint {
    EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(key(seed))
        .bind()
        .await
        .expect("dialer endpoint binds")
}

/// A lender wired the way `MeshIrohAccess::start` wires one: the peer and
/// guest binds of the client router up, and the acceptor routing through the
/// real decider.
///
/// The operator's own `:9741` listener is deliberately absent — the whole
/// point of the peer bind is that nothing here forwards to it.
///
/// `with_guest` false simulates the guest listener failing to bind — the
/// fail-closed case.
async fn lender(with_guest: bool) -> (Endpoint, IrohAcceptor) {
    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let peer = Some(spawn_router(client_router_for(state.clone(), ClientSurface::Peer)).await);
    let guest = if with_guest {
        Some(spawn_router(client_router_for(state, ClientSurface::Guest)).await)
    } else {
        None
    };

    let routes = AcceptorRoutes {
        // Nothing listens on these two in this test; no case below routes to
        // them, and a regression that did would surface as a dead connection
        // rather than as a pass.
        internal: "127.0.0.1:1".parse().unwrap(),
        rpc: Some("127.0.0.1:2".parse().unwrap()),
        peer,
        guest,
    };
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec(), RPC_ALPN.to_vec()]).await;
    let check = only_the_member();
    let acceptor = IrohAcceptor::spawn_admitting(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });
    (endpoint, acceptor)
}

/// GET `path` from `lender` as the holder of `seed`'s key, over `alpn`.
async fn get_as(
    lender: &Endpoint,
    seed: u8,
    alpn: &'static [u8],
    path: &str,
    bearer: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let dialer = dialer_endpoint(seed).await;
    let bridge = HttpBridge::spawn(dialer, dialable(lender), alpn)
        .await
        .expect("bridge binds");
    let mut req = reqwest::Client::new()
        .get(format!("http://{}{path}", bridge.local_addr()))
        .timeout(Duration::from_secs(10));
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.send().await;
    // Hold the bridge until the response is in hand — dropping it aborts the
    // accept loop mid-request.
    drop(bridge);
    resp
}

/// THE fix. A stranger holding the public dial string reaches the
/// bearer-checking listener, not the one that trusts the forward hop.
#[tokio::test]
async fn a_stranger_holding_the_dial_string_gets_no_free_access() {
    let (lender, _acceptor) = lender(true).await;
    let resp = get_as(&lender, STRANGER_SEED, CLIENT_ALPN, "/v1/models", None)
        .await
        .expect("the connection is served, just not admitted");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "possession of a gossiped dial string is not a credential"
    );
}

/// The twin, and the reason the test above means something: wire the SAME
/// lender the way it was wired before — routing on ALPN alone — and the same
/// stranger, presenting nothing, gets the whole client API.
#[tokio::test]
async fn routing_on_alpn_alone_is_the_hole_this_closes() {
    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let client = spawn_router(client_router(state)).await;
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec()]).await;
    let mut routes = HashMap::new();
    routes.insert(CLIENT_ALPN.to_vec(), client);
    let _acceptor = IrohAcceptor::spawn_routed(endpoint.clone(), routes);

    let resp = get_as(&endpoint, STRANGER_SEED, CLIENT_ALPN, "/v1/models", None)
        .await
        .expect("request reaches the lender");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "this is what the fix removes: no key, no bearer, full client API"
    );
}

/// And the arm that must survive it. Peer federated inference carries no
/// `Authorization` header at all — its key IS the credential, so a member
/// still reaches the listener that admits without one.
#[tokio::test]
async fn a_member_still_reaches_the_client_api_with_no_bearer() {
    let (lender, _acceptor) = lender(true).await;
    let resp = get_as(&lender, MEMBER_SEED, CLIENT_ALPN, "/v1/models", None)
        .await
        .expect("request reaches the lender");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "membership-by-key is what peer inference presents instead of a bearer"
    );
}

/// A stranger is downgraded, not walled off. Presenting the daemon token gets
/// it in — the same posture it would meet calling a LAN-bound daemon.
#[tokio::test]
async fn a_stranger_that_does_hold_a_credential_is_admitted() {
    let (lender, _acceptor) = lender(true).await;
    let resp = get_as(
        &lender,
        STRANGER_SEED,
        CLIENT_ALPN,
        "/v1/models",
        Some(TOKEN),
    )
    .await
    .expect("request reaches the lender");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// `/status` and `/oicp/v1/capabilities` must stay readable by a node that
/// could not yet hold anything — that is what they are for. Downgrading a
/// stranger to the bearer listener preserves them; refusing the dial outright
/// would not.
#[tokio::test]
async fn a_stranger_can_still_read_the_federation_handshake() {
    let (lender, _acceptor) = lender(true).await;
    let resp = get_as(&lender, STRANGER_SEED, CLIENT_ALPN, "/status", None)
        .await
        .expect("request reaches the lender");
    assert!(resp.status().is_success(), "got {}", resp.status());
}

/// Fail CLOSED. With no bearer-checking listener there is nothing safe to send
/// a stranger to, and the trusting listener is not a fallback: the connection
/// dies instead.
#[tokio::test]
async fn a_stranger_is_dropped_when_there_is_no_listener_to_downgrade_to() {
    let (lender, _acceptor) = lender(false).await;
    let outcome = get_as(&lender, STRANGER_SEED, CLIENT_ALPN, "/v1/models", None).await;
    assert!(
        outcome.is_err(),
        "expected a dead connection, got {:?}",
        outcome.map(|r| r.status())
    );
    // …and the member arm is untouched, so this is not "refuse everything".
    let resp = get_as(&lender, MEMBER_SEED, CLIENT_ALPN, "/v1/models", None)
        .await
        .expect("a member is still served");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// The rpc-server behind `RPC_ALPN` speaks raw tensor operations and
/// authenticates nothing, so there is no downgrade — a stranger is refused
/// outright.
#[tokio::test]
async fn a_stranger_cannot_open_the_tensor_rpc_path_at_all() {
    let (lender, _acceptor) = lender(true).await;
    let outcome = get_as(&lender, STRANGER_SEED, RPC_ALPN, "/", None).await;
    assert!(
        outcome.is_err(),
        "the rpc path has no credential of its own, so the dial must die: {:?}",
        outcome.map(|r| r.status())
    );
}

// ── the operator-only surface ───────────────────────────────────────
//
// `forward_for` narrowed CLIENT_ALPN from "anyone holding the public dial
// string" to "any member". That is a real reduction and it is not the whole
// bar: `routes_internal/guest_grant.rs` argues these routes are safe on
// `:9741` because `client_auth` there means "loopback-or-full-token". On a
// listener the iroh acceptor feeds, the loopback half of that is free — the
// acceptor forwards by `TcpStream::connect("127.0.0.1")` — so a member
// reached guest-grant minting on someone else's node with nothing presented.
//
// The fix is which router the listener SERVES, not a guard on the routes.

/// THE second fix. A member is admitted (no bearer, as federated inference
/// requires) and still cannot see the operator's own surface at all.
#[tokio::test]
async fn a_member_cannot_reach_the_operator_only_routes() {
    let (lender, _acceptor) = lender(true).await;

    let resp = get_as(
        &lender,
        MEMBER_SEED,
        CLIENT_ALPN,
        "/internal/guest/grant/list",
        None,
    )
    .await
    .expect("the connection is served, just not this route");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a peer must not be able to mint a credential for an outsider on this node"
    );

    // …and this is not a dead listener: the same dialer, same tunnel, one
    // route over, is served.
    let resp = get_as(&lender, MEMBER_SEED, CLIENT_ALPN, "/v1/models", None)
        .await
        .expect("request reaches the lender");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// The twin, watched failing. Wire the member arm the way it was wired until
/// 2026-08-28 — at the operator's own listener — and the identical request
/// from the identical dialer is served. Nothing about the tunnel, the key
/// check, or the route changed between the two; only the router behind the
/// listener did.
#[tokio::test]
async fn routing_a_member_at_the_operator_listener_is_the_hole_this_closes() {
    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let routes = AcceptorRoutes {
        internal: "127.0.0.1:1".parse().unwrap(),
        rpc: Some("127.0.0.1:2".parse().unwrap()),
        // The pre-fix wiring: CLIENT_ALPN from a member landed on the full
        // client router, `/internal/*` and all.
        peer: Some(spawn_router(client_router(state.clone())).await),
        guest: Some(spawn_router(client_router_for(state, ClientSurface::Guest)).await),
    };
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec(), RPC_ALPN.to_vec()]).await;
    let check = only_the_member();
    let _acceptor = IrohAcceptor::spawn_admitting(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });

    let resp = get_as(
        &endpoint,
        MEMBER_SEED,
        CLIENT_ALPN,
        "/internal/guest/grant/list",
        None,
    )
    .await
    .expect("request reaches the lender");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "this is what the fix removes: a peer minting guest credentials on your node"
    );
}
