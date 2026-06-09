// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StorageSnapshot` integration test.
//!
//! `commonwealth-state::run_storage_snapshot_loop` is L1-pinned (the
//! contribution module's own tests cover first-tick-immediate and
//! empty-walker-no-event). What's NOT pinned is the daemon-side
//! integration: that the walker `EmbeddedDaemon::start_daemon`
//! constructs (`daemon.rs::1546-1605`, paraphrased)
//!
//!     installed.into_iter()
//!         .filter(|i| i.mesh_sharing)
//!         .map(|i| (i.corpus_id, i.index_size_bytes as f64 / 1e9))
//!         .collect()
//!
//! actually drops `mesh_sharing == false` corpora before they reach
//! the ledger. The promise — §10 of `SYSTEM_OVERVIEW.md` —
//! is intra-mesh-only contribution accounting; a regression that
//! drops the `.filter(...)` line would publish *local* corpora into
//! the ledger that the rest of the mesh aggregates, leaking
//! private-by-design state into a shared signal.
//!
//! Approach: install two real `CorpusIndex` instances on disk (one
//! mesh-shared, one local), point an `EmbeddedDaemon` at them, run
//! `create_mesh` (which spawns the snapshot loop), wait ~100 ms for
//! the first immediate tick, then read the contribution emitter
//! and assert the recorded `StorageSnapshot` contains only the
//! mesh-shared corpus.
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::contributions::LedgerEventKind;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_mesh::daemon::EmbeddedDaemon;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    // Zero-vector embed: enough to satisfy the index's column
    // schema; we never search this corpus, so the values don't
    // matter for this test.
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Create a real on-disk corpus index at `<indexes>/<id>` with the
/// given `mesh_sharing` flag, populated with a single trivial
/// chunk so it counts as "installed" when the engine enumerates.
async fn install_corpus(indexes_dir: &std::path::Path, id: &str, name: &str, mesh_sharing: bool) {
    let path = indexes_dir.join(id);
    let index = CorpusIndex::create(
        &path,
        id,
        name,
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        mesh_sharing,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: format!("dummy content for {id}"),
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

#[tokio::test]
async fn first_tick_emits_only_mesh_shared_corpora_to_ledger() {
    // Stage: temp data_dir for the daemon, with `indexes/` populated
    // by two corpora — one mesh-shared, one not.
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();

    install_corpus(&indexes, "shared-corpus", "Mesh-Shared", true).await;
    install_corpus(&indexes, "local-only", "Local-Only", false).await;

    // CorpusEngine reads from the same `indexes` dir. The recipes
    // dir doesn't matter for this test — `installed_indexes()` only
    // enumerates the index side.
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    // Daemon: data_dir holds mesh.json + node_id; corpus engine
    // injected so `start_daemon` spawns the snapshot loop with the
    // mesh_sharing filter in place.
    let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf());
    daemon.set_corpus_engine(Arc::clone(&engine)).await;
    daemon
        .create_mesh("storage-snapshot test", "node")
        .await
        .expect("create_mesh succeeds with engine attached");

    // Run-time wait: the snapshot loop's first tick fires
    // immediately (per `run_storage_snapshot_loop`'s contract); the
    // tokio::spawn'd record + serialize + MeshStore::set round-trip
    // completes in single-digit ms on an in-memory store. 200 ms
    // is comfortable headroom for a loaded CI box.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let app_state = daemon
        .app_state()
        .await
        .expect("app_state present after create_mesh");
    let events = app_state
        .inner
        .contribution_emitter
        .events()
        .expect("contribution_emitter.events() reads from in-memory store");

    // Filter to StorageSnapshot rows.
    let snapshots: Vec<&Vec<(String, f64)>> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::StorageSnapshot { corpora } => Some(corpora),
            _ => None,
        })
        .collect();

    assert_eq!(
        snapshots.len(),
        1,
        "exactly one StorageSnapshot expected from the immediate first tick; \
         observed events: {events:?}"
    );

    let recorded = snapshots[0];
    let ids: Vec<&str> = recorded.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        ids.contains(&"shared-corpus"),
        "mesh-shared corpus must appear in the snapshot; got {ids:?}"
    );
    assert!(
        !ids.contains(&"local-only"),
        "local-only corpus must NOT appear in the snapshot — §10 promises \
         intra-mesh-only accounting; got {ids:?}. A regression that drops \
         the `.filter(|i| i.mesh_sharing)` call in start_daemon would \
         leak this private corpus into the gossip-replicated ledger."
    );

    daemon.shutdown().await.expect("graceful shutdown");
}

#[tokio::test]
async fn snapshot_emits_nothing_when_no_corpus_engine_attached() {
    // Counterpart: a daemon with no engine wired should NOT emit
    // a StorageSnapshot. The walker closure short-circuits on
    // `Option::None` and returns an empty Vec; the loop sees
    // empty input and skips the emission. Pre-fix nothing here
    // pinned this — a refactor that changed `Some/None` semantics
    // on the engine field would silently produce empty snapshots
    // every hour.
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf());
    // Intentionally NO `set_corpus_engine` call.
    daemon
        .create_mesh("no-engine test", "node")
        .await
        .expect("create_mesh works without an engine");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let app_state = daemon
        .app_state()
        .await
        .expect("app_state present after create_mesh");
    let events = app_state
        .inner
        .contribution_emitter
        .events()
        .expect("emitter.events() ok");

    let snapshot_count = events
        .iter()
        .filter(|e| matches!(e.kind, LedgerEventKind::StorageSnapshot { .. }))
        .count();
    assert_eq!(
        snapshot_count, 0,
        "no engine → no snapshot; got events: {events:?}"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}
