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
    MEDIA_ALPN, RPC_ALPN,
};
use sovereign_mesh::iroh_access::{AcceptorRoutes, MemberCheck, MemberIdentity};

use crate::common;
use crate::common::{client_app_state, spawn_router};

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

/// How the lender's roster names the member — what its media origin is told.
fn member_identity() -> MemberIdentity {
    MemberIdentity {
        name: "LittleMac".into(),
        node_id: NodeId::from_u128(0xB0B),
    }
}

fn only_the_member() -> MemberCheck {
    let member = member_pubkey();
    Arc::new(move |k| Box::pin(std::future::ready((k == member).then(member_identity))))
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
        media: None,
        media_allow: Arc::new(Vec::new()),
    };
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec(), RPC_ALPN.to_vec()]).await;
    let check = only_the_member();
    let acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
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
        media: None,
        media_allow: Arc::new(Vec::new()),
    };
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec(), RPC_ALPN.to_vec()]).await;
    let check = only_the_member();
    let _acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
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

// ── federated media: the viewer half ─────────────────────────────────
//
// The holder half (`AcceptorRoutes::media`) was landed with a unit test on
// the routing decision. This is the byte plane end to end through the SAME
// tunnel the daemon's transport mints for `TrafficClass::Media`: a member's
// `HttpBridge` over `MEDIA_ALPN`, a lender whose acceptor forwards to a
// loopback origin that authenticates nothing, and a player-shaped `GET`.

/// A lender that declares a media origin — an HTTP server on loopback that
/// serves one "title" and honours `Range`, the two things a player needs.
async fn lender_with_media(title: &'static [u8]) -> (Endpoint, IrohAcceptor) {
    lender_with_media_allowing(title, Vec::new()).await
}

/// [`lender_with_media`] with an `[iroh] media_allow` list. The origin also
/// answers `/whoami` with the identity headers it was handed — the witness
/// that the acceptor, not the client, said who is asking.
async fn lender_with_media_allowing(
    title: &'static [u8],
    media_allow: Vec<String>,
) -> (Endpoint, IrohAcceptor) {
    use axum::http::{header, HeaderMap, StatusCode as S};
    use axum::routing::get;
    let whoami = |headers: HeaderMap| async move {
        let h = |n: &str| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<absent>")
                .to_string()
        };
        format!("{} {}", h("x-mesh-member"), h("x-mesh-node"))
    };
    let origin = spawn_router(axum::Router::new().route("/whoami", get(whoami)).route(
        "/library/title.bin",
        get(move |headers: HeaderMap| async move {
            // `bytes=A-B` → 206 with exactly that slice; no header → 200 whole.
            let range = headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes="))
                .and_then(|v| v.split_once('-'))
                .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
            match range {
                Some((a, b)) if a <= b && b < title.len() => {
                    (S::PARTIAL_CONTENT, title[a..=b].to_vec())
                }
                _ => (S::OK, title.to_vec()),
            }
        }),
    ))
    .await;

    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let routes = AcceptorRoutes {
        internal: "127.0.0.1:1".parse().unwrap(),
        rpc: None,
        peer: Some(spawn_router(client_router_for(state.clone(), ClientSurface::Peer)).await),
        guest: Some(spawn_router(client_router_for(state, ClientSurface::Guest)).await),
        media: Some(origin),
        media_allow: Arc::new(media_allow),
    };
    let endpoint = lender_endpoint(vec![CLIENT_ALPN.to_vec(), MEDIA_ALPN.to_vec()]).await;
    let check = only_the_member();
    let acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });
    (endpoint, acceptor)
}

/// The origin learns WHO is asking from the acceptor, which verified the key
/// in the handshake — never from the client, which can type anything. The
/// failing input is a forged `X-Mesh-Member` surviving the forward, or the
/// verified one being absent.
#[tokio::test]
async fn the_origin_is_told_the_members_verified_name_and_not_what_the_client_typed() {
    let (lender, _acceptor) = lender_with_media(b"x").await;
    let dialer = dialer_endpoint(MEMBER_SEED).await;
    let bridge = HttpBridge::spawn(dialer, dialable(&lender), MEDIA_ALPN)
        .await
        .expect("bridge binds");
    let resp = reqwest::Client::new()
        .get(format!("http://{}/whoami", bridge.local_addr()))
        .header("X-Mesh-Member", "forged")
        .header("X-Mesh-Node", "node-forged")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("the member's request is forwarded");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    let expected = format!("LittleMac {}", member_identity().node_id);
    assert_eq!(body, expected);
    drop(bridge);
}

