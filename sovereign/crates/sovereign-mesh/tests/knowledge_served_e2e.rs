// SPDX-License-Identifier: AGPL-3.0-or-later
//! `KnowledgeQueryServed` ledger emission test.
//!
//! `routes_internal::knowledge_search` is the inter-node fan-out
//! target: peers POST a `KnowledgeSearchRequest` with an `X-Node-Id`
//! header identifying themselves, this daemon searches its
//! installed shards and emits **one `KnowledgeQueryServed` event
//! per corpus** that contributed at least one chunk.
//!
//! The contract (§10 of `SYSTEM_OVERVIEW.md`):
//!   - Local-origin requests (no `X-Node-Id`) skip emission. The
//!     dimensional ledger is intra-mesh-only.
//!   - Per-corpus chunk count is post-truncation — reflects what
//!     the requester actually sees, not the raw pre-merge size.
//!   - `for_node` is the requester (from the header), not the
//!     local node.
//!
//! Three cases worth pinning:
//!
//! 1. **Peer request → one event per contributing corpus.** Two
//!    corpora installed, both contribute → two events emitted with
//!    the right `for_node` + `corpus_id` + `chunks_returned`.
//! 2. **Local-origin request (no header) → no event.** Same
//!    request, no `X-Node-Id` → response succeeds, ledger stays
//!    empty.
//! 3. **Empty result → no event for the empty corpus.** Filter to
//!    a non-installed corpus → response has no results, no event.
//!
//! Pre-fix this contract was only readable in the route's comments;
//! no test would catch a regression that:
//! - Dropped the per-corpus emission loop entirely (silent ledger).
//! - Stamped the LOCAL node as `for_node` instead of the requester
//!   (lookup pollution).
//! - Emitted before truncation (over-counted under pressure).
use std::sync::Arc;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::NodeId;
use commonwealth_state::MeshStore;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};

mod common;
use common::{id_to_hex, solo_mesh, spawn_router};

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Install a corpus at `<indexes>/<id>` with one chunk pinned to a
/// known content string. Returns once `mark_ingestion_complete`
/// has been called, so the engine's `installed_indexes()` reports
/// it as present.
async fn install_corpus_with_chunk(
    indexes_dir: &std::path::Path,
    id: &str,
    name: &str,
    chunk_content: &str,
) {
    let path = indexes_dir.join(id);
    let index = CorpusIndex::create(
        &path,
        id,
        name,
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: chunk_content.into(),
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

/// Build an `AppState` with a `CorpusEngine` rooted at `tmp/indexes/`
/// and pre-installed corpora. Returns the state and the on-disk
/// directory (keep alive for the test's duration).
async fn build_state_with_corpora(
    self_id: NodeId,
    corpora: &[(&str, &str, &str)], // (id, name, chunk_content)
) -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    for (id, name, content) in corpora {
        install_corpus_with_chunk(&indexes, id, name, content).await;
    }
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state = AppState::new_with_platform_and_engine(
        self_id,
        solo_mesh(self_id, "knowledge-served-test"),
        mesh_store,
        app_registry,
        Some(engine),
    );
    (state, tmp)
}

#[tokio::test]
async fn peer_request_emits_one_knowledge_query_served_per_contributing_corpus() {
    let self_id = NodeId::from_u128(0xAAAA_AAAA);
    let requester = NodeId::from_u128(0xBBBB_BBBB);
    let (state, _tmp) = build_state_with_corpora(
        self_id,
        &[
            ("sep", "Stanford Encyclopedia", "Free will and determinism."),
            ("wikipedia", "Wikipedia", "Article about compatibilism."),
        ],
    )
    .await;
    let addr = spawn_router(internal_router(state.clone())).await;

    // Request both corpora — both should contribute one chunk each.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/knowledge/search"))
        .header("X-Node-Id", id_to_hex(&requester))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep", "wikipedia"],
            "limit": 10,
        }))
        .send()
        .await
        .expect("/internal/knowledge/search reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().expect("results is an array");
    assert_eq!(
        results.len(),
        2,
        "both corpora should each contribute one chunk; got: {body}"
    );

    // The ledger should now have exactly two `KnowledgeQueryServed`
    // events, one per contributing corpus, both stamped with
    // `for_node = requester`.
    let events = state
        .inner
        .contribution_emitter
        .events()
        .expect("emitter.events() reads");
    let served: Vec<(NodeId, String, u32)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::KnowledgeQueryServed {
                for_node,
                corpus_id,
                chunks_returned,
            } => Some((*for_node, corpus_id.clone(), *chunks_returned)),
            _ => None,
        })
        .collect();

    assert_eq!(
        served.len(),
        2,
        "one event per contributing corpus expected; got: {served:?}"
    );
    for (for_node, corpus_id, chunks) in &served {
        assert_eq!(
            for_node, &requester,
            "for_node must be the requester (from X-Node-Id), \
             not the local node — a §10 lookup-pollution regression"
        );
        assert!(
            corpus_id == "sep" || corpus_id == "wikipedia",
            "unexpected corpus_id in event: {corpus_id}"
        );
        assert_eq!(*chunks, 1, "each corpus contributed exactly one chunk");
    }
}

