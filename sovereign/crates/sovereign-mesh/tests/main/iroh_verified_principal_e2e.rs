// SPDX-License-Identifier: AGPL-3.0-or-later
//! The verified-principal half of the dialer-admission suite: `cwth/http/0`
//! carrying the acceptor's word, and the two-daemon chain behind it.
//!
//! Split out of `iroh_dialer_admission_e2e` on 2026-09-20 to keep that file
//! under the 1200-line ceiling (ARCH §3.2). Its lender/dialer helpers are
//! reused from there rather than copied (ARCH principle 11), which is why
//! they are `pub(crate)`.

use std::collections::HashMap;
use std::time::Duration;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_transport::iroh::{Endpoint, HttpBridge, IrohAcceptor, ALPN};
use sovereign_daemon::server::{client_router_for, ClientSurface};
use sovereign_mesh::iroh_access::{AcceptorRoutes, MediaRoute};

use crate::common;
use crate::common::{client_app_state, spawn_router};
use crate::iroh_dialer_admission_e2e::{
    dialable, dialer_endpoint, key, lender_endpoint, member_identity, member_pubkey,
    only_the_member, MEMBER_SEED, STRANGER_SEED, TOKEN,
};

// ---------------------------------------------------------------------------
// `cwth/http/0` — the internal router is told who crossed the tunnel
// ---------------------------------------------------------------------------
//
// Until 2026-09-20 this ALPN was a bare `Forward::Splice`: the internal router
// learned nothing about the dialer, so every route behind it had to believe an
// `x-node-id` header the CLIENT typed. These two drive the identity forward end
// to end over a real iroh handshake — the same shape as the media pair above,
// against the listener the mesh's own control plane lives behind.

/// A lender whose internal forward is an origin that echoes the `x-mesh-*` it
/// was handed, standing in for the daemon's internal router.
async fn lender_with_internal_echo() -> (Endpoint, IrohAcceptor) {
    use axum::http::HeaderMap;
    use axum::routing::get;
    let whoami = |headers: HeaderMap| async move {
        let h = |n: &str| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<absent>")
                .to_string()
        };
        format!(
            "{} {} {}",
            h("x-mesh-member"),
            h("x-mesh-node"),
            h("x-mesh-pubkey")
        )
    };
    let internal = spawn_router(axum::Router::new().route("/whoami", get(whoami))).await;

    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let routes = AcceptorRoutes {
        apps: Default::default(),
        internal,
        rpc: None,
        peer: Some(spawn_router(client_router_for(state.clone(), ClientSurface::Peer)).await),
        guest: Some(spawn_router(client_router_for(state, ClientSurface::Guest)).await),
        media: MediaRoute::fixed(None, Vec::new()),
        offer: Default::default(),
    };
    let endpoint = lender_endpoint(vec![ALPN.to_vec()]).await;
    let check = only_the_member();
    let acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });
    (endpoint, acceptor)
}

/// GET `/whoami` over the internal ALPN as `seed`, typing forged `x-mesh-*`.
async fn internal_whoami_as(lender: &Endpoint, seed: u8) -> String {
    let dialer = dialer_endpoint(seed).await;
    let bridge = HttpBridge::spawn(dialer, dialable(lender), ALPN)
        .await
        .expect("bridge binds");
    let resp = reqwest::Client::new()
        .get(format!("http://{}/whoami", bridge.local_addr()))
        .header("X-Mesh-Member", "forged")
        .header("X-Mesh-Node", "node-forged")
        .header("X-Mesh-Pubkey", "ff".repeat(32))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("the dial is forwarded");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    drop(bridge);
    body
}

/// The internal router learns who crossed the tunnel from the ACCEPTOR, which
/// verified the key in the handshake — never from the client. The failing
/// input is a forged `x-mesh-node` surviving the hop, which is exactly what a
/// splice let through.
#[tokio::test]
async fn the_internal_router_is_told_the_verified_dialer_and_not_what_the_client_typed() {
    let (lender, _acceptor) = lender_with_internal_echo().await;
    let body = internal_whoami_as(&lender, MEMBER_SEED).await;
    let expected = format!(
        "LittleMac {} {}",
        member_identity().node_id,
        hex::encode(member_pubkey().0)
    );
    assert_eq!(body, expected, "the acceptor's word, not the client's");
}

/// A joiner is not a member yet and still reaches this ALPN — that is
/// deliberate and unchanged. What it gets named by is the one thing the
/// handshake proved: its key. The roster's silence is an ABSENT member header,
/// not a placeholder one, so a route that needs a member can see that it has
/// none (ARCH principle 6).
#[tokio::test]
async fn a_joiner_is_named_by_its_verified_key_and_by_no_membership_it_lacks() {
    let (lender, _acceptor) = lender_with_internal_echo().await;
    let body = internal_whoami_as(&lender, STRANGER_SEED).await;
    let stranger = NodePubkey(*key(STRANGER_SEED).public().as_bytes());
    assert_eq!(
        body,
        format!("<absent> <absent> {}", hex::encode(stranger.0)),
        "a non-member is keyed, never named — and never the forged name"
    );
}

