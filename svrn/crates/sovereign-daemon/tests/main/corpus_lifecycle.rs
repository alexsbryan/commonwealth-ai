// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration test for the corpus install / cancel / reinstall lifecycle.
//!
//! Covers the unified-ingest flow end-to-end at the HTTP boundary:
//!
//!  1. `POST /internal/corpus/install` starts an ingest. The task is
//!     registered in `active_ingests` and writes progress into the
//!     shared `corpus_progress` map.
//!  2. `GET /internal/corpus/progress` reflects the current phase.
//!  3. `POST /internal/corpus/cancel` fires the cancellation, waits for
//!     the ingest to exit, and asks ingest to wipe the corpus.
//!  4. A second `POST /internal/corpus/install` for the same corpus
//!     starts cleanly.
//!
//! Split at the port (pb-ingest-dial-daemon-tests, phase-b-52): the ingest
//! is ingest's, reached through `IngestPort`, so the node holds
//! `IngestPortDouble` programmed by [`Ingests`] — a registry install whose
//! run is held open until cancelled, completes, or fails, and the on-disk
//! status the runs leave. These readings assert the daemon's own
//! bookkeeping and the port calls it makes. What ingest does with the same
//! asks — the partition promoted to a finalised canonical, a pause that
//! keeps the partition and resumes from it, the wipe, the refusal of an
//! unknown recipe, a failed ingest leaving no canonical, the article
//! sampler's estimate and sidecar — is corpus-engine's
//! install_lifecycle_port_parity, over this file's former fixtures.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use corpus_index::ingest_port::daemon::{
    ArticleStats, CorpusDiskStatus, IngestResult, InstallRefusal, PreparedInstall,
};
use corpus_index::ingest_port::double::IngestPortDouble;
use kernel_types::NodeId;
use sovereign_contracts::daemon_wire::IngestProgress;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::{fabric, node, serving, AppState};
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::common::ledger_double::RecordingLedger;

/// What the double's registry install does when the daemon runs it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    /// Report progress, hold a partition in progress, and return only when
    /// the port's cancel reaches it.
    HeldUntilCancelled,
    /// Report progress and finish with a ready canonical.
    Completes,
    /// Fail the way a mid-install embed failure does.
    Fails,
}

/// The registry install the node's port double runs, and the on-disk
/// status those runs leave, shared across the fresh states a test builds
/// (the reinstall and the resume each build one, as before the split).
struct Ingests {
    known: HashSet<String>,
    run: Mutex<Run>,
    disk: Mutex<HashMap<String, CorpusDiskStatus>>,
    held: Mutex<HashMap<String, Arc<Notify>>>,
    /// The port's `index_dir`. A completed install reads it on the spot to
    /// place a staged SEC fact store (`corpus_ingest.rs`); empty, so nothing
    /// is staged and nothing is placed.
    indexes: tempfile::TempDir,
}

fn absent(corpus_id: &str) -> CorpusDiskStatus {
    CorpusDiskStatus {
        corpus_id: corpus_id.to_string(),
        canonical_present: false,
        partition_present: false,
        canonical_in_progress: false,
        partition_in_progress: false,
        committed_iter_pos: 0,
        shards_completed: Vec::new(),
        shards_total: 0,
    }
}

impl Ingests {
    /// Installs of `known` resolve; any other id is not in the registry.
    fn new(known: &[&str], run: Run) -> Arc<Self> {
        Arc::new(Self {
            known: known.iter().map(|s| s.to_string()).collect(),
            run: Mutex::new(run),
            disk: Mutex::default(),
            held: Mutex::default(),
            indexes: tempfile::tempdir().expect("an index dir"),
        })
    }

    fn set_run(&self, run: Run) {
        *self.run.lock().unwrap() = run;
    }

    fn mark(&self, corpus_id: &str, edit: impl FnOnce(&mut CorpusDiskStatus)) {
        let mut disk = self.disk.lock().unwrap();
        edit(
            disk.entry(corpus_id.to_string())
                .or_insert_with(|| absent(corpus_id)),
        );
    }

