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
/// no named reporter is how a caller ends up inventing a poll loop, and
/// there is deliberately no second job table here — the ack points at
/// the watch-status route that already exists.
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
        serde_json::json!(format!("/internal/corpus/watch/status/{id}")),
        "the ack NAMES the existing route that reports this job — no second \
         job table: {ack:#?}"
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
