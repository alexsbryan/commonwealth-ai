//! End-to-end tests for the obsidian-vault live-sync wiring landed in
//! the `Workstream A` reconciliation work.
//!
//! Mirrors `watched_folder_e2e.rs` shape but registers an
//! `ObsidianVault` source instead of a `WatchedFolder`, so it
//! exercises the new code paths:
//!
//!   - `LocalCorpusSourceType::should_reconcile()` returning true for
//!     vaults
//!   - `LocalCorpusSourceType::reconcile_kind() == Some(ObsidianVault)`
//!   - `worker::reconciliation_config_for` synthesising a
//!     `WatchedFolderConfig` for vaults
//!   - The walker's vault excludes (`_sovereign-index/**`,
//!     `.obsidian/**`) — sovereign-managed files should not appear in
//!     the diff
//!   - `state.last_writeback_unix` populated post-sweep when a vault's
//!     `refresh_writeback_if_clustered` succeeds
//!
//! What's NOT exercised here: the actual writeback execution path,
//! because that requires `LocalCorpusManager::cluster()` which depends
//! on the inference layer. The `manager.refresh_writeback_if_clustered`
//! is exercised via its `Ok(None)` branch (no cached cluster → benign
//! skip) which is the v1 default state for a freshly-registered vault.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::traits::StateStore;
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::local_corpus::config::{
    LocalCorpusConfig, LocalCorpusSourceType, ReconcileKind,
};
use sovereign_tools::local_corpus::manager::LocalCorpusManager;
use sovereign_tools::local_corpus::watched::events::noop_sink;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::watched::state::WatchedFolderState;
use sovereign_tools::local_corpus::watched::worker::{Worker, WorkerOutcome};
use tempfile::TempDir;

const EMBED_DIMS: usize = 768;

fn stub_embed() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0f32; EMBED_DIMS]) }))
}

struct Fixture {
    _tmp: TempDir,
    vault: PathBuf,
    _data_dir: PathBuf,
    _engine: Arc<CorpusEngine>,
    manager: Arc<LocalCorpusManager>,
    registry: Arc<WatchedFolderRegistry>,
    worker: Arc<Worker>,
}

async fn boot() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let indexes_dir = data_dir.join("indexes");
    let recipes_dir = data_dir.join("local-corpus-recipes");
    let vault = tmp.path().join("vault");
    let snapshots = tmp.path().join("vault-snapshots");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::create_dir_all(&snapshots).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, indexes_dir, stub_embed())
            .with_embedding_model("test-mock"),
    );
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let manager = Arc::new(
        LocalCorpusManager::init(
            Arc::clone(&engine),
            store,
            None,
            data_dir.clone(),
            snapshots.clone(),
        )
        .await
        .expect("manager init"),
    );
    let registry = Arc::new(WatchedFolderRegistry::new());
    let worker = Arc::new(Worker::new(
        Arc::clone(&engine),
        Arc::clone(&manager),
        Arc::clone(&registry),
        noop_sink(),
        manager.index_dir_root(),
    ));
    Fixture {
        _tmp: tmp,
        vault,
        _data_dir: data_dir,
        _engine: engine,
        manager,
        registry,
        worker,
    }
}

fn write_note(p: &std::path::Path, s: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, s).unwrap();
}

async fn register_vault(fx: &Fixture, snapshots_root: PathBuf) -> String {
    let cfg = LocalCorpusConfig::obsidian_vault(fx.vault.clone(), snapshots_root);
    let id = fx.manager.register(cfg).await.expect("register vault");
    // Register at the watched-folder reconciliation registry with
    // a short sweep interval so the test doesn't wait. The scheduler
    // is not driving sweeps here — we call `worker.run_once` directly.
    fx.registry
        .register(id.clone(), /*sweep_interval_secs=*/ 60)
        .await;
    id
}

#[tokio::test]
async fn obsidian_vault_source_is_reconcilable() {
    // Recon contract: an obsidian-vault corpus surfaces as
    // `should_reconcile=true` and `reconcile_kind=ObsidianVault`,
    // which is what makes the daemon's auto-resume loop pick it up
    // alongside watched folders.
    let cfg = LocalCorpusConfig::obsidian_vault(
        PathBuf::from("/tmp/vault"),
        PathBuf::from("/tmp/snap"),
    );
    assert!(cfg.source_type.should_reconcile());
    assert_eq!(
        cfg.source_type.reconcile_kind(),
        Some(ReconcileKind::ObsidianVault)
    );
    // The factory should still tag the variant correctly.
    assert!(matches!(
        cfg.source_type,
        LocalCorpusSourceType::ObsidianVault { .. }
    ));
    // ARCH §7: vaults are personal, always local.
    assert_eq!(
        cfg.scope,
        sovereign_tools::local_corpus::config::CorpusScope::Local
    );
    // Live-sync (Phase A2): snapshot retention bumped from 3 → 24 to
    // cover ~24h of active editing at the daemon's sweep cadence.
    let wb = cfg.write_back.as_ref().expect("vault has writeback config");
    assert_eq!(wb.snapshot_retention, 24);
}

