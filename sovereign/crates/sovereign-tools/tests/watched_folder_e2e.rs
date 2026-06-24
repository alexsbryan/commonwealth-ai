// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end tests for the `WatchedFolder` reconciliation pipeline.
//!
//! Drives the full register → initial-ingest → run_once flow against
//! a real `CorpusEngine` + `LocalCorpusManager`, with a zero-vector
//! embed (LanceDB stores it as opaque bytes — adequate for our
//! diff-and-apply assertions which never re-search).
//!
//! What's exercised:
//!   - `LocalCorpusConfig::watched_folder` factory + persistence
//!   - `manager.register` + `manager.ingest` (initial sweep)
//!   - `Worker::run_once` happy path: NoChanges, Applied { added },
//!     Applied { modified }, Applied { removed }
//!   - Tombstones for deletions
//!   - Tombstone revival within the grace window

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::traits::{SensitiveCorpusOracle, StateStore};
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::local_corpus::config::{
    LocalCorpusConfig, LocalCorpusSourceType, WatchedFolderConfig,
};
use sovereign_tools::local_corpus::manager::LocalCorpusManager;
use sovereign_tools::local_corpus::watched::events::noop_sink;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::watched::diff::WatchedDiff;
use sovereign_tools::local_corpus::watched::state::WatchedFolderState;
use sovereign_tools::local_corpus::watched::worker::{Worker, WorkerOutcome};
use sovereign_tools::local_corpus::watched::workflow_trigger::WorkflowTriggerRuntime;
use tempfile::TempDir;

const EMBED_DIMS: usize = corpus_engine::DEFAULT_EMBED_DIM;

/// Deterministic embed: every call returns the same zero vector.
/// LanceDB stores it as opaque bytes so the diff/apply path completes
/// without any model on disk.
fn stub_embed() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0f32; EMBED_DIMS]) }))
}

struct Fixture {
    _tmp: TempDir,
    data_dir: PathBuf,
    folder: PathBuf,
    engine: Arc<CorpusEngine>,
    manager: Arc<LocalCorpusManager>,
    registry: Arc<WatchedFolderRegistry>,
    worker: Arc<Worker>,
}

async fn boot() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let indexes_dir = data_dir.join("indexes");
    // Point the engine's recipe-overrides directory at the same path
    // `LocalCorpusManager::ingest` writes recipe TOMLs to. Without
    // this, `engine.load_recipe(corpus_id)` (called by
    // `CorpusUpdater::apply_update`) returns
    // "No registry entry for corpus 'watched-…'" and every sweep
    // after the first errors. The production daemon is configured
    // the same way (data_dir.join("indexes") for both indexes and
    // recipe overrides — see daemon_cmd.rs:401).
    let recipes_dir = data_dir.join("local-corpus-recipes");
    let folder = tmp.path().join("watched");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::create_dir_all(&folder).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, indexes_dir, stub_embed()).with_embedding_model("test-mock"),
    );
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let manager = Arc::new(
        LocalCorpusManager::init(
            Arc::clone(&engine),
            store,
            None,
            data_dir.clone(),
            data_dir.join("vault-snapshots"),
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
        data_dir,
        folder,
        engine,
        manager,
        registry,
        worker,
    }
}

fn write(p: &std::path::Path, s: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, s).unwrap();
}

async fn register(fx: &Fixture, watched_cfg: WatchedFolderConfig) -> String {
    let cfg =
        LocalCorpusConfig::watched_folder(fx.folder.clone(), "test".into(), watched_cfg.clone());
    let id = fx.manager.register(cfg).await.expect("register");
    fx.registry
        .register(id.clone(), watched_cfg.sweep_interval_secs)
        .await;
    id
}

/// A test double for the living-trigger runtime that records each `dispatch`
/// instead of running a workflow — so the seam can be asserted in-process without
/// a daemon or a real workflow run.
#[derive(Default)]
struct RecordingRuntime {
    calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}

