// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/internal/corpus/local/*` — the non-watch half of
//! `LocalCorpusManager`, end to end (sv-surface D5).
//!
//! Every case drives the daemon's OWN manager through
//! `watched_folder_runtime`, the same singleton `corpus_watch_http_e2e`
//! drives. That is the point of the rung: the desktop's second manager
//! is what these routes exist to retire, so a fixture that built a
//! third one here would prove nothing about the twin.
//!
//! # Singleton constraint
//!
//! `watched_folder_runtime` keeps the manager in a process-global
//! `OnceLock`, this crate links ONE test binary, and
//! `corpus_watch_http_e2e` installs into the same slot. Whichever file
//! runs first wins, and `install` is a silent no-op for the loser — so
//! this harness installs opportunistically and then works through
//! `watched_folder_runtime::manager()`, the handle the HANDLERS read.
//! It never assumes its own manager won.
//!
//! # NOT covered here, named rather than implied (ARCH §18.1)
//!
//! - The **503** branch (`manager_or_503`). A `OnceLock` cannot be
//!   un-set, so within this binary there is no ordering that reaches
//!   an uninstalled runtime. The branch is real and reachable in
//!   production (a `sovereign daemon run` that never installed the
//!   local-corpus runtime); it has no test here and this file does not
//!   pretend otherwise.
//! - `write-tags`, `rollback`, `clean` and `preview` need a CLUSTERED
//!   vault, which is a fixture this file does not build. They are
//!   asserted for route + reach only: the body must name the manager
//!   method that refused, which an unmounted route cannot do.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! The fourteen routes were taken back OUT of `lc_router` — handlers,
//! DTOs and this file left in place, so the suite still built — and
//! every case re-run against the routerless daemon. ALL FIVE failed,
//! `pass: 0 fail: 5`:
//!
//! ```text
//! list_and_get_serve_the_daemons_own_manager     :161  left: 404  right: 200
//! an_unregistered_corpus_is_a_404_naming_it      :205  "the 404 must NAME
//!                                                       the corpus … Null"
//! cancel_and_git_report_facts_…                  :245  left: 404  right: 200
//! the_vault_routes_reach_the_manager_…           :298  left: 404  right: 500
//! ingest_is_a_job_that_names_its_progress_route  :333  left: 404  right: 202
//! ```
//!
//! `an_unregistered_corpus_…` is the one to read twice: its STATUS
//! line passed under sabotage — a routerless daemon 404s too — and
//! what went red is the line after it, the body that must name the
//! corpus. An empty 404 body cannot (ARCH §18.1). The other four are
//! red on a status no absent route produces (200 / 202 / 500).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::traits::StateStore;
use sovereign_mesh::lc_http::lc_router;
use sovereign_mesh::watched_folder_runtime;
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::local_corpus::config::LocalCorpusConfig;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::LocalCorpusManager;

use crate::common::spawn_router;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Install the singleton if it is still empty, then hand back the
/// manager the HANDLERS will read plus the router's address.
#[allow(clippy::unwrap_used)]
async fn harness() -> (Arc<LocalCorpusManager>, SocketAddr) {
    if watched_folder_runtime::manager().is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
        std::fs::create_dir_all(data_dir.join("recipes")).unwrap();
        // The singleton holds paths into this dir for the process
        // lifetime; dropping the guard would pull them out from under
        // it (`corpus_watch_http_e2e`'s reason, same fix).
        std::mem::forget(tmp);
        let store: Arc<InMemoryStateStore> = Arc::new(InMemoryStateStore::new());
        let engine = Arc::new(
            CorpusEngine::new(
                data_dir.join("recipes"),
                data_dir.join("indexes"),
                mock_embed_fn(),
            )
            .with_embedding_model("test-mock"),
        );
        let manager = Arc::new(
            LocalCorpusManager::init(
                engine,
                store as Arc<dyn StateStore>,
                None,
                data_dir.clone(),
                data_dir.join("vault-snapshots"),
            )
            .await
            .expect("manager init"),
        );
        watched_folder_runtime::install(manager, Arc::new(WatchedFolderRegistry::new()));
    }
    let manager = watched_folder_runtime::manager().expect("the singleton is installed by now");
    // A FRESH listener per test. `spawn_router`'s accept loop lives on
    // the calling test's tokio runtime, and `#[tokio::test]` drops that
    // runtime when the test returns — a shared address would be
    // connection-refused for every test after the first. The router is
    // stateless (it reads the singleton), so re-spawning costs a port.
    let addr = spawn_router(lc_router()).await;
    (manager, addr)
}