#[tokio::test]
async fn vault_initial_ingest_then_no_changes_sweep() {
    let fx = boot().await;
    let snapshots = fx._tmp.path().join("vault-snapshots");
    write_note(&fx.vault.join("alpha.md"), "# Alpha\n\nContent of alpha.");
    write_note(&fx.vault.join("beta.md"), "# Beta\n\nContent of beta.");

    let id = register_vault(&fx, snapshots).await;
    assert!(id.starts_with("obsidian-"), "vault corpus_id prefix: {id}");

    // Initial ingest stages chunks into the engine. Mirrors the
    // watched-folder boot sequence.
    let stats = fx.manager.ingest(&id, None, None).await.expect("ingest");
    assert!(
        stats.files_indexed >= 2,
        "expected >=2 files indexed, got {}",
        stats.files_indexed
    );

    // First sweep: walker sees the just-ingested files. State's
    // prior_hashes is empty so they classify as `added` and re-apply
    // through the updater (idempotent). State.entries populates as a
    // side effect.
    let _first = fx.worker.run_once(&id).await.expect("first sweep");

    let state_dir = fx.manager.index_dir_root().join(&id);
    let state = WatchedFolderState::load(&state_dir)
        .expect("state load")
        .expect("state file exists after sweep");
    assert!(
        !state.entries.is_empty(),
        "state.entries populated after first sweep"
    );
    // Vault has no cached cluster yet → refresh_writeback_if_clustered
    // returns Ok(None) → state.last_writeback_unix stays None.
    assert!(
        state.last_writeback_unix.is_none(),
        "no writeback fired without a cached cluster preview"
    );

    // Second sweep with no FS changes: NoChanges.
    let second = fx.worker.run_once(&id).await.expect("second sweep");
    assert_eq!(
        second,
        WorkerOutcome::NoChanges,
        "vault: second sweep with no edits reports NoChanges"
    );
}

#[tokio::test]
async fn vault_walker_excludes_sovereign_index_dir() {
    // Phase A2 invariant: files under `_sovereign-index/**` are
    // sovereign-owned and must never appear in the walker's snapshot
    // — otherwise the writeback feedback loop reignites every sweep.
    let fx = boot().await;
    let snapshots = fx._tmp.path().join("vault-snapshots");
    write_note(&fx.vault.join("user.md"), "# User note\n");
    write_note(
        &fx.vault.join("_sovereign-index/equity/canopy.md"),
        "# Map of Content\n",
    );

    let id = register_vault(&fx, snapshots).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("sweep");

    let state_dir = fx.manager.index_dir_root().join(&id);
    let state = WatchedFolderState::load(&state_dir)
        .expect("state load")
        .expect("state");
    assert!(
        state.entries.contains_key("user.md"),
        "user note must enter the walker snapshot"
    );
    assert!(
        !state
            .entries
            .keys()
            .any(|k| k.starts_with("_sovereign-index/")),
        "files under _sovereign-index/** must be excluded; entries = {:?}",
        state.entries.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn vault_editing_a_note_is_detected_as_modified() {
    let fx = boot().await;
    let snapshots = fx._tmp.path().join("vault-snapshots");
    write_note(&fx.vault.join("alpha.md"), "# Alpha\n\nOriginal body.\n");
    let id = register_vault(&fx, snapshots).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    // Edit the note.
    write_note(&fx.vault.join("alpha.md"), "# Alpha\n\nRewritten body.\n");

    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    match outcome {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(summary.modified, 1, "modified count: {summary:?}");
            assert_eq!(summary.added, 0);
            assert_eq!(summary.removed, 0);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[tokio::test]
async fn vault_adding_a_new_note_is_detected_as_added() {
    let fx = boot().await;
    let snapshots = fx._tmp.path().join("vault-snapshots");
    write_note(&fx.vault.join("alpha.md"), "# Alpha\n");
    let id = register_vault(&fx, snapshots).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    // Add a new note.
    write_note(&fx.vault.join("beta.md"), "# Beta\n\nBrand new.\n");

    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    match outcome {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(summary.added, 1, "added count: {summary:?}");
            assert_eq!(summary.modified, 0);
            assert_eq!(summary.removed, 0);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[tokio::test]
async fn vault_deleting_a_note_records_a_tombstone_under_guard() {
    // Five notes so a single deletion is 20% — under the 25%
    // default fractional guard. Otherwise the guard intercepts and
    // we never see the tombstone path.
    let fx = boot().await;
    let snapshots = fx._tmp.path().join("vault-snapshots");
    for i in 0..5 {
        write_note(&fx.vault.join(format!("n{i}.md")), &format!("# Note {i}\n"));
    }
    let id = register_vault(&fx, snapshots).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    std::fs::remove_file(fx.vault.join("n0.md")).unwrap();
    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    match outcome {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(summary.removed, 1, "removed: {summary:?}");
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    let state = WatchedFolderState::load(&fx.manager.index_dir_root().join(&id))
        .expect("state load")
        .expect("state file present");
    assert_eq!(
        state.tombstones.len(),
        1,
        "tombstone recorded for deleted vault note"
    );
    assert_eq!(state.tombstones[0].doc_id, "n0.md");
}
