// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus sharing over iroh — the W3 proof that the corpus-sharing
//! traffic classes actually move bytes when routed over iroh, to a
//! peer with NO gossiped IP address (the no-VPN case).
//!
//! Two classes, both riding the `PeerTransport` seam:
//!   1. **KnowledgeSearch** — a client's `/v1/knowledge/search` on the
//!      joiner fans out to the founder's `/internal/knowledge/search`
//!      over an iroh tunnel and merges the founder's chunk.
//!   2. **ControlPlane** — `canonical_pull` fetches a full corpus
//!      canonical (tar+zstd) from the founder over the same iroh
//!      transport, resolved for the ControlPlane class.
//!
//! The founder is reachable ONLY by key: its `MemberRecord` in the
//! joiner's view carries `node_pubkey` + `iroh_direct_addrs` and an
//! EMPTY `addresses` — so a regression that dropped the no-IP peer
//! (the `is_dialable` generalization) or failed to route the class
//! over iroh would make both tests fail with an empty/absent result.
//!
//! Excluded from default workspace gates; run with:
//!   cargo test -p sovereign-mesh --features iroh-experimental \
//!       --test corpus_sharing_over_iroh_e2e
#![cfg(feature = "iroh-experimental")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use commonwealth_api::server::{client_router, internal_router};
use commonwealth_api::state::AppState;
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId, NodePubkey};
use commonwealth_core::knowledge::CorpusShardInfo;
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use commonwealth_transport::iroh::{EndpointBuilder, IrohAcceptor, IrohTransport, SecretKey, ALPN};
use commonwealth_transport::{
    IpTransport, PeerContact, PeerTransport, RoutedTransport, TrafficClass,
};
use corpus_engine::index::{CorpusIndex, EmbeddedChunk, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};

mod common;
use common::spawn_router;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// A hermetic iroh endpoint: no relays, no address-lookup (mirrors
/// `iroh_transport_e2e`).
async fn bind_empty_endpoint(seed: u8) -> commonwealth_transport::iroh::Endpoint {
    EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[seed; 32]))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("iroh endpoint bind")
}

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

async fn install_corpus(indexes_dir: &Path, id: &str, name: &str, content: &str) {
    let index = CorpusIndex::create(
        &indexes_dir.join(id),
        id,
        name,
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: content.into(),
                title: Some(name.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(id.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; EMBED_DIM],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
}

fn caps_with_hosted(corpora: &[&str]) -> NodeCapabilities {
    NodeCapabilities {
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
        hosted_corpora: corpora
            .iter()
            .map(|id| CorpusShardInfo {
                corpus_id: id.to_string(),
                chunk_range: None,
                is_replica: false,
                last_updated: 0,
                chunk_count: 1,
                canonical_fingerprint: None,
                total_shards: None,
                processed_shards: vec![],
                atlas_atom_count: 0,
                atlas_tier2_count: 0,
                atlas_fingerprint: None,
            })
            .collect(),
        reported_at: 0,
        inference_availability: 0.0,
        inference_capable: false,
        loaded_models: vec![],
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
    }
}

/// A member record reachable ONLY by iroh key (pubkey + direct addrs,
/// no IP) — the no-VPN peer the whole feature exists to serve.
fn iroh_only_member(
    id: NodeId,
    name: &str,
    pubkey: NodePubkey,
    direct_addrs: Vec<SocketAddr>,
    hosted: &[&str],
) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: Some(pubkey),
        relay_url: None,
        iroh_direct_addrs: direct_addrs,
        dial_info_version: 1,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities: caps_with_hosted(hosted),
        addresses: vec![], // NO IP — dialable only by key.
    }
}