/// `[iroh] media_allow` end to end: a member the list does not name gets no
/// bytes (the dial dies, as a stranger's does), and the same member named
/// is served — so this is a refusal, not a dead origin.
#[tokio::test]
async fn a_member_outside_media_allow_is_closed_and_one_inside_is_served() {
    const TITLE: &[u8] = b"allow-listed";
    let (lender, _acceptor) = lender_with_media_allowing(TITLE, vec!["SomeoneElse".into()]).await;
    let outcome = get_as(&lender, MEMBER_SEED, MEDIA_ALPN, "/library/title.bin", None).await;
    assert!(
        outcome.is_err(),
        "a member outside media_allow must be closed, got {:?}",
        outcome.map(|r| r.status())
    );

    let (lender, _acceptor) = lender_with_media_allowing(TITLE, vec!["LittleMac".into()]).await;
    let resp = get_as(&lender, MEMBER_SEED, MEDIA_ALPN, "/library/title.bin", None)
        .await
        .expect("the named member is served");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), TITLE);
}

/// THE demo's byte plane. A member's player reads a title from the peer's
/// origin through the bridge — whole, and by `Range`, because seeking is a
/// `Range` request and the splice must not touch it.
#[tokio::test]
async fn a_member_reads_a_title_from_the_peers_media_origin_by_key() {
    const TITLE: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let (lender, _acceptor) = lender_with_media(TITLE).await;

    let resp = get_as(&lender, MEMBER_SEED, MEDIA_ALPN, "/library/title.bin", None)
        .await
        .expect("a member's dial is forwarded to the origin");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), TITLE);

    // A seek: one Range request, byte-exact through the splice.
    let dialer = dialer_endpoint(MEMBER_SEED).await;
    let bridge = HttpBridge::spawn(dialer, dialable(&lender), MEDIA_ALPN)
        .await
        .expect("bridge binds");
    let resp = reqwest::Client::new()
        .get(format!("http://{}/library/title.bin", bridge.local_addr()))
        .header("Range", "bytes=10-19")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("the seek is served");
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), &TITLE[10..=19]);
    drop(bridge);
}

/// The arm that matters, end to end: the dial string is public (it rides in
/// every invite), so a stranger holding it must get NO bytes — the dial dies
/// rather than being downgraded to some listener that would answer. The
/// failing input is `forward_for` returning `media` for a non-member.
#[tokio::test]
async fn a_stranger_holding_the_dial_string_cannot_read_the_library() {
    let (lender, _acceptor) = lender_with_media(b"not for you").await;
    let outcome = get_as(
        &lender,
        STRANGER_SEED,
        MEDIA_ALPN,
        "/library/title.bin",
        None,
    )
    .await;
    assert!(
        outcome.is_err(),
        "a stranger's media dial must die, got {:?}",
        outcome.map(|r| r.status())
    );
    // …and the same lender still serves the member, so this is not a dead
    // origin passing as a refusal.
    let resp = get_as(&lender, MEMBER_SEED, MEDIA_ALPN, "/library/title.bin", None)
        .await
        .expect("the member is still served");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// A node that declares no origin closes even a member's dial — no route to
/// nowhere, and no accidental forward to the peer listener.
#[tokio::test]
async fn a_member_dialing_a_node_with_no_media_origin_is_closed_not_misrouted() {
    // `lender(true)` declares `media: None` and advertises no MEDIA_ALPN.
    let (lender, _acceptor) = lender(true).await;
    let outcome = get_as(&lender, MEMBER_SEED, MEDIA_ALPN, "/library/title.bin", None).await;
    assert!(
        outcome.is_err(),
        "no origin means no route, got {:?}",
        outcome.map(|r| r.status())
    );
}