    fn prepare(self: &Arc<Self>, corpus_id: &str) -> Result<PreparedInstall, InstallRefusal> {
        if !self.known.contains(corpus_id) {
            return Err(InstallRefusal::RecipeNotFound(format!(
                "No registry entry for corpus {corpus_id}"
            )));
        }
        let (me, id, run) = (
            Arc::clone(self),
            corpus_id.to_string(),
            *self.run.lock().unwrap(),
        );
        Ok(PreparedInstall {
            // The post-install atlas pass is not these tests' subject.
            opts_out_of_auto_enrichment: true,
            run: Box::new(move |progress| {
                Box::pin(async move {
                    if run == Run::Fails {
                        return Err(corpus_index::Error::Embed(
                            "simulated mid-install embed failure".into(),
                        ));
                    }
                    me.mark(&id, |d| {
                        d.partition_present = true;
                        d.partition_in_progress = true;
                    });
                    if let Some(progress) = &progress {
                        progress(IngestProgress::Embedding {
                            chunks_embedded: 1,
                            total: 600,
                            docs_processed: 1,
                            chunks_per_sec: 1.0,
                            expected_docs: None,
                        });
                    }
                    if run == Run::HeldUntilCancelled {
                        let held = Arc::new(Notify::new());
                        me.held
                            .lock()
                            .unwrap()
                            .insert(id.clone(), Arc::clone(&held));
                        held.notified().await;
                        return Err(corpus_index::Error::Cancelled(id));
                    }
                    me.mark(&id, |d| {
                        d.partition_present = false;
                        d.partition_in_progress = false;
                        d.canonical_present = true;
                    });
                    Ok(IngestResult {
                        corpus_id: id,
                        chunks_created: 600,
                        index_size_bytes: 0,
                        duration_secs: 0,
                        docs_skipped: 0,
                    })
                })
            }),
        })
    }

    /// The port's cancel: signals a held run, and says whether there was one.
    fn cancel(&self, corpus_id: &str) -> bool {
        match self.held.lock().unwrap().remove(corpus_id) {
            Some(held) => {
                held.notify_one();
                true
            }
            None => false,
        }
    }

    fn double(self: &Arc<Self>) -> IngestPortDouble {
        let (prepare, cancel, wipe, disk) = (
            Arc::clone(self),
            Arc::clone(self),
            Arc::clone(self),
            Arc::clone(self),
        );
        IngestPortDouble::new()
            .with_index_dir(self.indexes.path())
            .on_prepare_registry_install(move |id| prepare.prepare(id))
            .on_cancel_corpus_ingest(move |id| cancel.cancel(id))
            .on_remove_corpus_everything(move |id| {
                wipe.disk.lock().unwrap().remove(id);
                Ok(())
            })
            .on_corpus_disk_status(move |id| {
                disk.disk
                    .lock()
                    .unwrap()
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| absent(id))
            })
            .with_in_progress_ingestions(Vec::new())
            .on_cached_article_stats(|_| None)
            .on_compute_article_stats(|_| None)
    }
}

/// An `AppState` whose corpus handle is `engine`, and the handle itself so
/// a test reads the port calls it made.
fn test_state(engine: IngestPortDouble) -> (AppState, Arc<IngestPortDouble>) {
    let engine = Arc::new(engine);
    (state_over_double(Arc::clone(&engine)), engine)
}

/// An AppState over the store-free recording double (five-programs fp-85).
fn state_over_double(engine: Arc<IngestPortDouble>) -> AppState {
    let self_id = NodeId::from_u128(1);
    AppState::new_with_seeds(
        self_id,
        Some(engine),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed::default(),
        Arc::new(RecordingLedger::new(self_id)).seed(),
    )
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
        // Both internal listeners attach `ConnectInfo`
        // (`daemon.rs`/`server.rs`), and `internal_gate` reads a MISSING one as
        // "not loopback" and refuses — the same fail-closed reading
        // `internal_principal` takes. A driver with no peer address is a shape
        // production never has, so say the local one here.
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            54321,
        ))))
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
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            54321,
        ))))
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