/// Founder: a real CorpusEngine holding `corpus_id`, its internal
/// router served behind an iroh acceptor. Returns (endpoint pubkey,
/// dialable sockets, the acceptor guard) — the acceptor must stay
/// alive for the test's duration.
async fn spawn_iroh_founder(
    seed: u8,
    engine: Arc<CorpusEngine>,
    founder_id: NodeId,
    mesh: Mesh,
) -> (AppState, NodePubkey, Vec<SocketAddr>, IrohAcceptor) {
    let store = Arc::new(MeshStore::in_memory().unwrap());
    let state = AppState::new_with_platform_and_engine(
        founder_id,
        mesh,
        store,
        Arc::new(AppRegistry::new()),
        Some(engine),
    );

    // The founder's plain internal listener (loopback) — the iroh
    // acceptor forwards bi-streams here, exactly as the daemon does.
    // `spawn_router` wires connect_info so the loopback guard sees the
    // production listener shape.
    let router_addr = spawn_router(internal_router(state.clone())).await;

    let ep = bind_empty_endpoint(seed).await;
    let pubkey = NodePubkey(*ep.id().as_bytes());
    let sockets = dialable_sockets(&ep);
    assert!(!sockets.is_empty(), "founder must expose a dialable socket");
    let acceptor = IrohAcceptor::spawn(ep, router_addr);
    (state, pubkey, sockets, acceptor)
}

/// Install a `RoutedTransport` on `state` routing `class` over an
/// `IrohTransport` built on a fresh endpoint (IP fallback retained,
/// no required classes — the plaintext prefer-iroh posture).
async fn install_iroh_route(state: &AppState, class: TrafficClass, seed: u8) {
    let ep = bind_empty_endpoint(seed).await;
    let iroh_t: Arc<dyn PeerTransport> = Arc::new(IrohTransport::new(ep));
    let mut per_class = HashMap::new();
    per_class.insert(class, iroh_t);
    let ip: Arc<dyn PeerTransport> = Arc::new(IpTransport::new(9741));
    state.install_peer_transport(Arc::new(RoutedTransport::new(per_class, ip)));
}

