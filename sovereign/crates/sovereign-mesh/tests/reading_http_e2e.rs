// SPDX-License-Identifier: AGPL-3.0-or-later
//! `reading_http` chunk-fetch test.
//!
//! The reading surface (`reading_http::reading_router`) powers the
//! desktop's glass-box reading inspector: GETs a chunk by id, GETs
//! its neighbor window, and (for atlas-enriched corpora) GETs an
//! atom card by id. These endpoints are loopback-only and read
//! through the daemon's `CorpusEngine`.
//!
//! Pre-fix integration coverage was zero — the 1000+ LOC route
//! file relied entirely on review for correctness, including the
//! corpus-not-found / chunk-not-found error paths that determine
//! whether the desktop renders an "missing" state vs a 500.
//!
//! Four cases:
//!
//! 1. **Happy path: chunk fetch returns the inserted content.**
//!    GET on an installed corpus + valid `chunk_id` → 200 with
//!    the chunk's text in the response body.
//! 2. **Missing corpus → 404.** A typo or pre-install state must
//!    surface `corpus open: …` rather than a 500 — the desktop
//!    inspector branches on the status code.
//! 3. **Valid corpus, missing chunk_id → 404.** Same shape;
//!    `chunk not found` distinguishes the error class.
//! 4. **Neighbors window returns center + (empty) prev/next.**
//!    For a one-chunk corpus the window has just the center.
//!    Pins the JSON shape so the inspector's renderer doesn't
//!    drift.
use std::sync::Arc;

use commonwealth_state::MeshStore;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::reading_http::reading_router;

mod common;
use common::spawn_router;
use sovereign_core::setup_config::SetupConfig;

const EMBED_DIM: usize = 8;
const CHUNK_TEXT: &str = "Compatibilism: free will is compatible with determinism.";

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Returns the chunk_id of the inserted row. `CorpusIndex` assigns
/// these sequentially starting from 0; we re-read it to avoid
/// hardcoding an implementation detail that could shift.
async fn install_one_chunk_corpus(
    indexes_dir: &std::path::Path,
    id: &str,
    name: &str,
    content: &str,
) -> u64 {
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
    // The first row's chunk_id is whatever `insert_batch` assigned.
    // Read the index back to discover it rather than guess.
    let rows = index.chunks_by_ids(&[0]).await.unwrap();
    if !rows.is_empty() {
        // chunk_id 0 worked — that's the canonical "first row" id.
        return 0;
    }
    // Fall back to scanning the small set; ours has one row.
    // (CorpusIndex doesn't expose `all_chunk_ids` directly, but a
    // search with a zero vector returns rows ordered by relevance
    // and surfaces their ids.)
    let results = index.search(&[0.0_f32; EMBED_DIM], "", 1).await.unwrap();
    results[0].chunk_id.unwrap_or(0)
}

/// Build a daemon backed by a CorpusEngine with one corpus, one
/// chunk. Returns the daemon, its router-mountable Arc, the tmp
/// dir (held to prevent cleanup), and the row's `chunk_id`.
async fn build_reading_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let chunk_id = install_one_chunk_corpus(&indexes, "sep", "SEP", CHUNK_TEXT).await;

    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );
    // The reading router reads `daemon.corpus_engine()` — we don't
    // need to fire up `create_mesh` because that path only matters
    // for routes that consult AppState (which reading_http does
    // not).
    let _ = MeshStore::in_memory(); // silence the unused-import lint on macOS minimal-feature builds.
    (daemon, tmp, chunk_id)
}

#[tokio::test]
async fn get_chunk_returns_inserted_content() {
    let (daemon, _tmp, chunk_id) = build_reading_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "http://{addr}/internal/corpus/sep/chunks/{chunk_id}"
        ))
        .send()
        .await
        .expect("reading_router reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "GET /internal/corpus/sep/chunks/{chunk_id} should return 200"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let content = body["content"]
        .as_str()
        .or_else(|| body["text"].as_str())
        .or_else(|| body["chunk"]["content"].as_str())
        .unwrap_or("");
    assert!(
        content.contains("Compatibilism") || content.contains("compatibilism"),
        "response body must carry the inserted chunk content; got: {body}"
    );
}

