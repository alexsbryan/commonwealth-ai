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
        // This test seeds the dial addrs via `add_known_peer` (the
        // fallback merge), so the contact itself carries none.
        relay_url: None,
        iroh_direct_addrs: vec![],
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

/// Track M building blocks: the phone-side `HttpBridge` dialing a
/// host-side `IrohAcceptor` on the client ALPN, with the dial info
/// round-tripped through the pairing string format — i.e. the exact
/// path `sovereign-mobile` ⇆ `sovereign-server` uses, minus the
/// model bootstrap.
#[tokio::test]
async fn http_bridge_reaches_acceptor_via_pairing_string() {
    use commonwealth_transport::iroh::{
        format_dial_string, parse_dial_string, HttpBridge, CLIENT_ALPN,
    };

    // "Host": a plain axum router (stands in for sovereign-server's
    // HTTP listener) + an acceptor on the client ALPN.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/status",
        axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let host_ep = EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[7; 32]))
        .alpns(vec![CLIENT_ALPN.to_vec()])
        .bind()
        .await
        .expect("host endpoint");
    let _acceptor =
        commonwealth_transport::iroh::IrohAcceptor::spawn(host_ep.clone(), http_addr);

    // Pairing string, exactly as the host's /status would render it
    // (hermetic test: direct UDP sockets instead of a relay URL).
    let mut addr = host_ep.addr();
    addr.addrs.clear();
    for s in dialable_sockets(&host_ep) {
        addr = addr.with_ip_addr(s);
    }
    let dial = format_dial_string(&addr).expect("host has dialable sockets");

    // "Phone": parse the pairing string, bridge, plain reqwest.
    let phone_ep = EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[8; 32]))
        .bind()
        .await
        .expect("phone endpoint");
    let target = parse_dial_string(&dial).expect("pairing string parses");
    let bridge = HttpBridge::spawn(phone_ep, target, CLIENT_ALPN)
        .await
        .expect("bridge spawns");

    let resp = reqwest::Client::new()
        .get(format!("http://{}/status", bridge.local_addr()))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("GET /status through the bridge");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[test]
fn dial_string_parse_rejects_malformed() {
    use commonwealth_transport::iroh::parse_dial_string;
    // No '@'.
    assert!(parse_dial_string("deadbeef").is_err());
    // Bad hex / wrong length.
    assert!(parse_dial_string("zz@127.0.0.1:1").is_err());
    assert!(parse_dial_string("deadbeef@127.0.0.1:1").is_err());
    // No targets.
    assert!(parse_dial_string(&format!("{}@", "ab".repeat(32))).is_err());
    // Garbage target.
    assert!(parse_dial_string(&format!("{}@not a url", "ab".repeat(32))).is_err());
}

#[tokio::test]
async fn peer_without_pubkey_is_not_dialable_on_iroh() {
    let client_ep = bind_iroh_endpoint(3).await;
    let transport = IrohTransport::new(client_ep);
    let contact = PeerContact {
        node_id: NodeId::from_u128(7),
        addresses: vec!["100.64.0.2:9742".parse().unwrap()],
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
    };
    let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
    assert!(
        endpoints.is_empty(),
        "no identity key → no iroh dial (a routed transport would fall back to IP)"
    );
}

/// W2 keystone: a peer is dialable purely from the dial info its
/// `MemberRecord` gossiped — here, the direct addrs carried in the
/// `PeerContact` — with NO `add_known_peer` seeding. "Knowing a member
/// record is sufficient to dial it." Also asserts the negative: a key
/// with no relay and no addrs has no path, so it's not dialable (a
/// routed composition falls back to IP).
#[tokio::test]
async fn iroh_transport_dials_from_contact_dial_info() {
    // Host: a plain axum router behind an iroh acceptor (internal ALPN,
    // which the Gossip traffic class selects).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/status",
        axum::routing::get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let host_ep = bind_iroh_endpoint(21).await;
    let host_pubkey = NodePubkey(*host_ep.id().as_bytes());
    let host_socks = dialable_sockets(&host_ep);
    assert!(!host_socks.is_empty(), "host must expose a dialable socket");
    let _acceptor = IrohAcceptor::spawn(host_ep.clone(), http_addr);

    // Client transport — dial info comes ONLY from the contact, never
    // from add_known_peer.
    let transport = IrohTransport::new(bind_iroh_endpoint(22).await);
    let contact = PeerContact {
        node_id: NodeId::from_u128(21),
        addresses: vec![],
        node_pubkey: Some(host_pubkey),
        relay_url: None,
        iroh_direct_addrs: host_socks,
    };
    let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
    assert_eq!(
        endpoints.len(),
        1,
        "a contact carrying direct addrs is dialable with no seeding"
    );
    let resp = reqwest::Client::new()
        .get(format!("{}/status", endpoints[0].base_url))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("GET /status over iroh, dialed purely from contact dial info");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);

    // Negative: a key with NO relay and NO addrs has no path. Use a
    // FRESH transport + a distinct (real) key so the prior dial's
    // per-(peer,ALPN) bridge cache can't mask the result.
    let other_ep = bind_iroh_endpoint(24).await;
    let other_pubkey = NodePubkey(*other_ep.id().as_bytes());
    let fresh = IrohTransport::new(bind_iroh_endpoint(23).await);
    let no_path = PeerContact {
        node_id: NodeId::from_u128(24),
        addresses: vec![],
        node_pubkey: Some(other_pubkey),
        relay_url: None,
        iroh_direct_addrs: vec![],
    };
    assert!(
        fresh
            .endpoints(&no_path, TrafficClass::Gossip)
            .await
            .is_empty(),
        "a bare key with no relay/addr is not dialable"
    );
}