#[async_trait::async_trait]
impl WorkflowTriggerRuntime for RecordingRuntime {
    async fn dispatch(
        &self,
        corpus_id: &str,
        _config: &LocalCorpusConfig,
        _watched_cfg: &WatchedFolderConfig,
        diff: &WatchedDiff,
    ) {
        self.calls
            .lock()
            .unwrap()
            .push((corpus_id.to_string(), diff.added.clone()));
    }
}

/// The living trigger: a sweep that changes files on a folder with `run_on_changes`
/// set dispatches the workflow runtime with the right corpus + diff — and a sweep
/// with no changes does not. Proves the Worker seam end-to-end (Phase 2/3) without
/// a daemon.
#[tokio::test]
async fn run_on_changes_dispatches_on_changed_sweep_only() {
    let fx = boot().await;
    write(&fx.folder.join("a.md"), "alpha content");

    let recorder = Arc::new(RecordingRuntime::default());
    // A worker wired with the recording runtime (the daemon wires a real one).
    let worker = Arc::new(
        Worker::new(
            Arc::clone(&fx.engine),
            Arc::clone(&fx.manager),
            Arc::clone(&fx.registry),
            noop_sink(),
            fx.manager.index_dir_root(),
        )
        .with_workflow_runtime(Some(recorder.clone() as Arc<dyn WorkflowTriggerRuntime>)),
    );

    let cfg = WatchedFolderConfig {
        run_on_changes: Some("notebook".into()),
        ..Default::default()
    };
    let id = register(&fx, cfg).await;
    // Initial ingest establishes the index + meta (mirrors the other tests).
    fx.manager
        .ingest(&id, None, None)
        .await
        .expect("initial ingest");
    // First sweep populates state.entries (it re-applies the ingested file as an
    // "add", so it may dispatch once — that's fine, we measure deltas from here).
    worker.run_once(&id).await.expect("first sweep");

    // A sweep with no filesystem change must NOT fire the trigger again.
    let before = recorder.calls.lock().unwrap().len();
    worker.run_once(&id).await.expect("no-change sweep");
    assert_eq!(
        recorder.calls.lock().unwrap().len(),
        before,
        "an unchanged sweep must not fire the trigger"
    );

    // Add a file → the next sweep produces a diff → the trigger fires with it.
    write(&fx.folder.join("b.md"), "beta content");
    worker.run_once(&id).await.expect("changed sweep");

    let calls = recorder.calls.lock().unwrap();
    assert!(
        calls.len() > before,
        "a changed sweep must fire the trigger (had {before}, now {})",
        calls.len()
    );
    let last = calls.last().unwrap();
    assert_eq!(last.0, id, "dispatched for the right corpus");
    assert!(
        last.1.iter().any(|p| p.contains("b.md")),
        "the diff carries the added file; got {:?}",
        last.1
    );
}

/// Empty-folder gotcha: registering + ingesting a folder with no files must still
/// produce a valid, ingestion-complete index (`_corpus_meta.json` present), so a
/// watched-folder sweep doesn't error forever on a missing meta — and so files
/// added later are picked up. Regression for the living-trigger e2e finding.
#[tokio::test]
async fn empty_folder_ingest_yields_a_sweepable_index() {
    let fx = boot().await;
    // Register an EMPTY folder, then ingest. Before the fix this left the index
    // without `_corpus_meta.json` and every sweep errored.
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager
        .ingest(&id, None, None)
        .await
        .expect("ingest of an empty folder must succeed");

    // The meta must exist now — the index is a valid (empty) corpus.
    let meta = fx
        .manager
        .index_dir_root()
        .join(&id)
        .join("_corpus_meta.json");
    assert!(
        meta.is_file(),
        "empty-folder ingest must create _corpus_meta.json at {}",
        meta.display()
    );

    // A sweep over the still-empty folder must NOT error (previously: Errored).
    fx.worker
        .run_once(&id)
        .await
        .expect("sweep over an empty folder must not error");

    // A file added after registration must be picked up — the empty index accepts
    // appends through the normal apply path.
    write(&fx.folder.join("late.md"), "arrived after registration");
    let outcome = fx
        .worker
        .run_once(&id)
        .await
        .expect("sweep after adding a file must not error");
    assert!(
        matches!(outcome, WorkerOutcome::Applied(ref s) if s.added >= 1),
        "the late file should be detected + applied; got {outcome:?}"
    );
}