// ---------------------------------------------------------------------------
// Two daemons over a real handshake: the peer principal IS the verified key
// ---------------------------------------------------------------------------
//
// bar `mp-principal-is-the-verified-key`, clauses (a), (b) and (d).
//
// The section above proves the ACCEPTOR forwards the verified dialer. These
// three drive the whole chain behind it — `internal_principal_layer` resolving
// the hop, the peer gate deciding on what it resolved — against the two
// forgeries the bar names and the caller the bar says must be refused.

/// The probe: the REAL internal resolver and the REAL peer gate in front of a
/// route that renders the principal it was decided on.
///
/// Rendering `Principal::label()` rather than a header is the point: a test
/// that read a header back would be asserting on the same bytes the forgery
/// supplies.
async fn internal_principal_probe(
    state: sovereign_daemon::state::AppState,
) -> std::net::SocketAddr {
    use axum::routing::get;
    use sovereign_serving_host::admission::AttachedPrincipal;
    let render = |attached: Option<axum::Extension<AttachedPrincipal>>| async move {
        attached
            .map(|axum::Extension(a)| a.0.label())
            .unwrap_or_else(|| "<no principal layer ran>".to_string())
    };
    let router = axum::Router::new()
        .route("/whoami", get(render))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            sovereign_daemon::admission::peer_knowledge_read_layer::<
                sovereign_daemon::state::AppState,
            >,
        ))
        // OUTERMOST, as `server::internal_router` mounts it.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            sovereign_daemon::internal_principal::internal_principal_layer,
        ))
        .with_state(state);
    spawn_router(router).await
}

/// A lender whose internal forward is the probe above, and whose roster names
/// the member dialer by the key the handshake will prove.
async fn lender_with_internal_principal() -> (Endpoint, IrohAcceptor, std::net::SocketAddr) {
    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    common::name_member_with_key(
        &state,
        member_identity().node_id,
        &member_identity().name,
        member_pubkey().0,
    )
    .await;
    let internal = internal_principal_probe(state.clone()).await;

    let routes = AcceptorRoutes {
        apps: Default::default(),
        internal,
        rpc: None,
        peer: Some(spawn_router(client_router_for(state.clone(), ClientSurface::Peer)).await),
        guest: Some(spawn_router(client_router_for(state, ClientSurface::Guest)).await),
        media: MediaRoute::fixed(None, Vec::new()),
        offer: Default::default(),
    };
    let endpoint = lender_endpoint(vec![ALPN.to_vec()]).await;
    let check = only_the_member();
    let acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });
    (endpoint, acceptor, internal)
}

/// GET `/whoami` over the internal ALPN as `seed`, typing `forged`.
async fn principal_over_iroh(lender: &Endpoint, seed: u8, forged: &[(&str, String)]) -> String {
    let dialer = dialer_endpoint(seed).await;
    let bridge = HttpBridge::spawn(dialer, dialable(lender), ALPN)
        .await
        .expect("bridge binds");
    let mut req = reqwest::Client::new()
        .get(format!("http://{}/whoami", bridge.local_addr()))
        .timeout(Duration::from_secs(10));
    for (name, value) in forged {
        req = req.header(*name, value);
    }
    let resp = req.send().await.expect("the dial is forwarded");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    drop(bridge);
    body
}

/// The node the forgeries below claim to be — a third member, not the dialer.
fn claimed_other() -> NodeId {
    NodeId::from_u128(0xC0C0)
}

/// bar `mp-principal-is-the-verified-key` (a)
///
/// B calls A typing `x-node-id: <C>`. A attributes the call to B. The failing
/// input is the whole reason this rung exists: every decider on A used to take
/// the requester from that header, so B could spend C's reciprocity by typing
/// four bytes.
#[tokio::test]
async fn a_peer_typing_another_nodes_x_node_id_is_still_itself() {
    let (lender, _acceptor, _probe) = lender_with_internal_principal().await;
    let body = principal_over_iroh(
        &lender,
        MEMBER_SEED,
        &[("x-node-id", claimed_other().to_hex())],
    )
    .await;
    assert_eq!(
        body,
        format!("member:{}", member_identity().node_id.to_hex()),
        "the principal is the key the handshake proved, not the id typed"
    );
}