/// Poll until the daemon's task for `corpus_id` has left `active_ingests`.
async fn wait_until_idle(state: &AppState, corpus_id: &str, label: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while state
        .inner
        .ingest
        .active_ingests
        .read()
        .await
        .contains(corpus_id)
    {
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_until_idle timed out at '{label}'");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The calls among `names` the port saw, in order.
fn calls_among(engine: &IngestPortDouble, names: &[&str]) -> Vec<&'static str> {
    engine
        .calls()
        .into_iter()
        .filter(|c| names.contains(c))
        .collect()
}

async fn install(state: &AppState, corpus_id: &str) -> (StatusCode, Vec<u8>) {
    post_json(
        internal_router(state.clone()),
        "/internal/corpus/install",
        &serde_json::json!({ "corpus_id": corpus_id }),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_cancel_reinstall_lifecycle() {
    let corpus_id = "testcorpus";
    let ingests = Ingests::new(&[corpus_id], Run::HeldUntilCancelled);

    // ── Phase 1: install, held open so cancel has room to land ──────
    let (state, engine) = test_state(ingests.double());

    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "install returned non-OK: {:?}",
        String::from_utf8_lossy(&body)
    );
    let install_resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(
        install_resp.spawned,
        "first install should report spawned=true"
    );
    assert_eq!(install_resp.corpus_id, corpus_id);

    // A second install immediately afterwards must be idempotent.
    let (_, body) = install(&state, corpus_id).await;
    let dup_resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(
        !dup_resp.spawned,
        "second install must not spawn a duplicate task"
    );
    assert_eq!(
        calls_among(&engine, &["prepare_registry_install"]).len(),
        1,
        "the idempotent second install never reaches ingest"
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

    // Status endpoint should expose the same corpus with a fused
    // on-disk + progress view. This is the data path the Desktop
    // poller consumes so the UI reflects daemon-owned ingests even
    // when Desktop didn't initiate them.
    let (status, body) = get(internal_router(state.clone()), "/internal/corpus/status").await;
    assert_eq!(status, StatusCode::OK);
    let status_resp: StatusResponse = serde_json::from_slice(&body).unwrap();
    let entry = status_resp
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .expect("status response must include our active corpus");
    assert!(
        entry.active,
        "entry should be marked active while ingesting"
    );
    assert!(
        entry.partition_in_progress,
        "entry should carry ingest's on-disk reading of the partition in progress"
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
    assert!(
        calls_among(
            &engine,
            &["cancel_corpus_ingest", "remove_corpus_everything"]
        )
        .is_empty(),
        "a refused cancel must not reach ingest"
    );

    // ── Phase 2b: cancel + wipe (with explicit confirm) ─────────────
    let (status, body) = post_json(
        internal_router(state.clone()),
        "/internal/corpus/cancel",
        &serde_json::json!({ "corpus_id": corpus_id, "confirm_wipe": true }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cancel returned non-OK: {:?}",
        String::from_utf8_lossy(&body)
    );
    let cancel_resp: CancelResp = serde_json::from_slice(&body).unwrap();
    assert!(cancel_resp.wiped, "cancel should report the wipe completed");
    assert_eq!(
        calls_among(
            &engine,
            &["cancel_corpus_ingest", "remove_corpus_everything"]
        ),
        vec!["cancel_corpus_ingest", "remove_corpus_everything"],
        "cancel stops the ingest, THEN asks ingest to wipe the corpus"
    );
    assert!(
        state
            .inner
            .ingest
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
            .ingest
            .corpus_progress
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "corpus_progress entry should have been cleared"
    );

    // ── Phase 3: reinstall on a fresh state → end-to-end completion ──
    ingests.set_run(Run::Completes);
    let (state, engine) = test_state(ingests.double());
    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reinstall returned non-OK: {:?}",
        String::from_utf8_lossy(&body)
    );
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(resp.spawned, "reinstall should spawn a fresh task");
    wait_until_idle(&state, corpus_id, "reinstall ran to completion").await;
    assert_eq!(
        calls_among(&engine, &["prepare_registry_install"]),
        vec!["prepare_registry_install"],
    );
    // The progress write is a spawned task (`ingest_progress_callback`), so
    // the task can leave `active_ingests` before its progress lands: probed
    // 2026-10-04, 2 of 40 reads right after idle saw no entry yet. Wait for
    // it, as phase 1 does, before asking status to report it.
    wait_until_progress(
        &state,
        |snap| snap.progress.contains_key(corpus_id),
        Duration::from_secs(10),
        "reinstall progress visible",
    )
    .await;
    let (_, body) = get(internal_router(state.clone()), "/internal/corpus/status").await;
    let statuses: StatusResponse = serde_json::from_slice(&body).unwrap();
    let entry = statuses
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .expect("the completed install is still reported");
    assert!(!entry.active, "the finished task no longer claims to run");
}

/// Companion to [`install_cancel_reinstall_lifecycle`] — verifies the
/// non-destructive `/internal/corpus/pause` route stops an in-flight
/// ingest cleanly while never asking ingest to wipe, and that a
/// subsequent `/internal/corpus/install` starts again.
///
/// This is the regression guard for the accidental-wipe incident:
/// before pause existed, the only way to stop an ingest was a route
/// that destroyed every committed chunk on the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_pause_resume_lifecycle() {
    let corpus_id = "pausetest";
    let ingests = Ingests::new(&[corpus_id], Run::HeldUntilCancelled);

    // ── Phase 1: install, held open so pause has room to land ───────
    let (state, engine) = test_state(ingests.double());
    let (status, _body) = install(&state, corpus_id).await;
    assert_eq!(status, StatusCode::OK, "install returned non-OK");

    wait_until_progress(
        &state,
        |snap| snap.progress.contains_key(corpus_id),
        Duration::from_secs(10),
        "ingest progress visible",
    )
    .await;

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
    let pause_resp: PauseResp = serde_json::from_slice(&body).unwrap();
    assert!(
        pause_resp.cancel_signalled,
        "the held ingest was in flight, so the cancel reached it"
    );

    // The whole point of pause: data must survive.
    assert_eq!(
        calls_among(
            &engine,
            &["cancel_corpus_ingest", "remove_corpus_everything"]
        ),
        vec!["cancel_corpus_ingest"],
        "pause must NOT ask ingest to wipe — that's the regression we're guarding"
    );
    // /pause synchronously waits up to 5 s for the ingest task to clear
    // out of `active_ingests` before returning.
    assert!(
        state
            .inner
            .ingest
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
            .ingest
            .corpus_progress
            .read()
            .await
            .get(corpus_id)
            .is_none(),
        "corpus_progress should be cleared after pause"
    );

    // ── Phase 3: resume by re-installing on a fresh state ───────────
    ingests.set_run(Run::Completes);
    let (state, _engine) = test_state(ingests.double());
    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(status, StatusCode::OK, "resume install returned non-OK");
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(resp.spawned, "the paused corpus installs again");
    wait_until_idle(&state, corpus_id, "resumed ingest ran to completion").await;
}

/// The "daemon resumed a legacy canonical ingest after the Desktop
/// session closed" scenario — no active ingest, no recent
/// `IngestProgress` event, just ingest's on-disk reading. The first
/// `/status` poll finds no sampled stats and asks ingest to compute
/// them; a later poll publishes the section-based `estimated_fraction`
/// the daemon derives from them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_sampler_publishes_estimated_fraction_on_resume() {
    let corpus_id = "testcorpus";
    // What ingest reads off the fixture this test wrote until the split:
    // a canonical in progress at committed_iter_pos 30, and 50 articles of
    // 3 sections each once sampled.
    let sampled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (cached, computed) = (Arc::clone(&sampled), Arc::clone(&sampled));
    let stats = ArticleStats {
        total_articles: 50,
        mean_sections_per_article: 3.0,
        total_sections_estimate: 150,
        source_mtime_secs: 0,
        source_size_bytes: 0,
        sampled_at_secs: 0,
    };
    let stats_for_cache = stats.clone();
    let engine = IngestPortDouble::new()
        .with_in_progress_ingestions(vec![corpus_id.to_string()])
        .on_corpus_disk_status(|id| CorpusDiskStatus {
            canonical_present: true,
            canonical_in_progress: true,
            committed_iter_pos: 30,
            ..absent(id)
        })
        .on_cached_article_stats(move |_| {
            cached
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| stats_for_cache.clone())
        })
        .on_compute_article_stats(move |_| {
            computed.store(true, std::sync::atomic::Ordering::SeqCst);
            Some(stats.clone())
        });
    let (state, engine) = test_state(engine);

    // First poll: nothing sampled yet. The handler kicks off the sampler
    // in a spawn_blocking task.
    let (status, body) = get(internal_router(state.clone()), "/internal/corpus/status").await;
    assert_eq!(status, StatusCode::OK);
    let resp: StatusResponse = serde_json::from_slice(&body).unwrap();
    let first = resp
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .expect("status must include legacy-resume corpus");
    assert!(first.canonical_in_progress);
    assert_eq!(first.committed_iter_pos, 30);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let entry = loop {
        let (_, body) = get(internal_router(state.clone()), "/internal/corpus/status").await;
        let resp: StatusResponse = serde_json::from_slice(&body).unwrap();
        let entry = resp
            .entries
            .into_iter()
            .find(|e| e.corpus_id == corpus_id)
            .expect("status must still include corpus");
        if entry.estimated_total_sections.is_some() && entry.estimated_fraction.is_some() {
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

    assert!(
        engine.calls().contains(&"compute_article_stats"),
        "an unsampled corpus with committed work asks ingest to sample it"
    );
    assert_eq!(entry.estimated_total_sections, Some(150));
    assert_eq!(entry.estimated_total_articles, Some(50));
    // committed 30 of 150 sampled sections: the daemon's own division.
    let fraction = entry.estimated_fraction.unwrap();
    assert!(
        (fraction - 0.20).abs() < 1e-6,
        "expected 30/150, got {fraction}"
    );
}

/// Installing a corpus whose recipe cannot be resolved (no local
/// override, no catalog entry, no bundled fallback) must fail LOUDLY:
/// the daemon returns 404, not a 200 with `spawned:false` that a client
/// can't tell apart from "already running". Regression guard for the
/// alignment-migrate silent-failure bug — the install POST reported
/// success while the daemon logged `No registry entry for corpus`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_unresolvable_recipe_is_404_not_silent_success() {
    let ingests = Ingests::new(&[], Run::Completes);
    let (state, _engine) = test_state(ingests.double());

    let (status, body) = install(&state, "no-such-corpus-xyz").await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unresolvable recipe must surface as 404, got body: {}",
        String::from_utf8_lossy(&body)
    );
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let msg = err.get("error").and_then(|e| e.as_str()).unwrap_or("");
    assert!(
        msg.contains("no-such-corpus-xyz"),
        "error body should name the corpus, got: {msg}"
    );
    assert!(
        state.inner.ingest.active_ingests.read().await.is_empty(),
        "a refused install must not leave a slot in active_ingests"
    );
}

/// A mid-install failure must be REPORTED as a failure and must keep the
/// corpus visible in `/internal/corpus/status`.
///
/// The bug this guards: `spawn_corpus_install` used to handle a failed
/// ingest with nothing but a `tracing::warn!`. Because it had already
/// removed the corpus from `active_ingests`, and because nothing wrote a
/// terminal record into `corpus_progress`, the corpus vanished from this
/// route's response entirely. The Desktop poller reads "present last
/// tick, absent this tick" as SUCCESS and emits phase=complete /
/// percent=100 / "Done" — so a 401 on a gated snapshot, and every other
/// ingest failure, rendered in the UI as a finished install that had
/// installed nothing. Found while auditing why `sep` could not be added.
///
/// Two assertions carry the fix, and both must hold:
///   1. the progress record is `Failed` with a non-empty message, and
///   2. the corpus is STILL an entry in `/internal/corpus/status` —
///      absence is precisely what the poller mistranslates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_ingest_reports_failed_and_stays_visible() {
    let corpus_id = "failcorpus";
    let ingests = Ingests::new(&[corpus_id], Run::Fails);
    let (state, _engine) = test_state(ingests.double());

    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "install POST itself should succeed — the failure is asynchronous; body: {}",
        String::from_utf8_lossy(&body)
    );
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(resp.spawned, "a task should have spawned");
    assert_eq!(resp.corpus_id, corpus_id);

    // (1) The failure is recorded as a terminal progress entry.
    let snapshot = wait_until_progress(
        &state,
        |s| {
            matches!(
                s.progress.get(corpus_id),
                Some(IngestProgress::Failed { .. })
            )
        },
        Duration::from_secs(60),
        "terminal Failed record",
    )
    .await;
    let Some(IngestProgress::Failed { message }) = snapshot.progress.get(corpus_id) else {
        panic!("expected a Failed record, got {snapshot:?}");
    };
    assert!(
        message.contains("simulated mid-install embed failure"),
        "the failure message is shown to the user verbatim, so it must be ingest's: {message}"
    );

    // (2) The corpus must not have disappeared from /status.
    let (st, body) = get(internal_router(state.clone()), "/internal/corpus/status").await;
    assert_eq!(st, StatusCode::OK);
    let statuses: StatusResponse = serde_json::from_slice(&body).unwrap();
    let entry = statuses
        .entries
        .iter()
        .find(|e| e.corpus_id == corpus_id)
        .unwrap_or_else(|| {
            panic!(
                "a failed corpus MUST remain an entry in /internal/corpus/status — \
                 its disappearance is what the Desktop poller reports as \"Done\". \
                 entries = {:?}",
                statuses.entries
            )
        });
    assert!(
        matches!(entry.progress, Some(IngestProgress::Failed { .. })),
        "the surviving entry must carry the Failed phase, got {:?}",
        entry.progress
    );
    assert!(
        !entry.active,
        "the task is over — it must not still claim to be active"
    );
}