#[tokio::test]
async fn watched_folder_factory_pins_local_scope() {
    // ARCH §7: privacy invariant. Even when the caller passes a
    // hand-constructed WatchedFolderConfig, the factory must hardcode
    // CorpusScope::Local. Pinned here at the integration boundary so
    // a refactor that parameterises scope would fail before any test
    // that exercises the actual data pipeline.
    let cfg = LocalCorpusConfig::watched_folder(
        PathBuf::from("/tmp/x"),
        "x".into(),
        WatchedFolderConfig::default(),
    );
    assert_eq!(
        cfg.scope,
        sovereign_tools::local_corpus::config::CorpusScope::Local
    );
    assert!(matches!(
        cfg.source_type,
        LocalCorpusSourceType::WatchedFolder(_)
    ));
}

#[tokio::test]
async fn initial_ingest_then_no_changes_sweep() {
    let fx = boot().await;
    write(&fx.folder.join("a.md"), "alpha content");
    write(&fx.folder.join("b.md"), "beta content");

    let id = register(&fx, WatchedFolderConfig::default()).await;

    let stats = fx
        .manager
        .ingest(&id, None, None)
        .await
        .expect("initial ingest");
    assert!(
        stats.files_indexed >= 2,
        "expected at least 2 files indexed, got {}",
        stats.files_indexed
    );

    // The Worker's first run_once should:
    //   1. Walk the folder.
    //   2. See no diff vs the prior manifest (the initial ingest's
    //      output) — but since we haven't populated the per-corpus
    //      state file with mtimes yet, the first sweep treats every
    //      walked file as `added` because prior_hashes is empty.
    //
    // This is the correct as-of-now behaviour: the sweep
    // re-classifies the just-ingested files as adds and re-applies
    // them through CorpusUpdater::apply_update. Re-application is
    // idempotent per `delta.rs`'s phase_additions (which inserts
    // chunks under the same source_doc_id). The acceptance check is
    // that the run completes without error and persists state.
    let outcome = fx.worker.run_once(&id).await.expect("first run_once");
    let _ = outcome;

    // After the first worker sweep, state.entries should be
    // populated with the walked files so the NEXT sweep detects no
    // changes.
    let state = WatchedFolderState::load(&fx.manager.index_dir_root().join(&id))
        .expect("state load")
        .expect("state file exists after sweep");
    assert!(
        !state.entries.is_empty(),
        "state.entries should be populated after first sweep"
    );

    let outcome2 = fx.worker.run_once(&id).await.expect("second run_once");
    assert_eq!(
        outcome2,
        WorkerOutcome::NoChanges,
        "second sweep with no filesystem changes should report NoChanges"
    );
    let _ = (fx.engine, fx.data_dir); // keep the fixture members live until end-of-scope
}

