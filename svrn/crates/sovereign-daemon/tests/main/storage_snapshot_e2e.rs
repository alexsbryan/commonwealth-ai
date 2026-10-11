// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StorageSnapshot` integration test.
//!
//! `sovereign-mesh::ledger_port::run_storage_snapshot_loop` is L1-pinned
//! (the ledger port's own tests cover first-tick-immediate and
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
//! `start` (which spawns the snapshot loop), wait ~100 ms for
//! the first immediate tick, then read the contribution emitter
//! and assert the recorded `StorageSnapshot` contains only the
//! mesh-shared corpus.
use std::sync::Arc;
use std::time::Duration;

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::mesh_admin_services;
use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use oicp_types::contributions::LedgerEventKind;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::ledger_port::ContributionLedgerPort;

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
                text_sha256: None,
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

    // The port double lists the same `indexes` dir through the leaf's
    // `FsIndexSource` — `installed_indexes()` only enumerates the index
    // side.
    let engine =
        Arc::new(crate::common::reading_double(indexes, mock_embed_fn()).accepting_yield_hooks());

    // Daemon: data_dir holds mesh.json + node_id; corpus engine
    // injected so `start_daemon` spawns the snapshot loop with the
    // mesh_sharing filter in place.
    // The daemon's contribution port dials cw-rails (five-programs fp-88):
    // a stand-in `/v1/ledger/contributions` door on `[daemon] rails_base`
    // hands each write to fp-80's recording double, which this test reads.
    let double = Arc::new(RecordingLedger::new(NodeId::from_u128(0)));
    let door = common::spawn_router(axum::Router::new().route(
        "/v1/ledger/contributions",
        axum::routing::post({
            let double = Arc::clone(&double);
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let kind = serde_json::from_value(body["record"].clone()).unwrap();
                double.record(kind).await.unwrap();
                axum::Json(())
            }
        }),
    ))
    .await;
    let mut config = SetupConfig::unconfigured();
    config.daemon.rails_base = Some(format!("http://{door}"));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        config,
        common::desktop_services_with_engine(
            Arc::clone(&engine) as Arc<dyn corpus_index::ingest_port::daemon::IngestPort>
        ),
    );
    daemon
        .start()
        .await
        .expect("start succeeds with engine attached");

    // Run-time wait: the snapshot loop's first tick fires
    // immediately (per `run_storage_snapshot_loop`'s contract), but the
    // boot shares this test's one runtime thread with the peer origin's
    // first registration, whose capability claims probe the hardware for
    // ~250 ms (pb-mesh-exit-transport). Wait for the write, bounded; the
    // next tick is an hour away, so "exactly one" below still holds.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while double.recorded_contributions().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let events = double.recorded_contributions();

    // Filter to StorageSnapshot rows.
    let snapshots: Vec<&Vec<(String, f64)>> = events
        .iter()
        .filter_map(|e| match e {
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
    // The same stand-in door as the test above: Fabric keeps no emitter
    // since five-programs fp-111, so the contribution port is what we read.
    let double = Arc::new(RecordingLedger::new(NodeId::from_u128(0)));
    let door = common::spawn_router(axum::Router::new().route(
        "/v1/ledger/contributions",
        axum::routing::post({
            let double = Arc::clone(&double);
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let kind = serde_json::from_value(body["record"].clone()).unwrap();
                double.record(kind).await.unwrap();
                axum::Json(())
            }
        }),
    ))
    .await;
    let mut config = SetupConfig::unconfigured();
    config.daemon.rails_base = Some(format!("http://{door}"));
    let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf(), config, mesh_admin_services());
    // Intentionally NO `set_corpus_engine` call.
    daemon.start().await.expect("start works without an engine");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let events = double.recorded_contributions();

    let snapshot_count = events
        .iter()
        .filter(|e| matches!(e, LedgerEventKind::StorageSnapshot { .. }))
        .count();
    assert_eq!(
        snapshot_count, 0,
        "no engine → no snapshot; got events: {events:?}"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}
