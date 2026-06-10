// SPDX-License-Identifier: AGPL-3.0-or-later
//! IrohTransport spike e2e — the honest proof behind the
//! PeerTransport seam: a REAL gossip round, dialed by Ed25519
//! pubkey over iroh QUIC, served by the unmodified axum internal
//! router, sent by an unmodified reqwest client.
//!
//! Excluded from default workspace gates; run with:
//!   cargo test -p sovereign-mesh --features iroh-experimental --test iroh_transport_e2e
//!
//! Topology (everything in-process, localhost UDP only, relays and
//! address-lookup disabled — CI-safe, no internet):
//!
//!   reqwest ── http://127.0.0.1:<bridge>  (IrohTransport's TCP bridge)
//!      │                │
//!      │        iroh bi-stream, dialed by the founder's pubkey
//!      │                │
//!      └──────► IrohAcceptor ── TCP ── founder's internal axum router
#![cfg(feature = "iroh-experimental")]

use std::net::SocketAddr;
use std::time::Duration;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_discovery::membership;
use commonwealth_transport::iroh::{
    Endpoint, EndpointBuilder, IrohAcceptor, IrohTransport, SecretKey, ALPN,
};
use commonwealth_transport::{PeerContact, PeerTransport, TrafficClass};

async fn bind_iroh_endpoint(seed: u8) -> Endpoint {
    // Builder::empty(): no relays, no address-lookup services —
    // dialing works only via explicitly provided socket addresses,
    // which is exactly the hermetic shape a CI test needs. empty()
    // also sets no crypto provider (only presets pick one), so it
    // must be passed explicitly.
    EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[seed; 32]))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("iroh endpoint bind")
}

/// The endpoint's dialable localhost UDP addresses. iroh binds the
/// wildcard, which is not dialable as-is — rewrite to loopback.
fn dialable_sockets(endpoint: &Endpoint) -> Vec<SocketAddr> {
    endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut a| {
            if a.ip().is_unspecified() {
                if a.is_ipv4() {
                    a.set_ip("127.0.0.1".parse().unwrap());
                } else {
                    a.set_ip("::1".parse().unwrap());
                }
            }
            a
        })
        .collect()
}

#[tokio::test]
async fn gossip_round_trips_over_iroh_dialed_by_pubkey() {
    // ── Founder side ────────────────────────────────────────────
    let founder_id = NodeId::from_u128(0xF0F0_F0F0_F0F0_F0F0);
    let (mesh, _join_key) = membership::init_mesh_with_node_id(
        "Iroh Spike Mesh",
        "Founder",
        vec!["127.0.0.1:9742".parse().unwrap()],
        founder_id,
    );
    let founder_mesh = mesh.clone();
    let founder_state = AppState::new(founder_id, mesh);

    // The daemon's existing internal HTTP listener, untouched.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router_addr = listener.local_addr().unwrap();
    let founder_state_for_router = founder_state.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, internal_router(founder_state_for_router)).await;
    });

    // Founder's iroh endpoint + the acceptor bridging bi-streams
    // into that listener.
    let founder_ep = bind_iroh_endpoint(1).await;
    let founder_pubkey = NodePubkey(*founder_ep.id().as_bytes());
    let founder_sockets = dialable_sockets(&founder_ep);
    assert!(
        !founder_sockets.is_empty(),
        "founder endpoint must expose a dialable UDP socket"
    );
    let _acceptor = IrohAcceptor::spawn(founder_ep, router_addr);

    // ── Client side ─────────────────────────────────────────────
    let client_ep = bind_iroh_endpoint(2).await;
    let transport = IrohTransport::new(client_ep);
    transport.add_known_peer(founder_pubkey, founder_sockets);

    // The contact a MemberRecord would yield: the gossiped IP
    // addresses are irrelevant to this transport — only the
    // identity pubkey matters. That's the seam's whole claim.
    let contact = PeerContact {
        node_id: founder_id,
        addresses: vec![],
        node_pubkey: Some(founder_pubkey),
    };
    let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
    assert_eq!(endpoints.len(), 1, "dial-by-key yields one bridge endpoint");
    let base_url = &endpoints[0].base_url;
    assert!(base_url.starts_with("http://127.0.0.1:"), "{base_url}");

    // ── A real gossip round through the tunnel ──────────────────
    // Our view: the founder's mesh (same id + join_key_hash, so the
    // auth boundary admits it) plus one member the founder doesn't
    // know yet.
    let mut my_view = founder_mesh.clone();
    let mut peer_record = my_view.members.get(&founder_id).unwrap().clone();
    peer_record.node_id = NodeId::from_u128(0xABCD);
    peer_record.name = "IrohPeer".to_string();
    peer_record.node_pubkey = Some(NodePubkey([0x42; 32]));
    my_view.members.insert(peer_record.node_id, peer_record);

    let wire_members: Vec<&commonwealth_core::mesh::MemberRecord> =
        my_view.members.values().collect();
    let body = serde_json::json!({
        "mesh": {
            "id": my_view.id,
            "name": my_view.name,
            "join_key_hash": my_view.join_key_hash,
            "members": wire_members,
            "peers": [],
        }
    });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = http
        .post(format!("{base_url}/internal/gossip"))
        .json(&body)
        .send()
        .await
        .expect("gossip POST over the iroh bridge must succeed");
    assert!(
        resp.status().is_success(),
        "gossip over iroh: {}",
        resp.status()
    );

    // The founder's reply (its updated snapshot) came back through
    // the same tunnel and includes the member we introduced.
    let reply: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = reply["mesh"]["members"]
        .as_array()
        .expect("members array")
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(names.contains(&"Founder"), "reply names: {names:?}");
    assert!(names.contains(&"IrohPeer"), "reply names: {names:?}");

    // And the founder's live AppState actually merged it — this was
    // a real anti-entropy round, not an echo.
    let live = founder_state.inner.mesh.read().await;
    let merged = live
        .members
        .values()
        .find(|m| m.name == "IrohPeer")
        .expect("founder merged the gossiped member");
    assert_eq!(
        merged.node_pubkey,
        Some(NodePubkey([0x42; 32])),
        "identity pubkey survived the round-trip"
    );
}

#[tokio::test]
async fn peer_without_pubkey_is_not_dialable_on_iroh() {
    let client_ep = bind_iroh_endpoint(3).await;
    let transport = IrohTransport::new(client_ep);
    let contact = PeerContact {
        node_id: NodeId::from_u128(7),
        addresses: vec!["100.64.0.2:9742".parse().unwrap()],
        node_pubkey: None,
    };
    let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
    assert!(
        endpoints.is_empty(),
        "no identity key → no iroh dial (a routed transport would fall back to IP)"
    );
}