#[tokio::test]
async fn adding_a_new_file_is_detected_as_added() {
    let fx = boot().await;
    write(&fx.folder.join("a.md"), "alpha content");
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    // Now add a brand new file and sweep again.
    write(&fx.folder.join("b.md"), "beta content");
    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    match outcome {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(summary.added, 1, "expected one added doc, got {summary:?}");
            assert_eq!(summary.modified, 0);
            assert_eq!(summary.removed, 0);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[tokio::test]
async fn deleting_a_file_records_a_tombstone() {
    let fx = boot().await;
    // Five files so that deleting one is 20% — under the default
    // 25% fractional guard threshold. Otherwise the threshold guard
    // would intercept the delete and we'd never get to the
    // tombstone-recording path.
    for i in 0..5 {
        write(&fx.folder.join(format!("f{i}.md")), &format!("content {i}"));
    }
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    // Remove one file and sweep.
    std::fs::remove_file(fx.folder.join("f0.md")).unwrap();
    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    match outcome {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(
                summary.removed, 1,
                "expected one removed doc, got {summary:?}"
            );
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    let state = WatchedFolderState::load(&fx.manager.index_dir_root().join(&id))
        .expect("state load")
        .expect("state file exists");
    assert_eq!(state.tombstones.len(), 1, "tombstone for deleted file");
    assert_eq!(state.tombstones[0].doc_id, "f0.md");
}

#[tokio::test]
async fn deletion_guard_pauses_when_threshold_tripped() {
    let fx = boot().await;
    // Five files; deleting all five trips the default 25% fractional
    // threshold (and the absolute threshold of 100 does NOT trip on
    // five — which is the point of testing the fractional rule).
    for i in 0..5 {
        write(&fx.folder.join(format!("f{i}.md")), &format!("content {i}"));
    }
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    // Delete every file.
    for i in 0..5 {
        std::fs::remove_file(fx.folder.join(format!("f{i}.md"))).unwrap();
    }
    let outcome = fx.worker.run_once(&id).await.expect("sweep");
    assert!(
        matches!(outcome, WorkerOutcome::PausedByGuard(_)),
        "expected guard to trip on 5/5 deletions, got {outcome:?}"
    );

    // State must reflect the pause; no tombstones recorded (the
    // deletion phase didn't run).
    let state = WatchedFolderState::load(&fx.manager.index_dir_root().join(&id))
        .expect("state load")
        .expect("state file exists");
    assert!(
        state.tombstones.is_empty(),
        "no tombstones should be recorded when guard trips"
    );
    assert!(
        matches!(
            state.status,
            sovereign_tools::local_corpus::watched::status::WatchedFolderStatus::PausedAwaitingConfirmation { .. }
        ),
        "status should transition to PausedAwaitingConfirmation, got {:?}",
        state.status
    );

    // Confirm clears the pause; the next sweep re-walks (per Q3:
    // re-walk on confirm rather than replay the stale diff) and
    // applies whatever the current diff is.
    fx.manager
        .confirm_pending_deletion(&id)
        .await
        .expect("confirm");
    let outcome2 = fx.worker.run_once(&id).await.expect("post-confirm sweep");
    match outcome2 {
        WorkerOutcome::Applied(summary) => {
            assert_eq!(
                summary.removed, 5,
                "post-confirm sweep applies the pending deletes"
            );
        }
        other => panic!("expected Applied after confirm, got {other:?}"),
    }
}

#[tokio::test]
async fn unsupported_extensions_surface_in_skipped_breakdown() {
    let fx = boot().await;
    // Indexed file (md) plus three files in unsupported formats.
    // .docx / .epub / .html landed as v1 supported formats, so we
    // pick formats outside that breadth (e.g. .rtf, .pages, no-ext)
    // to exercise the skipped bucket.
    write(&fx.folder.join("a.md"), "indexed content");
    write(&fx.folder.join("b.rtf"), "[fake rtf]");
    write(&fx.folder.join("c.rtf"), "[also fake]");
    write(&fx.folder.join("d.pages"), "[Apple Pages]");
    write(&fx.folder.join("README"), "no extension at all");
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("sweep");

    let state = fx.manager.watched_state(&id).await.expect("watched_state");
    assert_eq!(
        state.skipped_by_extension.get("rtf").copied().unwrap_or(0),
        2,
        "two .rtf files should appear in skipped_by_extension; got {:?}",
        state.skipped_by_extension
    );
    assert_eq!(
        state
            .skipped_by_extension
            .get("pages")
            .copied()
            .unwrap_or(0),
        1
    );
    assert_eq!(
        state
            .skipped_by_extension
            .get("(no extension)")
            .copied()
            .unwrap_or(0),
        1,
        "extension-less files bucket as `(no extension)`"
    );
}

/// Phase A end-to-end: a folder with one file per supported format
/// (PDF deferred — pdf-extract requires a real PDF, exercised by the
/// dedicated PDF tests) ingests cleanly, every file lands in
/// `state.entries`, and `state.failed_files` / `state.skipped_by_extension`
/// stay empty for the v1 format breadth.
#[tokio::test]
async fn v1_format_breadth_ingests_cleanly() {
    use std::io::Write as _;

    let fx = boot().await;

    // .txt / .md
    write(&fx.folder.join("note.txt"), "plain text body");
    write(&fx.folder.join("note.md"), "# md heading\n\nbody");

    // .html / .htm / .mhtml — all routed through the same body-text
    // routine; .mhtml needs a tiny multipart MIME envelope.
    write(
        &fx.folder.join("page.html"),
        "<html><body><p>html body</p></body></html>",
    );
    write(
        &fx.folder.join("page.htm"),
        "<html><body><p>htm body</p></body></html>",
    );
    write(
        &fx.folder.join("archive.mhtml"),
        "From: <browser>\r\n\
         Subject: x\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/related; boundary=\"B\"\r\n\
         \r\n\
         --B\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         \r\n\
         <html><body><p>mhtml body</p></body></html>\r\n\
         --B--\r\n",
    );

    // .epub via rbook builder.
    {
        use rbook::epub::EpubChapter;
        let epub = rbook::Epub::builder()
            .title("Sample")
            .language("en")
            .chapter([EpubChapter::new("C1").xhtml_body("<p>epub body</p>")])
            .build();
        let f = std::fs::File::create(fx.folder.join("book.epub")).unwrap();
        epub.write().write(f).expect("write epub");
    }

    // .docx via hand-rolled minimal zip.
    {
        use zip::write::FileOptions;
        use zip::CompressionMethod;
        let f = std::fs::File::create(fx.folder.join("doc.docx")).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opts: FileOptions<()> =
            FileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
        )
        .unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
        )
        .unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body><w:p><w:r><w:t>docx body</w:t></w:r></w:p></w:body>
</w:document>"#,
        )
        .unwrap();
        zip.finish().unwrap();
    }

    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("sweep");

    let state = fx.manager.watched_state(&id).await.expect("watched_state");
    assert_eq!(
        state.entries.len(),
        7,
        "all 7 v1 format files should be entries; got {} entries: {:?}",
        state.entries.len(),
        state.entries.keys().collect::<Vec<_>>()
    );
    assert!(
        state.failed_files.is_empty(),
        "no failures expected on the v1 format breadth happy-path; got {:?}",
        state.failed_files
    );
    assert!(
        state.skipped_by_extension.is_empty(),
        "no skipped extensions expected on the v1 format breadth; got {:?}",
        state.skipped_by_extension
    );
}