/// Register one document folder through the manager the handlers read,
/// and hand back its corpus id.
#[allow(clippy::unwrap_used)]
async fn register_folder(manager: &Arc<LocalCorpusManager>, tag: &str) -> (String, PathBuf) {
    let root = manager.index_dir_root();
    let folder = root
        .parent()
        .unwrap_or(&root)
        .join(format!("lc-fixture-{tag}"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("note.md"), "quiet hours begin at 11 PM").unwrap();
    let cfg = LocalCorpusConfig::document_folder(folder.clone(), format!("Fixture {tag}"));
    let id = manager.register(cfg).await.expect("register");
    (id, folder)
}

/// Write a document folder holding `files` named documents and mint its
/// config — WITHOUT registering it anywhere. The `files` count is what
/// makes a terminal `files_indexed` a number a hard-coded `1` cannot
/// fake; the un-registered half is what lets a caller drive the register
/// route the way the desktop does.
#[allow(clippy::unwrap_used)]
fn folder_fixture(
    manager: &Arc<LocalCorpusManager>,
    tag: &str,
    files: usize,
) -> (LocalCorpusConfig, PathBuf) {
    let root = manager.index_dir_root();
    let folder = root
        .parent()
        .unwrap_or(&root)
        .join(format!("lc-fixture-{tag}"));
    std::fs::create_dir_all(&folder).unwrap();
    for n in 0..files {
        std::fs::write(
            folder.join(format!("doc-{n}.txt")),
            format!("document {n}: quiet hours begin at 11 PM"),
        )
        .unwrap();
    }
    let cfg = LocalCorpusConfig::document_folder(folder.clone(), format!("Fixture {tag}"));
    (cfg, folder)
}

/// Register a document folder holding `files` named documents, so a
/// terminal `files_indexed` is a number a hard-coded `1` cannot fake.
#[allow(clippy::unwrap_used)]
async fn register_folder_with_files(
    manager: &Arc<LocalCorpusManager>,
    tag: &str,
    files: usize,
) -> (String, PathBuf) {
    let (cfg, folder) = folder_fixture(manager, tag, files);
    let id = manager.register(cfg).await.expect("register");
    (id, folder)
}

async fn get(addr: SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .expect("lc_router reachable");
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn post(addr: SocketAddr, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .json(&body)
        .send()
        .await
        .expect("lc_router reachable");
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The registry reads answer the DAEMON's manager — the corpus this
/// test registered through the singleton comes back over HTTP with the
/// display name and the root it was registered with.
#[tokio::test]
async fn list_and_get_serve_the_daemons_own_manager() {
    let (manager, addr) = harness().await;
    let (id, folder) = register_folder(&manager, "list").await;

    let (status, body) = get(addr, "/internal/corpus/local").await;
    assert_eq!(status, 200, "list: {body:#?}");
    let rows = body.as_array().expect("an array of configs");
    let row = rows
        .iter()
        .find(|r| r["id"] == serde_json::json!(id))
        .unwrap_or_else(|| panic!("the registered corpus must be in the list: {body:#?}"));
    assert_eq!(
        row["display_name"], "Fixture list",
        "the config is the MANAGER's row, not a stub: {row:#?}"
    );

    let (status, one) = get(addr, &format!("/internal/corpus/local/{id}")).await;
    assert_eq!(status, 200, "get: {one:#?}");
    assert_eq!(one["id"], serde_json::json!(id));
    assert!(
        serde_json::to_string(&one)
            .unwrap_or_default()
            .contains(&folder.to_string_lossy().to_string()),
        "the single-corpus read carries the registered ROOT — a defaulted \
         config would not: {one:#?}"
    );

    let (status, ocr) = get(addr, "/internal/corpus/local/ocr-available").await;
    assert_eq!(status, 200, "ocr: {ocr:#?}");
    assert!(
        ocr["available"].is_boolean(),
        "the answer is a NAMED field, so 'no OCR' cannot be confused with \
         'this daemon did not understand the question': {ocr:#?}"
    );

    let (status, jobs) = get(addr, "/internal/corpus/local/incomplete-jobs").await;
    assert_eq!(status, 200, "incomplete-jobs: {jobs:#?}");
    assert!(jobs.is_array(), "incomplete jobs is a list: {jobs:#?}");
}

/// A corpus the manager does not carry is a 404 that NAMES it, on the
/// read and on the job submit — never a 200 with a defaulted config,
/// and never a job id for a corpus that does not exist.
#[tokio::test]
async fn an_unregistered_corpus_is_a_404_naming_it() {
    let (_manager, addr) = harness().await;

    let (status, body) = get(addr, "/internal/corpus/local/no-such-corpus").await;
    assert_eq!(status, 404, "get: {body:#?}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-corpus"),
        "the 404 must NAME the corpus — the status alone is not a gate, a \
         routerless daemon 404s too: {body:#?}"
    );

    let (status, body) = post(
        addr,
        "/internal/corpus/local/no-such-corpus/ingest",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status, 404,
        "an ingest submit must refuse an unknown corpus on THIS response, \
         not minutes later in a log: {body:#?}"
    );
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("no-such-corpus"));
}

/// `cancel` reports whether there WAS something to cancel, and `git`
/// answers an explicit `null` for a folder that is not a repository —
/// two facts a bare `ok: true` would flatten.
#[tokio::test]
async fn cancel_and_git_report_facts_a_bare_ack_would_flatten() {
    let (manager, addr) = harness().await;
    let (id, _folder) = register_folder(&manager, "cancel").await;

    let (status, body) = post(
        addr,
        &format!("/internal/corpus/local/{id}/cancel"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "cancel: {body:#?}");
    assert_eq!(body["corpus_id"], serde_json::json!(id));
    assert_eq!(
        body["cancelled"], false,
        "nothing was running, so `cancelled` is false — a successful call \
         that stopped nothing: {body:#?}"
    );

    let (status, body) = get(addr, &format!("/internal/corpus/local/{id}/git")).await;
    assert_eq!(status, 200, "git: {body:#?}");
    assert!(
        body.is_null(),
        "a plain folder is not a git worktree, and that is an explicit \
         null — not an invented clean status: {body:#?}"
    );

    // A plain document folder is not configured for write-back at all,
    // which the manager reports by name. The route passes that through
    // rather than answering an empty list — "no snapshots" and "this
    // corpus never takes snapshots" are different facts (§18.3).
    let (status, body) = get(addr, &format!("/internal/corpus/local/{id}/snapshots")).await;
    assert_eq!(status, 500, "snapshots: {body:#?}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("list_snapshots:"),
        "the body must name `list_snapshots` — an unmounted route's empty          404 cannot: {body:#?}"
    );
}

/// The four vault-mutating routes need a clustered vault this fixture
/// does not build. What IS proven here is that each is mounted and
/// reaches the manager: the refusal NAMES the manager method, which an
/// unmounted route's empty 404 cannot.
#[tokio::test]
async fn the_vault_routes_reach_the_manager_and_name_what_refused() {
    let (manager, addr) = harness().await;
    let (id, _folder) = register_folder(&manager, "vault").await;

    for (path, body, method) in [
        (
            format!("/internal/corpus/local/{id}/preview"),
            serde_json::json!({}),
            "get_preview",
        ),
        (
            format!("/internal/corpus/local/{id}/rollback"),
            serde_json::json!({ "snapshot_path": "/nonexistent/snap" }),
            "rollback",
        ),
    ] {
        let (status, answer) = post(addr, &path, body).await;
        assert_eq!(
            status, 500,
            "{path}: an unclustered vault refuses, and the route must report \
             the refusal rather than a well-formed empty: {answer:#?}"
        );
        assert!(
            answer["error"]
                .as_str()
                .unwrap_or_default()
                .starts_with(method),
            "{path}: the body must name `{method}` — the status alone is not \
             a gate: {answer:#?}"
        );
    }
}

/// `ingest` answers `202` with a job id AND the route that reports it,
/// then the job actually runs on the daemon's manager: the folder's one
/// document becomes searchable through `search` on the same manager.
///
/// The `progress_route` field is the load-bearing half. A job id with
/// no named reporter is how a caller ends up inventing a poll loop.
/// CORRECTED 2026-09-10: it named `/internal/corpus/watch/status/{id}`,
/// a route that cannot serve this arm — see the test below, which
/// demonstrates the 404 rather than asserting the correction on trust.
#[tokio::test]
async fn ingest_is_a_job_that_names_its_progress_route_and_then_runs() {
    let (manager, addr) = harness().await;
    let (id, _folder) = register_folder(&manager, "ingest").await;

    let (status, ack) = post(
        addr,
        &format!("/internal/corpus/local/{id}/ingest"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 202, "ingest: {ack:#?}");
    assert_eq!(ack["corpus_id"], serde_json::json!(id));
    assert!(
        ack["job_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("lc-ingest-"),
        "the ack carries a job id: {ack:#?}"
    );
    assert_eq!(
        ack["progress_route"],
        serde_json::json!(format!("/internal/corpus/local/{id}/ingest/progress")),
        "the ack NAMES the route that reports THIS job — and it is the one \
         that works for this corpus kind, not the watch-status route that \
         404s for it: {ack:#?}"
    );

    // The job is real: poll the search route until the folder's one
    // document is indexed. 20s is generous for one 26-byte file under
    // a mock embedder; a timeout here is a broken ingest, not a slow one.
    // A `search` before the index exists is a NAMED 500 ("Index not
    // found"), not an empty list — the same §18.3 posture, and the
    // reason this loop tolerates a non-200 until the deadline rather
    // than treating the first poll as the answer.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut last = serde_json::Value::Null;
    loop {
        let (status, hits) = post(
            addr,
            &format!("/internal/corpus/local/{id}/search"),
            serde_json::json!({ "query": "quiet hours", "limit": 5 }),
        )
        .await;
        if status == 200 && hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            assert!(
                hits[0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("quiet hours"),
                "the hit is the fixture's own text, projected into the wire \
                 shape (ScoredChunk does not serialise): {hits:#?}"
            );
            assert_eq!(hits[0]["corpus_id"], serde_json::json!(id));
            return;
        }
        last = hits;
        if std::time::Instant::now() > deadline {
            panic!(
                "the ingest job never indexed the fixture document; last search answer: {last:#?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// The ingest progress route follows a **DocumentFolder** corpus from
/// submission to its terminal counts — the contract the desktop's ingest
/// arm could not cross without them.
///
/// Three assertions, in the order the finding (069660fd9) put them:
///
/// 1. The route the ack USED to name, `GET /internal/corpus/watch/
///    status/{id}`, **404s for this very corpus**. That is not incidental
///    colour: it is the measured reason the old ack could not be polled,
///    reproduced here so a future edit that points the ack back at it
///    reddens instead of shipping. Its handler wants a RECONCILABLE
///    watched folder and a document folder is not one.
/// 2. The new route serves the same corpus while the job is live and
///    keeps `finished: false` — a caller cannot mistake "running" for
///    "done and indexed nothing".
/// 3. The terminal frame carries `IngestStats`. `files_indexed` is
///    asserted against the THREE files the fixture wrote, so a handler
///    answering a plausible `1`, or defaulting the struct, fails.
///
/// The corpus is registered but never ingested at the top, which is the
/// fourth case: `200` with both halves null, NOT a 404 — "no such corpus"
/// and "this corpus has not started" are different answers (ARCH §18.3).
///
/// # Red-watch (2026-09-10, run, not asserted)
///
/// Watched red TWICE, because one sabotage would only have covered the
/// route's existence and the counts are the half that was owed:
///
/// ```text
/// route removed from lc_router (handler left)  :460  left: 404 right: 200
/// record_ingest_outcome writes stats: None     :536  files_indexed
///                                                    3 vs null
/// ```
///
/// The second is the one that matters: with the route mounted and the
/// receipt still written, dropping only the STATS leaves `finished: true`
/// and a well-formed answer — the exact shape a caller would have had to
/// fabricate counts from — and the test reddens on the count itself.
#[tokio::test]
async fn ingest_progress_follows_a_document_folder_to_its_terminal_stats() {
    let (manager, addr) = harness().await;
    let (id, _folder) = register_folder_with_files(&manager, "progress", 3).await;
    let progress_path = format!("/internal/corpus/local/{id}/ingest/progress");

    // (0) Registered, never ingested: a 200 that says so.
    let (status, body) = get(addr, &progress_path).await;
    assert_eq!(
        status, 200,
        "a registered corpus has a progress answer: {body:#?}"
    );
    assert_eq!(body["corpus_id"], serde_json::json!(id));
    assert_eq!(
        body["finished"],
        serde_json::json!(false),
        "nothing has finished because nothing has started: {body:#?}"
    );
    assert!(
        body["outcome"].is_null(),
        "no receipt before there is a job: {body:#?}"
    );

    // (1) The route the ack used to name cannot serve this corpus kind.
    let watch_addr = spawn_router(sovereign_mesh::corpus_watch_http::corpus_watch_router()).await;
    let (watch_status, watch_body) =
        get(watch_addr, &format!("/internal/corpus/watch/status/{id}")).await;
    assert_eq!(
        watch_status, 404,
        "the watch-status route requires a RECONCILABLE watched folder, and a \
         document folder is not one — this is why the ack could not name it, \
         and asserting it here is what stops the ack drifting back: {watch_body:#?}"
    );

    // (2) Submit, and read progress while it is live.
    let (status, ack) = post(
        addr,
        &format!("/internal/corpus/local/{id}/ingest"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 202, "ingest: {ack:#?}");
    let job_id = ack["job_id"].as_str().expect("a job id").to_string();
    assert_eq!(
        ack["progress_route"],
        serde_json::json!(progress_path),
        "the ack names THIS route: {ack:#?}"
    );

    // (3) Poll to the terminal frame. 30s is generous for three small
    // files under a mock embedder; a timeout is a broken ingest, not a
    // slow one.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut last = serde_json::Value::Null;
    loop {
        let (status, body) = get(addr, &progress_path).await;
        assert_eq!(status, 200, "the progress route stays reachable: {body:#?}");
        if body["finished"] == serde_json::json!(true) {
            last = body;
            break;
        }
        // While it is live, `finished` and the receipt agree with each
        // other — a route that set one without the other would let a
        // caller finish early on a half-written answer.
        assert!(
            body["outcome"].is_null(),
            "an unfinished job has no receipt: {body:#?}"
        );
        last = body;
        if std::time::Instant::now() > deadline {
            panic!("the ingest job never reached its terminal frame; last answer: {last:#?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    let outcome = &last["outcome"];
    assert_eq!(
        outcome["job_id"],
        serde_json::json!(job_id),
        "the receipt names the job the ack handed back, so two runs are \
         tellable apart: {last:#?}"
    );
    assert!(
        outcome["error"].is_null(),
        "the fixture ingest succeeds; an error here is a real failure: {last:#?}"
    );
    let stats = &outcome["stats"];
    assert_eq!(
        stats["files_indexed"],
        serde_json::json!(3),
        "the terminal frame carries the count the desktop renders — three \
         files in, three files reported: {last:#?}"
    );
    assert_eq!(stats["corpus_id"], serde_json::json!(id));
    assert!(
        stats["chunks_written"].as_u64().unwrap_or(0) > 0,
        "three documents produce chunks; a zero here means the counts are \
         defaulted rather than measured: {last:#?}"
    );
}

/// The desktop's WHOLE folder-drop journey over the wire, from a folder
/// no manager has ever heard of: register, ingest, follow the progress
/// route to its terminal counts.
///
/// # What this reproduces
///
/// The defect the real-mode harness found on run 5. D8 crossed the
/// desktop's ingest arm (502304f63) and left its registration behind in
/// `lc_pre_scan`, on this process's own `LocalCorpusManager`. On an
/// attached boot that manager is a different instance from this one, so
/// the ingest arrived for a corpus the daemon had never been told about
/// and answered
/// `404 … corpus 'folder-corpus-2918e9ebc0b5' is not registered
/// locally`. Every other case in this file registers through
/// `manager.register` directly, which is exactly the step that was
/// missing — so none of them could see it.
///
/// Step (0) is that 404, asserted rather than described: it is the
/// harness's own failure, and it is what the register route has to turn
/// into a 202.
///
/// # Red-watch (2026-09-10, run, not asserted)
///
/// `POST /internal/corpus/local` taken back out of `lc_router` (the
/// handler left in place, so the suite still built) and this case
/// re-run — `pass: 0 fail: 1`:
///
/// ```text
/// register_then_ingest_a_folder_no_manager_has_seen  :653
///     assertion `left == right` failed: register: Null
///       left: 405   right: 200
/// ```
///
/// Read step (0) alongside it: that assertion PASSED under the sabotage,
/// because a daemon with no register route 404s the ingest exactly as
/// one that was never told about the corpus does. Step (0) is the
/// harness's failure reproduced, and step (1) is the only line that can
/// tell the fix apart from it.
#[tokio::test]
async fn register_then_ingest_a_folder_no_manager_has_seen() {
    let (manager, addr) = harness().await;
    let (cfg, folder) = folder_fixture(&manager, "register", 2);
    let minted = cfg.id.clone();

    // (0) The harness's own failure. Nothing registered this corpus, so
    // the ingest route refuses it BY NAME — not a job id for a corpus
    // that does not exist.
    let (status, body) = post(
        addr,
        &format!("/internal/corpus/local/{minted}/ingest"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status, 404,
        "an ingest for an unregistered corpus is refused, not accepted: {body:#?}"
    );
    assert!(
        body["error"].as_str().unwrap_or_default().contains(&minted),
        "the refusal NAMES the corpus — this is the exact body the real-mode \
         harness read on run 5: {body:#?}"
    );

    // (1) Register over the wire, the way `lc_pre_scan` now does.
    let (status, registered) = post(
        addr,
        "/internal/corpus/local",
        serde_json::to_value(&cfg).expect("the config serialises"),
    )
    .await;
    assert_eq!(status, 200, "register: {registered:#?}");
    let corpus_id = registered["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the answer carries the id AS REGISTERED: {registered:#?}"))
        .to_string();
    assert_eq!(
        registered["display_name"], "Fixture register",
        "the answer is the config the manager KEPT, not an ack stub: {registered:#?}"
    );
    assert!(
        serde_json::to_string(&registered)
            .unwrap_or_default()
            .contains(&folder.to_string_lossy().to_string()),
        "the registered config carries the root it was registered with: {registered:#?}"
    );

    // Idempotent, because `register` is: a second pre-scan of the same
    // folder is an ordinary thing for a user to do, and it must not 409.
    let (status, again) = post(
        addr,
        "/internal/corpus/local",
        serde_json::to_value(&cfg).expect("the config serialises"),
    )
    .await;
    assert_eq!(status, 200, "re-register is idempotent: {again:#?}");
    assert_eq!(
        again["id"],
        serde_json::json!(corpus_id),
        "re-registering the same folder keeps the same id — a second id \
         would orphan the first corpus and double-ingest the folder: {again:#?}"
    );

    // (2) The very call that 404'd at step (0) is now accepted.
    let (status, ack) = post(
        addr,
        &format!("/internal/corpus/local/{corpus_id}/ingest"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status, 202,
        "the ingest the registration unblocked: {ack:#?}"
    );
    let job_id = ack["job_id"].as_str().expect("a job id").to_string();
    let progress_path = ack["progress_route"]
        .as_str()
        .expect("the ack names its progress route")
        .to_string();

    // (3) Follow it to the terminal counts. Two files in, two reported —
    // a defaulted or fabricated stat block cannot pass this.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let last = loop {
        let (status, body) = get(addr, &progress_path).await;
        assert_eq!(status, 200, "the progress route stays reachable: {body:#?}");
        if body["finished"] == serde_json::json!(true) {
            break body;
        }
        if std::time::Instant::now() > deadline {
            panic!("the ingest never reached its terminal frame; last answer: {body:#?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    };
    assert_eq!(
        last["outcome"]["job_id"],
        serde_json::json!(job_id),
        "the receipt names the job the ack handed back: {last:#?}"
    );
    assert!(
        last["outcome"]["error"].is_null(),
        "the fixture ingest succeeds; an error here is a real failure: {last:#?}"
    );
    assert_eq!(
        last["outcome"]["stats"]["files_indexed"],
        serde_json::json!(2),
        "two files were written into the folder and two are reported — the \
         count the desktop's terminal frame renders: {last:#?}"
    );
    assert_eq!(
        last["outcome"]["stats"]["corpus_id"],
        serde_json::json!(corpus_id)
    );
}

/// The cluster job (2026-09-11): `POST …/{c}/cluster` answers 202 with the
/// host's job id and the progress route; `GET …/{c}/cluster/progress`
/// serves the frames the job appended, from a cursor, and says when the
/// job ended. This harness has no inference and its folder carries no
/// enrichment config, so the job's terminal frame is an `Error` NAMING
/// the manager's refusal — which is the frame a desktop re-emits on its
/// channel, and which an unmounted route cannot produce.
///
/// The cursor contract is asserted too: a second poll from `next` returns
/// no frames already served.
#[tokio::test]
async fn cluster_is_a_job_whose_progress_route_serves_its_frames() {
    let (manager, addr) = harness().await;
    let (id, _folder) = register_folder(&manager, "cluster").await;

    // Progress before any job: a 404 naming the corpus, not an empty 200.
    let (status, body) = get(
        addr,
        &format!("/internal/corpus/local/{id}/cluster/progress"),
    )
    .await;
    assert_eq!(status, 404, "no job yet: {body:#?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains(&id),
        "the 404 must NAME the corpus: {body:#?}"
    );

    let (status, ack) = post(
        addr,
        &format!("/internal/corpus/local/{id}/cluster"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 202, "cluster: {ack:#?}");
    assert_eq!(ack["corpus_id"], serde_json::json!(id));
    let job_id = ack["job_id"].as_str().unwrap_or_default().to_string();
    assert!(
        job_id.starts_with("lc-cluster-"),
        "the ack carries a job id: {ack:#?}"
    );
    assert_eq!(
        ack["progress_route"],
        serde_json::json!(format!("/internal/corpus/local/{id}/cluster/progress")),
        "the ack NAMES the route that reports THIS job: {ack:#?}"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut cursor = 0u64;
    let mut frames: Vec<serde_json::Value> = Vec::new();
    let mut last = serde_json::Value::Null;
    loop {
        let (status, p) = get(
            addr,
            &format!("/internal/corpus/local/{id}/cluster/progress?after={cursor}"),
        )
        .await;
        assert_eq!(status, 200, "progress: {p:#?}");
        assert_eq!(p["job_id"], serde_json::json!(job_id));
        cursor = p["next"].as_u64().unwrap_or(cursor);
        frames.extend(p["frames"].as_array().cloned().unwrap_or_default());
        last = p.clone();
        if p["finished"] == serde_json::json!(true) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the cluster job never reported finished; last poll: {p:#?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let terminal = frames.last().expect("a finished job has a terminal frame");
    assert_eq!(
        terminal["phase"],
        serde_json::json!("error"),
        "frames: {frames:#?}"
    );
    let message = terminal["data"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("does not support clustering")
            || message.contains("requires an inference provider"),
        "the terminal frame names the manager's refusal verbatim, not a \
         generic failure: {message:?}"
    );

    // Cursor: nothing already served comes back, and `finished` holds.
    let (status, again) = get(
        addr,
        &format!("/internal/corpus/local/{id}/cluster/progress?after={cursor}"),
    )
    .await;
    assert_eq!(status, 200, "re-poll: {again:#?}");
    assert_eq!(
        again["frames"],
        serde_json::json!([]),
        "already-served frames: {again:#?}"
    );
    assert_eq!(
        again["finished"],
        serde_json::json!(true),
        "{again:#?} (last: {last:#?})"
    );

    // Unregistered corpus: the job route 404s naming it, BEFORE any spawn.
    let (status, body) = post(
        addr,
        "/internal/corpus/local/no-such-corpus/cluster",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 404, "{body:#?}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-corpus"),
        "the 404 must NAME the corpus: {body:#?}"
    );
}

/// `POST /internal/corpus/local/pre-scan` (2026-09-11): the user-picked
/// path is registered on the DAEMON's manager and classified there. The
/// answer's `corpus_id` is the one the registry kept, and the registry
/// holds it afterwards — which is the fact the desktop's D9c 404 turned
/// on. A path that is not a directory and an unknown source kind are
/// 400s naming the input, before anything is registered.
#[tokio::test]
async fn pre_scan_registers_on_the_daemons_manager_and_classifies_the_folder() {
    let (manager, addr) = harness().await;
    let root = manager.index_dir_root();
    let folder = root.parent().unwrap_or(&root).join("lc-fixture-pre-scan");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.md"), "alpha").unwrap();
    std::fs::write(folder.join("b.md"), "beta").unwrap();
    std::fs::write(folder.join("c.xyz"), "not a supported type").unwrap();

    let (status, body) = post(
        addr,
        "/internal/corpus/local/pre-scan",
        serde_json::json!({
            "path": folder.to_string_lossy(),
            "source_type": "folder",
            "display_name": "Pre-scan fixture",
        }),
    )
    .await;
    assert_eq!(status, 200, "pre-scan: {body:#?}");
    let id = body["corpus_id"].as_str().unwrap_or_default().to_string();
    assert!(!id.is_empty(), "the answer names the corpus: {body:#?}");
    assert_eq!(body["display_name"], serde_json::json!("Pre-scan fixture"));
    assert_eq!(
        body["result"]["readable"].as_array().map(Vec::len),
        Some(2),
        "two markdown files are readable: {body:#?}"
    );
    assert_eq!(body["result"]["total_visited"], serde_json::json!(3));
    assert!(
        manager.get(&id).await.is_some(),
        "the daemon's manager holds '{id}' after the pre-scan — the ingest \
         that follows reads THIS registry"
    );

    // Re-registering the same folder keeps the id (the manager's
    // path-identity guard), which is why the answer carries the id back.
    let (status, again) = post(
        addr,
        "/internal/corpus/local/pre-scan",
        serde_json::json!({ "path": folder.to_string_lossy(), "source_type": "folder" }),
    )
    .await;
    assert_eq!(status, 200, "{again:#?}");
    assert_eq!(
        again["corpus_id"],
        serde_json::json!(id),
        "same path, same id: {again:#?}"
    );

    let (status, body) = post(
        addr,
        "/internal/corpus/local/pre-scan",
        serde_json::json!({ "path": folder.join("a.md").to_string_lossy(), "source_type": "folder" }),
    )
    .await;
    assert_eq!(status, 400, "a file is not a directory: {body:#?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("a.md"),
        "the 400 names the path: {body:#?}"
    );

    let (status, body) = post(
        addr,
        "/internal/corpus/local/pre-scan",
        serde_json::json!({ "path": folder.to_string_lossy(), "source_type": "zip" }),
    )
    .await;
    assert_eq!(status, 400, "unknown source kind: {body:#?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("zip"),
        "the 400 names the kind: {body:#?}"
    );
}