/// W3 keystone: a `RoutedTransport` with Gossip flipped to iroh lists
/// the iroh candidate FIRST and the IP candidate AFTER (automatic
/// per-dial fallback), and the iroh-first candidate actually serves.
/// A peer with no iroh path degrades to the IP candidate alone — the
/// "a failed/absent iroh dial degrades to the tailnet path" guarantee,
/// proven end to end over real iroh QUIC + a real `IpTransport`.
#[tokio::test]
async fn routed_transport_prefers_iroh_then_falls_back_to_ip() {
    use commonwealth_transport::{IpTransport, RoutedTransport};
    use std::collections::HashMap;
    use std::sync::Arc;

    // A host reachable over iroh (internal ALPN, which Gossip selects).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/x",
        axum::routing::get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let host_ep = bind_iroh_endpoint(31).await;
    let host_pubkey = NodePubkey(*host_ep.id().as_bytes());
    let host_socks = dialable_sockets(&host_ep);
    let _acceptor = IrohAcceptor::spawn(host_ep.clone(), http_addr);

    // RoutedTransport: Gossip → iroh (dialing from a client endpoint);
    // default → IP.
    let iroh_t: Arc<dyn PeerTransport> = Arc::new(IrohTransport::new(bind_iroh_endpoint(32).await));
    let ip_t: Arc<dyn PeerTransport> = Arc::new(IpTransport::new(9742));
    let mut per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>> = HashMap::new();
    per_class.insert(TrafficClass::Gossip, iroh_t);
    let routed = RoutedTransport::new(per_class, ip_t);

    // Peer reachable BOTH ways: iroh (pubkey + direct addrs) and IP.
    // Routed must list iroh first, IP as fallback.
    let dual = PeerContact {
        node_id: NodeId::from_u128(31),
        addresses: vec!["100.64.0.9:9742".parse().unwrap()],
        node_pubkey: Some(host_pubkey),
        relay_url: None,
        iroh_direct_addrs: host_socks,
    };
    let eps = routed.endpoints(&dual, TrafficClass::Gossip).await;
    assert!(eps.len() >= 2, "iroh candidate + IP fallback, got {eps:?}");
    assert!(
        eps[0].label.starts_with("iroh:"),
        "iroh candidate must be first, got {}",
        eps[0].label
    );
    assert!(
        eps.iter().any(|e| e.label.starts_with("ip:")),
        "IP fallback candidate must be present"
    );
    // The iroh-first candidate actually serves a request.
    let resp = reqwest::Client::new()
        .get(format!("{}/x", eps[0].base_url))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("GET over the iroh-first routed candidate");
    assert!(resp.status().is_success());
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

    // Peer with NO iroh path (no pubkey) but an IP address → iroh
    // yields nothing, routed degrades to the IP candidate alone.
    let ip_only = PeerContact {
        node_id: NodeId::from_u128(99),
        addresses: vec!["100.64.0.10:9742".parse().unwrap()],
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: vec![],
    };
    let eps2 = routed.endpoints(&ip_only, TrafficClass::Gossip).await;
    assert_eq!(eps2.len(), 1, "no iroh path → IP fallback only, got {eps2:?}");
    assert!(
        eps2[0].label.starts_with("ip:"),
        "fallback candidate must be the IP one, got {}",
        eps2[0].label
    );
}