/// Folder-ingest v1 §3.4 + ARCH §7.4 (defence in depth):
/// `LocalCorpusManager` implements `SensitiveCorpusOracle`. The
/// runtime's ambient retrieval path (`search_corpus_indexes`)
/// consults this oracle before fanning out, so a folder marked
/// sensitive is structurally absent from pre-turn context
/// assembly. Pinned here so a refactor that drops the impl OR
/// silently inverts the flag fails before the privacy invariant
/// reaches a user.
#[tokio::test]
async fn sensitive_oracle_only_returns_sensitive_corpora() {
    let fx = boot().await;
    let extra_root = fx._tmp.path().join("watched-sensitive");
    std::fs::create_dir_all(&extra_root).unwrap();

    // Register two corpora: one sensitive, one not.
    let mut sensitive_cfg = WatchedFolderConfig::default();
    sensitive_cfg.sensitive = true;
    let sensitive_lc = LocalCorpusConfig::watched_folder(
        extra_root.clone(),
        "Sensitive notes".into(),
        sensitive_cfg,
    );
    let sensitive_id = fx
        .manager
        .register(sensitive_lc)
        .await
        .expect("register sensitive");

    let normal_id = register(&fx, WatchedFolderConfig::default()).await;

    let sensitive_set: std::collections::HashSet<String> = (*fx.manager)
        .sensitive_corpus_ids()
        .await
        .into_iter()
        .collect();

    assert!(
        sensitive_set.contains(&sensitive_id),
        "sensitive corpus must surface in oracle: got {sensitive_set:?}"
    );
    assert!(
        !sensitive_set.contains(&normal_id),
        "non-sensitive corpus must NOT surface: got {sensitive_set:?}"
    );
    assert_eq!(
        sensitive_set.len(),
        1,
        "only the sensitive corpus should be reported; got {sensitive_set:?}"
    );
}