/// Retrying after a failure must retire the stale failure record, so a
/// UI that shows "Install failed" stops showing it once the corpus
/// installs. The `Failed` entry is deliberately sticky (it has to
/// outlive its task to be reportable at all), which makes the retry
/// path responsible for clearing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_after_failure_clears_the_stale_failure_record() {
    let corpus_id = "retrycorpus";
    let ingests = Ingests::new(&[corpus_id], Run::Fails);
    let (state, _engine) = test_state(ingests.double());

    // First attempt: armed to fail.
    let (status, _) = install(&state, corpus_id).await;
    assert_eq!(status, StatusCode::OK);
    wait_until_progress(
        &state,
        |s| {
            matches!(
                s.progress.get(corpus_id),
                Some(IngestProgress::Failed { .. })
            )
        },
        Duration::from_secs(60),
        "first attempt fails",
    )
    .await;

    // Disarm, then retry the same corpus on the same state.
    ingests.set_run(Run::Completes);
    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a retry after failure must be accepted; body: {}",
        String::from_utf8_lossy(&body)
    );
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(
        resp.spawned,
        "the previous failure must not block a retry — the corpus was removed \
         from active_ingests when its task ended"
    );

    // The stale Failed record must be gone: the retry either progresses
    // or completes, but it never keeps reporting the old failure.
    wait_until_progress(
        &state,
        |s| {
            !matches!(
                s.progress.get(corpus_id),
                Some(IngestProgress::Failed { .. })
            )
        },
        Duration::from_secs(60),
        "stale failure cleared by retry",
    )
    .await;
}

