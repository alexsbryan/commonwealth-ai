// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plaintext-mesh join over iroh — the W2c proof that a joiner with
//! NO shared IP route reaches the founder by key and completes the
//! `/internal/join` admission over a QUIC tunnel, AND that the path is
//! fail-SOFT (unlike the encrypted join): an unreachable/garbage iroh
//! dial falls through to the direct-hint / mDNS paths instead of
//! erroring.
//!
//! Excluded from default workspace gates; run with:
//!   cargo test -p sovereign-mesh --features iroh-experimental \
//!       --test plaintext_join_over_iroh_e2e
//!
//! Topology (in-process, localhost only, relays disabled — CI-safe):
//!
//!   perform_join(iroh=Some(founder_dial,seed)) ── iroh bi-stream ──►
//!       IrohAcceptor ── TCP ── founder's internal axum router
//!
//! The positive test drives `perform_join`'s prefer-iroh arm end to
//! end; the fail-soft tests pin that a bad dial degrades to IP and
//! that no error text points a user at Tailscale (a W5 checklist item).
#![cfg(feature = "iroh-experimental")]

use std::net::SocketAddr;
use std::time::Duration;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_discovery::membership;
use commonwealth_transport::iroh::{EndpointBuilder, IrohAcceptor, SecretKey, ALPN};

use sovereign_mesh::join::perform_join;

/// A hermetic iroh endpoint: no relays, no address-lookup — dialing
/// works only via explicitly provided socket addresses, exactly the
/// shape a CI test needs (mirrors `iroh_transport_e2e`).
async fn bind_empty_endpoint(seed: u8) -> commonwealth_transport::iroh::Endpoint {
    EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[seed; 32]))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("iroh endpoint bind")
}

/// The endpoint's dialable localhost sockets (iroh binds the wildcard,
/// which isn't dialable as-is — rewrite to loopback).
fn dialable_sockets(endpoint: &commonwealth_transport::iroh::Endpoint) -> Vec<SocketAddr> {
    endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut a| {
            if a.ip().is_unspecified() {
                a.set_ip(if a.is_ipv4() {
                    "127.0.0.1".parse().unwrap()
                } else {
                    "::1".parse().unwrap()
                });
            }
            a
        })
        .collect()
}

/// Build a founder AppState from a fresh mesh; returns it plus the
/// plaintext join key.
fn build_founder(name: &str) -> (AppState, NodeId, String) {
    let founder_id = NodeId::from_u128(0xF0F0_F0F0_F0F0_F0F0);
    let (mesh, join_key) = membership::init_mesh_with_node_id(
        name,
        "Founder",
        vec!["127.0.0.1:9742".parse().unwrap()],
        founder_id,
    );
    (AppState::new(founder_id, mesh), founder_id, join_key)
}

async fn spawn_plain_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, internal_router(state)).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

#[tokio::test]
async fn plaintext_join_completes_over_iroh_tunnel() {
    let mesh_name = "Iroh Plaintext Mesh";
    let (founder_state, _founder_id, join_key) = build_founder(mesh_name);

    // Founder's internal router behind an iroh acceptor — the ONLY
    // ingress the joiner is given (no relay hint, no mDNS below).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router_addr = listener.local_addr().unwrap();
    let router_state = founder_state.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, internal_router(router_state)).await;
    });

    let founder_ep = bind_empty_endpoint(11).await;
    let id_hex = hex::encode(founder_ep.id().as_bytes());
    let sockets = dialable_sockets(&founder_ep);
    assert!(!sockets.is_empty(), "founder must expose a dialable socket");
    // The `dial=` connect code a plaintext invite carries: id@addr,addr.
    let founder_dial = format!(
        "{id_hex}@{}",
        sockets
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let _acceptor = IrohAcceptor::spawn(founder_ep, router_addr);

    // Joiner: iroh-only. No relay hint, no mDNS — if the tunnel path
    // didn't work, this could not possibly join.
    let joiner_seed = [22u8; 32];
    let joiner_addr: SocketAddr = "127.0.0.1:9876".parse().unwrap();
    let result = perform_join(
        mesh_name,
        &join_key,
        "IrohJoiner",
        vec![joiner_addr],
        Some((founder_dial.as_str(), joiner_seed)),
        &commonwealth_transport::iroh::RelayConfig::default(),
        None, // no relay hint
        None, // no mDNS
        Duration::from_secs(5),
        Some(NodeId::from_u128(0x5151_5151_5151_5151)),
        None,
    )
    .await
    .expect("join over the iroh tunnel must succeed");

    // Adopted the founder's mesh, and it is NOT encrypted — a plaintext
    // mesh reached over iroh, exactly the W2c goal.
    assert_eq!(result.mesh.name, mesh_name);
    assert!(
        !result.mesh.require_encryption,
        "a plaintext mesh joined over iroh must stay plaintext"
    );
    assert!(
        result.mesh.members.len() >= 2,
        "snapshot must include founder + joiner"
    );

    // The founder's live state actually admitted the joiner (real
    // admission over the tunnel, not an echo).
    let live = founder_state.inner.mesh.read().await;
    assert!(
        live.members.values().any(|m| m.name == "IrohJoiner"),
        "founder must have admitted the joiner over iroh"
    );
}

#[tokio::test]
async fn bad_iroh_dial_falls_back_to_direct_hint() {
    // Fail-soft: a malformed `dial=` fails fast (before any endpoint
    // is built) and the join falls through to the direct hint — the
    // encrypted join, by contrast, would error here.
    let mesh_name = "Fallback Mesh";
    let (founder_state, _id, join_key) = build_founder(mesh_name);
    let addr = spawn_plain_router(founder_state.clone()).await;

    let result = perform_join(
        mesh_name,
        &join_key,
        "FallbackJoiner",
        vec!["127.0.0.1:9876".parse().unwrap()],
        Some(("not-a-valid-dial-string", [33u8; 32])),
        &commonwealth_transport::iroh::RelayConfig::default(),
        Some(&addr.to_string()), // working direct hint
        None,
        Duration::from_secs(5),
        Some(NodeId::from_u128(0x5252_5252_5252_5252)),
        None,
    )
    .await
    .expect("must fall back to the direct hint and join");

    assert_eq!(result.mesh.name, mesh_name);
    assert!(founder_state
        .inner
        .mesh
        .read()
        .await
        .members
        .values()
        .any(|m| m.name == "FallbackJoiner"));
}

#[tokio::test]
async fn join_failure_never_suggests_tailscale() {
    // W5 checklist item: no join error may point a user at Tailscale.
    // Every path fails here (bad dial, unreachable hint, no mDNS).
    let unreachable: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let err = perform_join(
        "Nowhere Mesh",
        // A validly-FORMATTED key so we reach the network paths rather
        // than bailing on format validation upstream.
        "cwth-0000-1111-2222",
        "LonelyJoiner",
        vec!["127.0.0.1:9876".parse().unwrap()],
        Some(("also-garbage", [44u8; 32])),
        &commonwealth_transport::iroh::RelayConfig::default(),
        Some(&unreachable.to_string()),
        None,
        Duration::from_millis(200),
        None,
        None,
    )
    .await
    .expect_err("with every path dead the join must fail");

    let msg = err.to_string().to_lowercase();
    assert!(
        !msg.contains("tailscale"),
        "join error must not suggest Tailscale: {msg}"
    );
}