/// The default-defaults case: nothing is sensitive when the user
/// hasn't opted in. Pinned because the structural-privacy flag
/// must default to OFF — a refactor that flips the default to
/// `true` would over-block ambient retrieval and degrade the
/// product silently. ARCH §7.2.
#[tokio::test]
async fn sensitive_oracle_empty_when_no_sensitive_corpora() {
    let fx = boot().await;
    let _id = register(&fx, WatchedFolderConfig::default()).await;
    let set = (*fx.manager).sensitive_corpus_ids().await;
    assert!(
        set.is_empty(),
        "default-defaults oracle must report no sensitive corpora; got {set:?}"
    );
}

#[tokio::test]
async fn watched_incomplete_jobs_excludes_idle_corpora() {
    let fx = boot().await;
    write(&fx.folder.join("a.md"), "alpha");
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up");

    // Idle corpus: should NOT appear in incomplete-jobs.
    let jobs = fx.manager.watched_incomplete_jobs().await;
    assert!(
        jobs.is_empty(),
        "idle corpus should not surface as incomplete; got {jobs:?}"
    );

    // Pause it: should appear.
    fx.manager
        .pause_watched(&id, "test".into())
        .await
        .expect("pause");
    let jobs = fx.manager.watched_incomplete_jobs().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].corpus_id, id);
    assert!(matches!(
        jobs[0].status,
        sovereign_tools::local_corpus::watched::status::WatchedFolderStatus::PausedManual { .. }
    ));
}

#[tokio::test]
async fn watched_folder_factory_projects_with_ocr_onto_local_corpus() {
    use sovereign_tools::local_corpus::config::WatchedFolderConfig;
    let mut wf = WatchedFolderConfig::default();
    wf.with_ocr = true;
    let cfg =
        LocalCorpusConfig::watched_folder(PathBuf::from("/tmp/ocr-watched"), "ocr".into(), wf);
    assert!(
        cfg.ocr_pdfs,
        "watched_folder factory must project WatchedFolderConfig.with_ocr onto LocalCorpusConfig.ocr_pdfs"
    );
}

#[tokio::test]
async fn watched_folder_with_ocr_off_keeps_default() {
    let cfg = LocalCorpusConfig::watched_folder(
        PathBuf::from("/tmp/no-ocr"),
        "no-ocr".into(),
        WatchedFolderConfig::default(),
    );
    assert!(
        !cfg.ocr_pdfs,
        "default WatchedFolderConfig leaves ocr_pdfs off"
    );
}

#[tokio::test]
async fn pause_then_resume_skips_then_runs() {
    let fx = boot().await;
    write(&fx.folder.join("a.md"), "alpha");
    let id = register(&fx, WatchedFolderConfig::default()).await;
    fx.manager.ingest(&id, None, None).await.expect("ingest");
    fx.worker.run_once(&id).await.expect("warm-up sweep");

    fx.manager
        .pause_watched(&id, "test".into())
        .await
        .expect("pause");
    let outcome = fx.worker.run_once(&id).await.expect("paused sweep");
    assert!(
        matches!(
            outcome,
            WorkerOutcome::Skipped(
                sovereign_tools::local_corpus::watched::worker::SkipReason::PausedManually
            )
        ),
        "expected Skipped(PausedManually), got {outcome:?}"
    );

    fx.manager.resume_watched(&id).await.expect("resume");
    let outcome2 = fx.worker.run_once(&id).await.expect("resumed sweep");
    assert!(
        !matches!(outcome2, WorkerOutcome::Skipped(_)),
        "after resume, sweep should run; got {outcome2:?}"
    );
}