#[tokio::test]
async fn local_origin_request_with_no_x_node_id_emits_nothing() {
    // Same request, no `X-Node-Id` header. The route serves the
    // search results but must NOT record any ledger event — §10
    // promises intra-mesh-only accounting, and a missing header
    // means "I can't tell who you are" → safe-default skip.
    let self_id = NodeId::from_u128(0xCCCC_CCCC);
    let (state, _tmp) = build_state_with_corpora(
        self_id,
        &[("sep", "Stanford Encyclopedia", "Compatibilism essay.")],
    )
    .await;
    let addr = spawn_router(internal_router(state.clone())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/knowledge/search"))
        // intentionally no `X-Node-Id`
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["results"].as_array().unwrap().len(),
        1,
        "the search still serves results; only the ledger emission is gated"
    );

    let events = state.inner.contribution_emitter.events().unwrap();
    let served_count = events
        .iter()
        .filter(|e| matches!(e.kind, LedgerEventKind::KnowledgeQueryServed { .. }))
        .count();
    assert_eq!(
        served_count, 0,
        "no `X-Node-Id` header → no KnowledgeQueryServed events. \
         Got events: {events:?}"
    );
}

#[tokio::test]
async fn unavailable_corpus_filter_emits_no_event_and_lists_unavailable() {
    // Caller asks for a corpus we don't host. The route returns 200
    // with `corpora_unavailable` listing it, no chunks served, no
    // event emitted (zero chunks → no entry in per_corpus_chunks).
    let self_id = NodeId::from_u128(0xDDDD_DDDD);
    let requester = NodeId::from_u128(0xEEEE_EEEE);
    let (state, _tmp) = build_state_with_corpora(
        self_id,
        &[("sep", "Stanford Encyclopedia", "Some content.")],
    )
    .await;
    let addr = spawn_router(internal_router(state.clone())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/knowledge/search"))
        .header("X-Node-Id", id_to_hex(&requester))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "anything",
            "corpora": ["not-hosted-here"],
            "limit": 10,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let unavailable: Vec<&str> = body["corpora_unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        unavailable.contains(&"not-hosted-here"),
        "the route must report the unhosted corpus in `corpora_unavailable`; got {body}"
    );

    let events = state.inner.contribution_emitter.events().unwrap();
    let served_count = events
        .iter()
        .filter(|e| matches!(e.kind, LedgerEventKind::KnowledgeQueryServed { .. }))
        .count();
    assert_eq!(
        served_count, 0,
        "zero chunks served → zero KnowledgeQueryServed events. \
         A regression that emits per-corpus regardless of chunk count \
         would over-credit the local node for serving empty searches."
    );
}