#[tokio::test]
async fn knowledge_fanout_over_iroh_reaches_peer_with_no_ip() {
    let mesh_id = MeshId::from_u128(1);
    let mesh_name = "iroh-corpus-mesh";

    // === Founder (A): hosts the "sep" corpus, reachable only by key ===
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    install_corpus(
        &indexes_a,
        "sep",
        "Stanford Encyclopedia of Philosophy",
        "Compatibilism: free will is compatible with determinism.",
    )
    .await;
    let recipes_a = tmp_a.path().join("recipes");
    std::fs::create_dir_all(&recipes_a).unwrap();
    let engine_a = Arc::new(
        CorpusEngine::new(recipes_a, indexes_a, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let id_a = NodeId::from_u128(0xAAAA_AAAA_AAAA_AAAA);

    let mut members_a = HashMap::new();
    members_a.insert(id_a, {
        let mut m = iroh_only_member(id_a, "Founder", NodePubkey([0u8; 32]), vec![], &["sep"]);
        m.addresses = vec!["127.0.0.1:9742".parse().unwrap()];
        m
    });
    let mesh_a = Mesh {
        id: mesh_id,
        name: mesh_name.into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: members_a,
        peers: vec![],
    };
    let (state_a, a_pubkey, a_sockets, _acceptor) =
        spawn_iroh_founder(41, engine_a, id_a, mesh_a).await;

    // === Joiner (B): no corpora; sees A as an iroh-only peer ===
    let id_b = NodeId::from_u128(0xBBBB_BBBB_BBBB_BBBB);
    let mut members_b = HashMap::new();
    members_b.insert(id_b, {
        let mut m = iroh_only_member(id_b, "Joiner", NodePubkey([1u8; 32]), vec![], &[]);
        m.addresses = vec!["127.0.0.1:1".parse().unwrap()];
        m
    });
    members_b.insert(
        id_a,
        iroh_only_member(id_a, "Founder", a_pubkey, a_sockets, &["sep"]),
    );
    let mesh_b = Mesh {
        id: mesh_id,
        name: mesh_name.into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: members_b,
        peers: vec![],
    };
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let state_b = AppState::new_with_platform_and_engine(
        id_b,
        mesh_b,
        store_b,
        Arc::new(AppRegistry::new()),
        None,
    );
    install_iroh_route(&state_b, TrafficClass::KnowledgeSearch, 42).await;

    let addr_b = spawn_router(client_router(state_b.clone())).await;

    // === Client asks B; B must fan out to A over iroh ===
    let resp = reqwest::Client::new()
        .post(format!("http://{addr_b}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .send()
        .await
        .expect("/v1/knowledge/search reachable on B");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "body: {body}");
    let results = body["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "fan-out over iroh to a no-IP peer must surface A's chunk; empty means \
         the peer was dropped (is_dialable regression) or the class wasn't routed \
         over iroh. body: {body}"
    );
    assert!(
        results
            .iter()
            .all(|r| r["corpus_id"].as_str() == Some("sep")),
        "every result must come from 'sep': {body}"
    );

    // The founder actually served it (real round-trip, not an echo):
    // its live state is unchanged but the chunk content came back.
    assert!(
        results.iter().any(|r| r["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Compatibilism")),
        "A's chunk content must survive the tunnel round-trip: {body}"
    );
    let _ = &state_a; // keep A alive to end of test
}

#[tokio::test]
async fn canonical_pull_over_iroh_from_peer_with_no_ip() {
    use sovereign_mesh::canonical_pull::pull_canonical_from_peer;

    let mesh_id = MeshId::from_u128(2);
    let corpus_id = "canon";

    // === Founder (A): a real canonical, served behind an iroh acceptor ===
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    let idx = CorpusIndex::create(
        &indexes_a.join(corpus_id),
        corpus_id,
        "Canonical over iroh",
        "test-embed",
        4,
        true,
        "MIT",
    )
    .await
    .unwrap();
    idx.insert_chunks(&[EmbeddedChunk {
        insert: InsertChunk {
            content: "canonical chunk".into(),
            title: Some("doc".into()),
            url: None,
            metadata: None,
            content_hash: Some("hash-aaa".into()),
            source_doc_id: Some("hash-aaa".into()),
            source_file: None,
            code: Default::default(),
            unit_id: None,
        },
        embedding: vec![1.0, 0.0, 0.0, 0.0],
    }])
    .await
    .unwrap();
    idx.mark_ingestion_complete().unwrap();
    let fingerprint = idx.compute_and_stamp_fingerprint().await.unwrap();

    let zero_embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) }));
    let engine_a = Arc::new(
        CorpusEngine::new(indexes_a.clone(), indexes_a.clone(), zero_embed)
            .with_embedding_model("test-embed"),
    );
    let id_a = NodeId::from_u128(0xA1);
    let mesh_a = Mesh {
        id: mesh_id,
        name: "canon-mesh".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: Default::default(),
        peers: vec![],
    };
    let (_state_a, a_pubkey, a_sockets, _acceptor) =
        spawn_iroh_founder(43, engine_a, id_a, mesh_a).await;

    // === Puller (B): resolve A's ControlPlane endpoint over iroh, then
    // pull the canonical through the bridge base URL. A is iroh-only. ===
    let puller_ep = bind_empty_endpoint(44).await;
    let iroh_t = IrohTransport::new(puller_ep);
    let contact = PeerContact {
        node_id: id_a,
        addresses: vec![], // no IP — dial by key only
        node_pubkey: Some(a_pubkey),
        relay_url: None,
        iroh_direct_addrs: a_sockets,
    };
    let endpoints = iroh_t.endpoints(&contact, TrafficClass::ControlPlane).await;
    assert_eq!(
        endpoints.len(),
        1,
        "dial-by-key must yield exactly one bridge endpoint for the ControlPlane class"
    );
    let base = endpoints[0].base_url.clone();
    assert!(base.starts_with("http://127.0.0.1:"), "{base}");

    let tmp_b = tempfile::tempdir().unwrap();
    let dest = tmp_b.path().join("indexes");
    std::fs::create_dir_all(&dest).unwrap();
    let report = pull_canonical_from_peer(&[base], corpus_id, &dest, Some(fingerprint.as_str()))
        .await
        .expect("canonical pull over iroh must succeed");

    assert_eq!(report.corpus_id, corpus_id);
    assert!(
        dest.join(corpus_id).exists(),
        "the canonical must have landed on the puller after an iroh transfer"
    );
}
