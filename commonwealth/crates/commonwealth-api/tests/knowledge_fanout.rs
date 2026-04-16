//! End-to-end test for `/v1/knowledge/search` fan-out.
//!
//! Builds two `AppState`s: *Host* owns a tiny real `CorpusEngine`
//! with one chunk in a corpus called `"sep"`; *Joiner* has no corpora
//! but gossip-knows Host exists and hosts `sep`. We spin a real
//! `internal_router` for Host on an ephemeral port so the fan-out's
//! reqwest call in `routes_knowledge` has a live socket to POST to.
//!
//! The test proves the whole feature in one shot: Joiner queries its
//! own `/v1/knowledge/search`, the handler walks `hosted_corpora`,
//! fires `/internal/knowledge/search` at Host, gets the SEP chunk
//! back, and surfaces it with `metadata["peer_name"] = "Host"` so
//! the UI can render `sep (1) via Host`. Also exercises the
//! resilience path — an offline peer must not tank the query.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use commonwealth_api::server::{client_router, internal_router};
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{
    AvailableResources, HardwareProfile, NodeCapabilities,
};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::knowledge::CorpusShardInfo;
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use commonwealth_app::registry::AppRegistry;
use corpus_engine::{CorpusEngine, EmbedFn};
use corpus_engine::index::{CorpusIndex, InsertChunk};
use tower::ServiceExt;

/// 8-dim zero vector — matches what mock-embed-backed indexes ship
/// with throughout the corpus-engine test suite.
fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }))
}

/// Construct a CorpusEngine rooted in `dir` with a single installed
/// corpus `corpus_id` holding `chunks`. Completes ingestion so
/// `installed_indexes()` returns it.
async fn make_engine_with_corpus(
    dir: &std::path::Path,
    corpus_id: &str,
    chunks: Vec<InsertChunk>,
) -> Arc<CorpusEngine> {
    let recipes = dir.join("recipes");
    let indexes = dir.join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();

    let idx_path = indexes.join(corpus_id);
    let index = CorpusIndex::create(
        &idx_path,
        corpus_id,
        corpus_id,          // corpus_name
        "nomic-embed-text-v2", // embedding_model — must match what
                               // the engine will look for when it
                               // opens the index via `open_index`.
        8,
        true,      // mesh_sharing
        "CC0-1.0",
    )
    .await
    .unwrap();

    let pairs: Vec<(InsertChunk, Vec<f32>)> =
        chunks.into_iter().map(|c| (c, vec![0.0_f32; 8])).collect();
    index.insert_batch(&pairs).await.unwrap();
    // Without this, `installed_indexes()` silently skips the corpus
    // as "partial / ingestion not completed" and our fan-out has
    // nothing to serve — silent mode of the test you'd debug for
    // an hour.
    index.mark_ingestion_complete().unwrap();

    Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("nomic-embed-text-v2"),
    )
}

/// Start a real TCP internal_router for `state`. Returns the bound
/// address; the server task is leaked (lives for the test duration).
async fn spawn_internal_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = internal_router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // Give tokio a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// Build a `MemberRecord` with the given id, name, status, address,
/// and `hosted_corpora`. Everything else is zero/default.
fn member(
    id: NodeId,
    name: &str,
    status: NodeStatus,
    addr: SocketAddr,
    hosted: Vec<String>,
) -> MemberRecord {
    MemberRecord {
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 1_000,
        status,
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
            hosted_corpora: hosted
                .into_iter()
                .map(|corpus_id| CorpusShardInfo {
                    corpus_id,
                    chunk_range: None,
                    is_replica: false,
                    last_updated: 1_000,
                })
                .collect(),
            reported_at: 1_000,
        },
        addresses: vec![addr],
    }
}

/// Build an `AppState` around `node_id`, optionally attached to a
/// `CorpusEngine` and populated with a two-member `Mesh` that
/// includes `peer` as an online member with `hosted_corpora`.
fn make_state(
    node_id: NodeId,
    peer: MemberRecord,
    engine: Option<Arc<CorpusEngine>>,
) -> AppState {
    let mesh_id = MeshId::from_u128(42);
    let hash = [7u8; 32];
    // Include self — otherwise the fan-out logic can't tell which
    // member is "us" and which are peers.
    let self_record = member(
        node_id,
        "Self",
        NodeStatus::Online,
        "127.0.0.1:0".parse().unwrap(),
        vec![],
    );
    let mut members = HashMap::new();
    members.insert(node_id, self_record);
    members.insert(peer.node_id, peer);
    let mesh = Mesh {
        id: mesh_id,
        name: "Test".into(),
        join_key_hash: hash,
        members,
        peers: vec![],
    };

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    AppState::new_with_platform_and_engine(
        node_id,
        mesh,
        mesh_store,
        app_registry,
        engine,
    )
}