/// A benign idempotent second install (corpus already in flight) stays a
/// 200 with `spawned:false` — it is NOT a failure, so it must not be
/// promoted to a 4xx by the new outcome mapping. Guards the boundary
/// between "already active" (fine) and "recipe not found" (error).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_install_while_active_is_200_spawned_false() {
    let corpus_id = "dualpath";
    // A held ingest keeps the first task in `active_ingests` for the
    // second POST to observe.
    let ingests = Ingests::new(&[corpus_id], Run::HeldUntilCancelled);
    let (state, _engine) = test_state(ingests.double());

    let (status1, body1) = install(&state, corpus_id).await;
    assert_eq!(status1, StatusCode::OK, "first install should be 200");
    let first: InstallResp = serde_json::from_slice(&body1).unwrap();
    assert!(first.spawned, "first install should spawn a task");

    // Second install while the first is still running: benign no-op.
    let (status2, body2) = install(&state, corpus_id).await;
    assert_eq!(
        status2,
        StatusCode::OK,
        "already-active install must stay 200, got body: {}",
        String::from_utf8_lossy(&body2)
    );
    let second: InstallResp = serde_json::from_slice(&body2).unwrap();
    assert_eq!(second.corpus_id, corpus_id);
    assert!(
        !second.spawned,
        "already-active install must report spawned:false"
    );
}
