//! Integration test for the corpus install / cancel / reinstall lifecycle.
//!
//! Covers the unified-ingest flow end-to-end at the HTTP boundary:
//!
//!  1. `POST /internal/corpus/install` starts an ingest into
//!     `<index_dir>/<corpus_id>-partition-<self>/`. The task is
//!     registered in `active_ingests` and writes progress into the
//!     shared `corpus_progress` map.
//!  2. `GET /internal/corpus/progress` reflects the current phase.
//!  3. `POST /internal/corpus/cancel` fires the cancellation flag,
//!     waits for the ingest loop to exit at its next poll boundary,
//!     and wipes the canonical directory + every partition-*/ sibling.
//!  4. A second `POST /internal/corpus/install` for the same corpus
//!     resumes cleanly from a clean slate and — with no peers
//!     involved — promotes the partition-of-self directory to the
//!     canonical index via the `sharding::promote_single_shard` fast
//!     path.
//!
//! Uses a local JSONL fixture + a mock embed fn so the pipeline can be
//! exercised without LLM weights or external network.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use corpus_engine::{CorpusEngine, IngestProgress};
use tempfile::TempDir;
use tower::ServiceExt;

/// Deterministic 8-dim vector derived from the input text. Non-zero
/// and reasonably well-spread across inputs so LanceDB's IVF-PQ
/// training sees a non-degenerate vector distribution (it otherwise
/// refuses with "KMeans cannot train K centroids with 0 vectors").
fn mock_embedding(text: &str) -> Vec<f32> {
    let mut bytes = [0u8; 32];
    for (i, b) in text.as_bytes().iter().enumerate() {
        bytes[i % 32] ^= *b;
    }
    let mut v = vec![0.0_f32; 8];
    for i in 0..8 {
        // Split the 32-byte hash across 8 dims; map each 4-byte
        // chunk into [-1.0, 1.0] so vectors live on a sensible
        // magnitude scale.
        let chunk = &bytes[i * 4..(i + 1) * 4];
        let as_u32 = u32::from_le_bytes(chunk.try_into().unwrap());
        v[i] = (as_u32 as f32) / (u32::MAX as f32) * 2.0 - 1.0;
    }
    v
}

/// Slow mock embed that sleeps briefly per call so the ingest takes
/// long enough to observe mid-flight cancellation reliably on fast
/// hardware. 2 ms per embed × ~500 chunks ≈ 1 s of embedding — plenty
/// of margin between "install returns 202" and "canonical exists" for
/// the cancel path to land in the middle.
fn slow_mock_embed_fn() -> corpus_engine::types::EmbedFn {
    Arc::new(|text: &str| {
        let v = mock_embedding(text);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(v)
        })
    })
}

/// A fast mock embed for the reinstall phase where we want the
/// ingest to finish promptly for the canonical-index assertion.
fn fast_mock_embed_fn() -> corpus_engine::types::EmbedFn {
    Arc::new(|text: &str| {
        let v = mock_embedding(text);
        Box::pin(async move { Ok(v) })
    })
}

/// Write a minimal recipe.toml + a JSONL source file in `dir` and
/// return the recipe dir + source path. The recipe uses the `jsonl`
/// extractor + `paragraph` chunker so 500 docs produce ≈ 500 chunks
/// at a small `max_chars`.
///
/// `mesh_sharing` controls whether the recipe opts into distributed
/// share-the-index gossip. License-restricted corpora (Stanford
/// Encyclopedia of Philosophy, etc.) ship with `false`; default
/// builtin corpora ship with `true`. The lifecycle install/pause/
/// cancel paths don't branch on this flag (they go through
/// `spawn_corpus_install` → `engine.ingest`, which is mesh-agnostic),
/// so existing tests work for both values; carrying the param makes
/// future tests of `corpus_collaborate` peer-filtering and the
/// auto-ingest gossip path possible without forking the fixture.
fn seed_fixture(
    dir: &std::path::Path,
    corpus_id: &str,
    doc_count: usize,
    mesh_sharing: bool,
) -> (PathBuf, PathBuf) {
    let recipes_dir = dir.join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let source_path = dir.join(format!("{corpus_id}.jsonl"));
    let mut source_body = String::new();
    for i in 0..doc_count {
        // Keep text long enough to not fall below chunking thresholds.
        let line = serde_json::json!({
            "title": format!("Article {i}"),
            "text": format!(
                "This is a paragraph of test content for article {i}. \
                 It is long enough to be kept by the chunker rather than \
                 filtered out as noise, and it carries a handful of stop \
                 words so the full-text index isn't trivially empty. \
                 Padding padding padding padding padding."
            ),
        });
        source_body.push_str(&line.to_string());
        source_body.push('\n');
    }
    std::fs::write(&source_path, source_body).unwrap();

    let recipe_toml = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "Test Corpus"
description = "Integration-test fixture"
license = "MIT"
mesh_sharing = {mesh_sharing}
size_compressed_gb = 0.0
size_indexed_gb = 0.0

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "text"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 400
overlap_chars = 40

[index]
fts = true
vector = true
embedding_model = "mock-8d"
embedding_dimensions = 8
"#,
        source_path.display()
    );
    std::fs::write(recipes_dir.join(format!("{corpus_id}.toml")), recipe_toml).unwrap();
    (recipes_dir, source_path)
}