/// Issue a `/v1/knowledge/search` POST against `state` via
/// `tower::oneshot`. Fan-out inside the handler still speaks real
/// HTTP over reqwest to the Host's bound port — this is only the
/// test-side transport for the caller.
async fn post_knowledge_search(
    state: AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let app = client_router(state);
    let response = app
        .oneshot(
            Request::post("/v1/knowledge/search")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn fanout_fetches_sep_chunk_from_peer_with_attribution() {
    // ── Host ("BeefyMac") — owns a real `sep` corpus ──────────
    let host_dir = tempfile::tempdir().unwrap();
    let host_engine = make_engine_with_corpus(
        host_dir.path(),
        "sep",
        vec![InsertChunk {
            content:
                "Compatibilists hold that free will is compatible with \
                 determinism, reinterpreting 'freedom' as responsiveness \
                 to reasons rather than the power to do otherwise."
                    .into(),
            title: Some("Compatibilism".into()),
            url: Some("https://plato.stanford.edu/entries/compatibilism/".into()),
            metadata: None,
            content_hash: None,
            source_doc_id: Some("compatibilism".into()),
            source_file: None,
            code: Default::default(),
        }],
    )
    .await;
    let host_id = NodeId::from_u128(100);
    // Give Host the same `AppState` shape it would have in prod.
    // Peer field here is the Joiner but Host won't fan out to it in
    // this test — Joiner sends the request, not Host.
    let host_joiner_peer = member(
        NodeId::from_u128(200),
        "Joiner",
        NodeStatus::Online,
        "127.0.0.1:1".parse().unwrap(),
        vec![],
    );
    let host_state = make_state(host_id, host_joiner_peer, Some(host_engine));
    let host_addr = spawn_internal_router(host_state).await;

    // ── Joiner ("LittleMac") — no corpora ─────────────────────
    let joiner_id = NodeId::from_u128(200);
    let host_in_joiner_view = member(
        host_id,
        "BeefyMac",
        NodeStatus::Online,
        host_addr,
        vec!["sep".into()],
    );
    let joiner_state = make_state(joiner_id, host_in_joiner_view, None);

    // Query: free-will question, embedding = 8-zero vector (matches
    // index dim). The FTS path picks up "determinism" / "free will"
    // regardless of vector similarity — we just need a non-empty hit.
    let request = serde_json::json!({
        "query_embedding": vec![0.0_f32; 8],
        "query_text": "is free will compatible with determinism?",
        "limit": 8,
    });
    let (status, body) = post_knowledge_search(joiner_state, request).await;
    assert_eq!(status, StatusCode::OK, "fan-out response must be 200");

    let results = body["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "fan-out should have returned SEP chunk, got empty: {body:?}"
    );
    let first = &results[0];
    assert_eq!(first["corpus_id"], "sep", "hit must be from the sep corpus");
    assert_eq!(
        first["metadata"]["peer_name"],
        "BeefyMac",
        "peer attribution must survive fan-out: {first:?}"
    );
    assert_eq!(
        body["corpora_searched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "sep"),
        true
    );
}

#[tokio::test]
async fn fanout_survives_offline_peer() {
    // The Joiner has a peer record pointing at an address where no
    // server is listening. Our handler must treat the attempt as a
    // transport error, mark the corpus unavailable, and return 200
    // with no results — not 5xx.
    let joiner_id = NodeId::from_u128(300);
    let dead_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let dead_peer = member(
        NodeId::from_u128(301),
        "ZombieFounder",
        NodeStatus::Online,
        dead_addr,
        vec!["sep".into()],
    );
    let joiner_state = make_state(joiner_id, dead_peer, None);

    let request = serde_json::json!({
        "query_embedding": vec![0.0_f32; 8],
        "query_text": "anything",
        "limit": 4,
    });
    let (status, body) = post_knowledge_search(joiner_state, request).await;
    assert_eq!(status, StatusCode::OK, "offline peer must not 5xx");
    assert!(
        body["results"].as_array().unwrap().is_empty(),
        "no results available"
    );
    let unavailable = body["corpora_unavailable"].as_array().unwrap();
    assert!(
        unavailable.iter().any(|c| c == "sep"),
        "sep should be reported unavailable, got {unavailable:?}"
    );
}