/// bar `mp-principal-is-the-verified-key` (b)
///
/// The same forgery in the acceptor's OWN namespace, which is the one a
/// rename would move to. B types the full verified triple naming C — member
/// name, node and public key — and is still B: the acceptor strips what the
/// client typed before writing what it proved.
#[tokio::test]
async fn a_peer_typing_the_whole_verified_triple_for_another_node_is_still_itself() {
    let (lender, _acceptor, _probe) = lender_with_internal_principal().await;
    let stranger = NodePubkey(*key(STRANGER_SEED).public().as_bytes());
    let body = principal_over_iroh(
        &lender,
        MEMBER_SEED,
        &[
            ("x-mesh-member", "SomebodyElse".to_string()),
            ("x-mesh-node", claimed_other().to_string()),
            ("x-mesh-pubkey", hex::encode(stranger.0)),
        ],
    )
    .await;
    assert_eq!(
        body,
        format!("member:{}", member_identity().node_id.to_hex()),
        "a forged identity triple must not survive the acceptor's hop"
    );
}

/// bar `mp-principal-is-the-verified-key` (c) and (d)
///
/// The caller the bar names: a DIRECT TCP dial to the internal port with no
/// acceptor in front, presenting the whole triple for C. It does not become C
/// — and because a peer ceiling cannot be keyed on an identity nobody proved,
/// it is refused with a sentence that names the route and says why, rather
/// than served under a bucket any caller could pick.
#[tokio::test]
async fn a_direct_caller_presenting_a_forged_identity_is_refused_by_name() {
    let (_lender, _acceptor, probe) = lender_with_internal_principal().await;
    let stranger = NodePubkey(*key(STRANGER_SEED).public().as_bytes());
    let resp = reqwest::Client::new()
        .get(format!("http://{probe}/whoami"))
        .header("x-mesh-member", "SomebodyElse")
        .header("x-mesh-node", claimed_other().to_string())
        .header("x-mesh-pubkey", hex::encode(stranger.0))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("the internal port answers");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a claim with no handshake behind it must be refused, not served"
    );
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("/whoami") && text.contains("not verified"),
        "the refusal must name the route and say why: {text}"
    );
}

/// bar `mp-principal-is-the-verified-key` (a) — the bar's demo, on the real
/// route.
///
/// "Node B calls node A's knowledge search claiming to be node C. A answers B
/// and names B." Driven through the whole production chain: a real iroh
/// handshake, `AcceptorRoutes::forward_for`, `server::internal_router` with
/// its own principal layer, and the ledger `routes_internal::knowledge_search`
/// actually writes. The forged `x-node-id: C` is the input every decider on A
/// used to take the requester from.
#[tokio::test]
async fn the_knowledge_ledger_names_the_dialer_and_not_the_node_it_claimed_to_be() {
    use crate::knowledge_served_e2e::{build_state_with_corpora, EMBED_DIM};
    use commonwealth_core::contributions::LedgerEventKind;

    let (state, _tmp) = build_state_with_corpora(
        NodeId::from_u128(0xA11CE),
        &[("sep", "Stanford Encyclopedia", "Free will and determinism.")],
    )
    .await;
    common::name_member_with_key(
        &state,
        member_identity().node_id,
        &member_identity().name,
        member_pubkey().0,
    )
    .await;
    let internal = spawn_router(sovereign_daemon::server::internal_router(state.clone())).await;

    let routes = AcceptorRoutes {
        apps: Default::default(),
        internal,
        rpc: None,
        peer: None,
        guest: None,
        media: MediaRoute::fixed(None, Vec::new()),
        offer: Default::default(),
    };
    let endpoint = lender_endpoint(vec![ALPN.to_vec()]).await;
    let check = only_the_member();
    let _acceptor = IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
        let check = check.clone();
        let routes = routes.clone();
        async move { routes.forward_for(&alpn, dialer, &check).await }
    });

    let dialer = dialer_endpoint(MEMBER_SEED).await;
    let bridge = HttpBridge::spawn(dialer, dialable(&endpoint), ALPN)
        .await
        .expect("bridge binds");
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/internal/knowledge/search",
            bridge.local_addr()
        ))
        // The forgery: B types C's node id on its own request.
        .header("X-Node-Id", claimed_other().to_hex())
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("the dial is forwarded");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "A answers B");
    drop(bridge);

    let events = state
        .inner
        .fabric
        .contribution_emitter
        .events()
        .expect("emitter.events() reads");
    let served: Vec<NodeId> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::KnowledgeQueryServed { for_node, .. } => Some(*for_node),
            _ => None,
        })
        .collect();
    assert_eq!(
        served,
        vec![member_identity().node_id],
        "A must credit the key it verified. Crediting {} — the id B typed — \
         is how B spends C's reciprocity",
        claimed_other()
    );
}