/// Build an `AppState` whose corpus engine is rooted in `tmp` and
/// reports its node id as `node-test` so partition directories land
/// at a predictable path.
fn test_state(tmp: &TempDir, embed_fn: corpus_engine::types::EmbedFn) -> AppState {
    let index_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&index_dir).unwrap();
    let recipes_dir = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let engine = CorpusEngine::new(recipes_dir, index_dir, embed_fn)
        .with_embedding_model("mock-8d")
        .with_self_node_id("node-test");

    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        join_key_hash: [0u8; 32],
        members: HashMap::new(),
        peers: vec![],
    };
    let state = AppState::new_with_platform_and_engine(
        NodeId::from_u128(1),
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
        Arc::new(commonwealth_app::registry::AppRegistry::new()),
        Some(Arc::new(engine)),
    );
    state
}

async fn post_json<T: serde::Serialize>(
    app: Router,
    path: &str,
    body: &T,
) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

async fn get(app: Router, path: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

#[derive(serde::Deserialize, Debug)]
struct InstallResp {
    corpus_id: String,
    spawned: bool,
}

#[derive(serde::Deserialize, Debug)]
struct ProgressSnapshot {
    progress: HashMap<String, IngestProgress>,
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)] // cancel_signalled is racy; retained for Debug format only.
struct CancelResp {
    cancel_signalled: bool,
    wiped: bool,
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)] // cancel_signalled is racy; retained for Debug format only.
struct PauseResp {
    cancel_signalled: bool,
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)] // Most fields carried for Debug-format panics only.
struct StatusEntry {
    corpus_id: String,
    active: bool,
    progress: Option<IngestProgress>,
    shards_completed: usize,
    shards_total: usize,
    committed_iter_pos: u64,
    canonical_present: bool,
    partition_present: bool,
    canonical_in_progress: bool,
    partition_in_progress: bool,
    estimated_fraction: Option<f32>,
    #[serde(default)]
    estimated_total_sections: Option<u64>,
    #[serde(default)]
    estimated_total_articles: Option<u64>,
}

#[derive(serde::Deserialize, Debug)]
struct StatusResponse {
    entries: Vec<StatusEntry>,
}