#[tokio::test]
async fn get_chunk_returns_404_for_unknown_corpus() {
    let (daemon, _tmp, _chunk_id) = build_reading_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "http://{addr}/internal/corpus/nonexistent/chunks/0"
        ))
        .send()
        .await
        .expect("reading_router reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "unknown corpus must surface 404, not a 500 or 503 — \
         the desktop inspector branches on the status code"
    );
}

#[tokio::test]
async fn get_chunk_returns_404_for_unknown_chunk_id() {
    let (daemon, _tmp, _chunk_id) = build_reading_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    // chunk_id well past anything we inserted.
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/corpus/sep/chunks/9999999"))
        .send()
        .await
        .expect("reading_router reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "missing chunk_id within a valid corpus must surface 404"
    );
}

#[tokio::test]
async fn get_neighbors_returns_center_with_empty_prev_and_next_for_single_chunk_corpus() {
    let (daemon, _tmp, chunk_id) = build_reading_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "http://{addr}/internal/corpus/sep/chunks/{chunk_id}/neighbors?radius=2"
        ))
        .send()
        .await
        .expect("reading_router reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    // The handler returns a `NeighborWindowResponse { center, prev, next, .. }`.
    // For a single-chunk corpus, prev + next are both empty.
    assert!(
        body["center"].is_object() || body.get("center").is_some(),
        "neighbors response must carry a center chunk: {body}"
    );
    let prev = body["prev"].as_array().unwrap_or(&Vec::new()).clone();
    let next = body["next"].as_array().unwrap_or(&Vec::new()).clone();
    assert!(
        prev.is_empty(),
        "single-chunk corpus → empty prev; got {prev:?}"
    );
    assert!(
        next.is_empty(),
        "single-chunk corpus → empty next; got {next:?}"
    );
}

/// A corpus whose ingest completed but whose promotion to canonical never
/// landed must NOT read as "chunk absent".
///
/// This is the regression test for the fault the real-mode e2e suite caught on
/// 2026-07-27. `promote_single_shard` refuses a canonical dir that already
/// holds a colliding entry (observed live: an `atlas/` directory written before
/// promote ran), so all the Lance data stays in `<corpus>-partition-*` and the
/// canonical dir never gets `_corpus_meta.json`. Retrieval still finds the
/// chunks — `installed_indexes()` enumerates directories and keys off each
/// meta's `corpus_id` — so search looks healthy while every citation into the
/// corpus fails to dereference.
///
/// It stayed invisible because this router answered 404 for the open failure and
/// the desktop's `daemon_reading_get` maps ANY 404 to `Ok(None)` -> `null`. In
/// Attach mode, which is the mode every shipped desktop runs, a structurally
/// broken index was therefore indistinguishable from a missing chunk and the
/// real error text never reached the desktop, its logs, or this suite. So the
/// assertion that matters is the STATUS CLASS (5xx, not 404) plus the diagnosis
/// actually naming the un-promoted partition.
#[tokio::test]
async fn get_chunk_reports_unpromoted_partition_instead_of_masquerading_as_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();

    // The completed ingest, still sitting in its partition dir.
    install_one_chunk_corpus(&indexes, "sep-partition-node-aaaa", "SEP", CHUNK_TEXT).await;
    // The canonical dir, pre-populated with the entry that made promote refuse
    // and WITHOUT `_corpus_meta.json` — exactly the shape observed on disk.
    std::fs::create_dir_all(indexes.join("sep").join("atlas")).unwrap();

    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );

    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/corpus/sep/chunks/0"))
        .send()
        .await
        .expect("reading_router reachable");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap_or("");

    assert_ne!(
        status,
        reqwest::StatusCode::NOT_FOUND,
        "an un-promoted partition is an infrastructure fault, not an absent \
         chunk — 404 here is what the desktop silently turns into `null`. Body: {body}"
    );
    assert!(
        status.is_server_error(),
        "expected a 5xx so the desktop surfaces the message; got {status}. Body: {body}"
    );
    assert!(
        err.contains("un-promoted"),
        "the diagnosis must name the actual fault so an operator can act on it; got: {err}"
    );
    assert!(
        err.contains("sep-partition-node-aaaa"),
        "the diagnosis must name WHERE the stranded data is; got: {err}"
    );
}