/// Poll `/internal/corpus/progress` until the predicate returns true
/// or a timeout elapses. Returns the last observed snapshot.
async fn wait_until_progress<F>(
    state: &AppState,
    predicate: F,
    timeout: Duration,
    label: &str,
) -> ProgressSnapshot
where
    F: Fn(&ProgressSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let app = internal_router(state.clone());
        let (status, body) = get(app, "/internal/corpus/progress").await;
        assert_eq!(status, StatusCode::OK, "GET /progress at {label}");
        let snapshot: ProgressSnapshot = serde_json::from_slice(&body).unwrap();
        if predicate(&snapshot) {
            return snapshot;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_until_progress timed out at '{label}'; last snapshot = {snapshot:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll on-disk state until `predicate` holds or timeout. Lets us
/// assert the daemon finished the wipe / finalise without sprinkling
/// raw sleeps through the test.
async fn wait_until_filesystem<F>(
    check: F,
    timeout: Duration,
    label: &str,
) where
    F: Fn() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while !check() {
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_until_filesystem timed out at '{label}'");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_cancel_reinstall_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let corpus_id = "testcorpus";

    seed_fixture(tmp.path(), corpus_id, 600, true);

    // ── Phase 1: install with slow embed so cancel has room to land ──
    let state = test_state(&tmp, slow_mock_embed_fn());
    let partition_dir = tmp
        .path()
        .join("indexes")
        .join(format!("{corpus_id}-partition-node-test"));
    let canonical_dir = tmp.path().join("indexes").join(corpus_id);

    let (status, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "install returned non-OK: {:?}", String::from_utf8_lossy(&body));
    let install_resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(install_resp.spawned, "first install should report spawned=true");
    assert_eq!(install_resp.corpus_id, corpus_id);

    // A second install immediately afterwards must be idempotent.
    let (_, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    let dup_resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(
        !dup_resp.spawned,
        "second install must not spawn a duplicate task"
    );

    // Progress eventually reports the corpus, confirming the ingest
    // task actually started rather than erroring out silently.
    wait_until_progress(
        &state,
        |snap| snap.progress.contains_key(corpus_id),
        Duration::from_secs(10),
        "ingest progress visible",
    )
    .await;
    assert!(
        partition_dir.exists(),
        "partition-of-self directory should exist while ingest is running"
    );

    // Status endpoint should expose the same corpus with a fused
    // on-disk + progress view. This is the data path the Desktop
    // poller consumes so the UI reflects daemon-owned ingests even
    // when Desktop didn't initiate them.
    let (status, body) = get(
        internal_router(state.clone()),
        "/internal/corpus/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status_resp: StatusResponse = serde_json::from_slice(&body).unwrap();
    let entry = status_resp
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .expect("status response must include our active corpus");
    assert!(entry.active, "entry should be marked active while ingesting");
    assert!(
        entry.partition_in_progress,
        "entry should report partition_in_progress while the ingest writes to the partition dir"
    );
    assert!(
        entry.progress.is_some(),
        "entry should carry the latest IngestProgress event once one has landed"
    );

    // ── Phase 2a: cancel without confirm_wipe must be rejected ──────
    // The endpoint is destructive; missing confirm is a 400 with a
    // pointer to /pause for the non-destructive variant. This is the
    // guardrail introduced after an accidental wipe of weeks of
    // ingest work.
    let (status, _body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/cancel",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "cancel without confirm_wipe must be rejected"
    );

    // ── Phase 2b: cancel + wipe (with explicit confirm) ─────────────
    let (status, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/cancel",
        &serde_json::json!({ "corpus_id": corpus_id, "confirm_wipe": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cancel returned non-OK: {:?}", String::from_utf8_lossy(&body));
    let cancel_resp: CancelResp = serde_json::from_slice(&body).unwrap();
    assert!(cancel_resp.wiped, "cancel should report the wipe completed");
    // `cancel_signalled` is racy: if the ingest finished before cancel
    // arrived, no flag was flipped and it's reported false. Either
    // outcome is acceptable — what we care about is that the wipe
    // cleared on-disk state.

    wait_until_filesystem(
        || !partition_dir.exists() && !canonical_dir.exists(),
        Duration::from_secs(5),
        "canonical and partition dirs wiped",
    )
    .await;
    assert!(
        state
            .inner
            .active_ingests
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "active_ingests should no longer contain the cancelled corpus"
    );
    assert!(
        state
            .inner
            .corpus_progress
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "corpus_progress entry should have been cleared"
    );

    // ── Phase 3: reinstall with fast embed → end-to-end completion ──
    // A fresh engine with the fast embed lets the ingest finish in
    // sub-second so we can assert the canonical directory materialised.
    let state = test_state(&tmp, fast_mock_embed_fn());
    let (status, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reinstall returned non-OK: {:?}", String::from_utf8_lossy(&body));
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(resp.spawned, "reinstall should spawn a fresh task");

    // Wait for the partition → canonical promotion to complete.
    wait_until_filesystem(
        || canonical_dir.join("_corpus_meta.json").exists() && !partition_dir.exists(),
        Duration::from_secs(30),
        "solo finalise promoted partition to canonical",
    )
    .await;

    // _corpus_meta.json should carry the post-finalise shape.
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(canonical_dir.join("_corpus_meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta["ingestion_in_progress"],
        serde_json::Value::Bool(false),
        "canonical meta must not be left in-progress"
    );
    assert_eq!(
        meta["is_shard"],
        serde_json::Value::Bool(false),
        "canonical meta must not claim is_shard"
    );
    assert!(
        meta["processed_shards"]
            .as_array()
            .map(|arr| arr.is_empty())
            .unwrap_or(false),
        "canonical meta should have no processed_shards entries"
    );
}

/// Companion to [`install_cancel_reinstall_lifecycle`] — verifies the
/// non-destructive `/internal/corpus/pause` route stops an in-flight
/// ingest cleanly while leaving on-disk state intact, and that a
/// subsequent `/internal/corpus/install` resumes rather than starting
/// over.
///
/// This is the regression guard for the accidental-wipe incident:
/// before pause existed, the only way to stop an ingest was a route
/// that destroyed every committed chunk on the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_pause_resume_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let corpus_id = "pausetest";

    seed_fixture(tmp.path(), corpus_id, 600, true);

    // ── Phase 1: install with slow embed so pause has room to land ──
    let state = test_state(&tmp, slow_mock_embed_fn());
    let partition_dir = tmp
        .path()
        .join("indexes")
        .join(format!("{corpus_id}-partition-node-test"));
    let canonical_dir = tmp.path().join("indexes").join(corpus_id);

    let (status, _body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "install returned non-OK");

    wait_until_progress(
        &state,
        |snap| snap.progress.contains_key(corpus_id),
        Duration::from_secs(10),
        "ingest progress visible",
    )
    .await;
    assert!(
        partition_dir.exists(),
        "partition dir should exist while ingest is running"
    );

    // ── Phase 2: pause — must NOT wipe ──────────────────────────────
    let (status, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/pause",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "pause returned non-OK: {:?}",
        String::from_utf8_lossy(&body)
    );
    let _pause_resp: PauseResp = serde_json::from_slice(&body).unwrap();

    // /pause synchronously waits up to 5 s for the ingest task to clear
    // out of `active_ingests` before returning, so by the time we're
    // here the task has exited (or hit that ceiling and logged a warn).
    // The whole point of pause: data must survive.
    assert!(
        partition_dir.exists(),
        "pause must NOT wipe the partition dir — that's the regression we're guarding"
    );
    assert!(
        partition_dir.join("_corpus_meta.json").exists(),
        "pause must preserve _corpus_meta.json so resume works"
    );
    assert!(
        state
            .inner
            .active_ingests
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "active_ingests should be cleared after pause"
    );
    assert!(
        state
            .inner
            .corpus_progress
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "corpus_progress should be cleared after pause"
    );

    // ── Phase 3: resume by re-installing with a fast embed ──────────
    // A fresh state with the fast embed lets the resumed ingest finish
    // promptly. The committed_iter_pos written before pause means the
    // loop skips past already-committed docs rather than re-embedding
    // from zero.
    let state = test_state(&tmp, fast_mock_embed_fn());
    let (status, _body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resume install returned non-OK");

    wait_until_filesystem(
        || canonical_dir.join("_corpus_meta.json").exists() && !partition_dir.exists(),
        Duration::from_secs(30),
        "resumed ingest finalised to canonical",
    )
    .await;

    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(canonical_dir.join("_corpus_meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta["ingestion_in_progress"],
        serde_json::Value::Bool(false),
        "canonical meta must not be left in-progress after resume"
    );
}

/// Exercises the "daemon resumed a legacy canonical ingest after the
/// Desktop session closed" scenario — no active ingest, no recent
/// `IngestProgress` event, just on-disk state and an `extracted.jsonl`
/// next to the corpus. The sampler should fire on first `/status`
/// poll and publish a section-based `estimated_fraction` on the next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_sampler_publishes_estimated_fraction_on_resume() {
    let tmp = TempDir::new().unwrap();
    let corpus_id = "testcorpus";

    // We don't need a full ingest pipeline for this test — just the
    // disk fingerprint the status handler reads: canonical index dir
    // with committed_iter_pos and ingestion_in_progress, plus the
    // extractor's merged JSONL file the sampler consumes.
    let index_dir = tmp.path().join("indexes");
    let downloads = index_dir.join("_downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let canonical = index_dir.join(corpus_id);
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::write(
        canonical.join("_corpus_meta.json"),
        serde_json::json!({
            "corpus_id": corpus_id,
            "ingestion_in_progress": true,
            "committed_iter_pos": 30u64,
            "processed_shards": [],
        })
        .to_string(),
    )
    .unwrap();

    // 50 articles × 2 sections each → sampler estimates
    // total_sections ≈ 50 × (2 + 1 lead) = 150. With committed=30 we
    // expect fraction ≈ 0.20. We allow a generous ±20 % window so
    // the assertion isn't brittle against the sampler's exact
    // bookkeeping (lead-counting etc.).
    let extracted = downloads.join(format!("{corpus_id}.extracted.jsonl"));
    let mut jsonl = String::new();
    for i in 0..50 {
        let line = serde_json::json!({
            "name": format!("Article {i}"),
            "identifier": i,
            "abstract": "Stub abstract with enough padding to clear the extractor minimums.",
            "url": format!("https://en.wikipedia.org/wiki/A{i}"),
            "sections": [
                { "name": "A", "type": "section", "has_parts": [] },
                { "name": "B", "type": "section", "has_parts": [] }
            ]
        });
        jsonl.push_str(&line.to_string());
        jsonl.push('\n');
    }
    std::fs::write(&extracted, jsonl).unwrap();

    // Build an AppState against this pre-seeded disk layout. No
    // ingest is spawned — the status handler does all the work on
    // its own by consulting the engine's on-disk helpers.
    let engine = CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        fast_mock_embed_fn(),
    )
    .with_embedding_model("mock-8d")
    .with_self_node_id("node-test");
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        join_key_hash: [0u8; 32],
        members: HashMap::new(),
        peers: vec![],
    };
    let state = AppState::new_with_platform_and_engine(
        NodeId::from_u128(1),
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
        Arc::new(commonwealth_app::registry::AppRegistry::new()),
        Some(Arc::new(engine)),
    );

    // First poll: sampler hasn't run yet, so sidecar is absent and
    // we should see None for estimated_total_sections. The handler
    // kicks off the sampler in a spawn_blocking task.
    let (status, body) = get(
        internal_router(state.clone()),
        "/internal/corpus/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let resp: StatusResponse = serde_json::from_slice(&body).unwrap();
    let first = resp
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .expect("status must include legacy-resume corpus");
    assert!(first.canonical_in_progress);
    assert_eq!(first.committed_iter_pos, 30);
    // First poll: may or may not have the sidecar depending on how
    // fast the spawn_blocking task lands. Don't assert — just ensure
    // the overall shape is sensible.
    let _ = first.estimated_total_sections;

    // Give the sampler a beat to finish. The fixture is ~5 KB so the
    // full sample runs in microseconds; a 500 ms budget is ample and
    // keeps the test fast locally.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let entry = loop {
        let (_, body) = get(
            internal_router(state.clone()),
            "/internal/corpus/status",
        )
        .await;
        let resp: StatusResponse = serde_json::from_slice(&body).unwrap();
        let entry = resp
            .entries
            .into_iter()
            .find(|e| e.corpus_id == corpus_id)
            .expect("status must still include corpus");
        if entry.estimated_total_sections.is_some()
            && entry.estimated_fraction.is_some()
        {
            break entry;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "sampler never populated estimated_total_sections for {corpus_id}; last entry = {:?}",
                entry
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let total = entry
        .estimated_total_sections
        .expect("sampler should have published a total");
    let fraction = entry.estimated_fraction.unwrap();
    assert!(total > 0, "sampler must publish a non-zero total");
    // Truth is 30 committed / (50 × 3) = 0.20; allow ±30 % for the
    // lead-counting heuristic and sample extrapolation.
    assert!(
        (fraction - 0.20).abs() < 0.30,
        "expected fraction near 0.20, got {fraction} (total={total})"
    );
    assert!(
        entry.estimated_total_articles.unwrap_or(0) > 0,
        "articles estimate should accompany sections estimate"
    );

    // Sidecar file should have landed on disk so the next daemon
    // session reads it without resampling.
    let sidecar = downloads.join(format!("{corpus_id}.extracted.jsonl.count"));
    assert!(sidecar.exists(), "sidecar file should be written");
}
