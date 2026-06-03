//! `/internal/corpus/watch/*` HTTP-surface integration test.
//!
//! Closes the biggest single bucket gap in the matrix: 14 routes,
//! 0 prior integration tests. Pinned shape: the daemon's
//! watched-folder reconciliation subsystem registers, pauses,
//! resumes, and tears down corpora via HTTP — covering the
//! happy-path lifecycle a desktop client drives when the user opens
//! "Watch folder" → "Pause" → "Resume" → "Remove".
//!
//! Singleton constraint: `watched_folder_runtime` keeps the
//! `LocalCorpusManager` + `WatchedFolderRegistry` in process-global
//! `OnceLock`s. Each test binary gets a fresh process so the
//! singleton is clean at start, but **all tests in this file MUST
//! share the same install** — re-installing across tests is a
//! silent no-op. So this file groups every corpus_watch test under
//! one `Lazy` harness that installs the singleton exactly once.
//!
//! Five assertions (one per route, exercising the
//! register → list → status → pause → resume → remove arc):
//!
//! 1. **Register a watched folder.** POST `/register` with an
//!    absolute path → 200 with the assigned `corpus_id`.
//! 2. **List surfaces the registration.** GET `/list` → the
//!    response's `corpora[]` contains the registered corpus.
//! 3. **Status returns a `WatchedFolderStatus` for the corpus.**
//!    GET `/status/{corpus_id}` → 200 + a status (Idle / Sweeping /
//!    Paused — any well-formed variant is fine; the assertion is
//!    that the route round-trips and the corpus is known).
//! 4. **Pause + resume flip the state.** POST `/pause/{id}` →
//!    200; GET `/status/{id}` → `PausedManual`. POST `/resume/{id}` →
//!    200; subsequent status MUST NOT be PausedManual.
//! 5. **Delete unregisters and 404s subsequent status.** DELETE
//!    `/{id}` → 200; GET `/status/{id}` → 404 (the corpus is gone).
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::traits::StateStore;
use sovereign_mesh::corpus_watch_http::corpus_watch_router;
use sovereign_mesh::watched_folder_runtime;
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::LocalCorpusManager;

mod common;
use common::spawn_router;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// One-shot harness builder. The singleton is filled the first time
/// any test calls `install_singleton`; subsequent calls reuse the
/// same handles. Returns the data_dir (so tests can compute folder
/// paths under it) and the listener address.
async fn install_singleton_and_spawn() -> (PathBuf, SocketAddr) {
    static HARNESS: OnceLock<(PathBuf, SocketAddr)> = OnceLock::new();
    if let Some(h) = HARNESS.get() {
        return h.clone();
    }
    // Build the manager + registry once, install into the runtime
    // singleton, spawn the router on a free port. Subsequent calls
    // race the OnceLock initialisation harmlessly.
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
    std::fs::create_dir_all(data_dir.join("recipes")).unwrap();
    // Leak the TempDir so its lifetime equals the process — the
    // singleton's manager holds paths into it indefinitely.
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
            store.clone() as Arc<dyn StateStore>,
            None,
            data_dir.clone(),
            data_dir.join("vault-snapshots"),
        )
        .await
        .expect("manager init"),
    );
    let registry = Arc::new(WatchedFolderRegistry::new());

    watched_folder_runtime::install(manager, registry);

    let addr = spawn_router(corpus_watch_router()).await;
    let entry = (data_dir, addr);
    let _ = HARNESS.set(entry.clone());
    entry
}

async fn post_json(addr: SocketAddr, path: &str, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .json(&body)
        .send()
        .await
        .expect("HTTP request must reach the watched-folder router")
}

async fn get(addr: SocketAddr, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .expect("HTTP request must reach the watched-folder router")
}

async fn delete(addr: SocketAddr, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!("http://{addr}{path}"))
        .send()
        .await
        .expect("HTTP request must reach the watched-folder router")
}

/// Create a unique on-disk folder under the singleton's data_dir so
/// the test fixtures don't collide across the multi-test sequence.
fn make_unique_folder(data_dir: &std::path::Path, tag: &str) -> PathBuf {
    let folder = data_dir.join(format!("watch-target-{tag}"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("hello.txt"), "hello world").unwrap();
    folder
}

/// Register a watched folder and return its assigned `corpus_id`.
async fn register_one(addr: SocketAddr, folder: PathBuf, display: &str) -> String {
    let resp = post_json(
        addr,
        "/internal/corpus/watch/register",
        serde_json::json!({
            "path": folder,
            "display_name": display,
            "sync_initial": false,
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "register MUST 200 on a valid path; got {}",
        resp.status()
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    json["corpus_id"]
        .as_str()
        .expect("register response must carry `corpus_id`")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn register_then_list_then_status_round_trip() {
    let (data_dir, addr) = install_singleton_and_spawn().await;
    let folder = make_unique_folder(&data_dir, "list-target");
    let corpus_id = register_one(addr, folder.clone(), "List Target").await;

    // (2) /list surfaces the registration.
    let list = get(addr, "/internal/corpus/watch/list").await;
    assert_eq!(list.status(), reqwest::StatusCode::OK);
    let list_json: serde_json::Value = list.json().await.unwrap();
    let corpora = list_json["corpora"]
        .as_array()
        .expect("list response must carry `corpora`");
    let ids: Vec<&str> = corpora
        .iter()
        .filter_map(|c| c["corpus_id"].as_str())
        .collect();
    assert!(
        ids.contains(&corpus_id.as_str()),
        "registered corpus_id `{corpus_id}` MUST appear in /list — \
         got: {ids:?}"
    );

    // (3) /status returns a well-formed StatusResponse for that id.
    let status = get(addr, &format!("/internal/corpus/watch/status/{corpus_id}")).await;
    assert_eq!(
        status.status(),
        reqwest::StatusCode::OK,
        "status for a registered corpus MUST 200; got {}",
        status.status()
    );
    let status_json: serde_json::Value = status.json().await.unwrap();
    assert_eq!(
        status_json["corpus_id"].as_str(),
        Some(corpus_id.as_str()),
        "status response must echo the corpus_id; got: {status_json}"
    );
    assert!(
        status_json["status"].is_object() || status_json["status"].is_string(),
        "status response must carry a `status` field with a \
         WatchedFolderStatus shape; got: {status_json}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_round_trip_flips_status() {
    let (data_dir, addr) = install_singleton_and_spawn().await;
    let folder = make_unique_folder(&data_dir, "pause-target");
    let corpus_id = register_one(addr, folder, "Pause Target").await;

    // Pause.
    let pause = post_json(
        addr,
        &format!("/internal/corpus/watch/pause/{corpus_id}"),
        serde_json::json!({ "reason": "integration test" }),
    )
    .await;
    assert_eq!(
        pause.status(),
        reqwest::StatusCode::OK,
        "pause MUST 200 on a registered corpus; got {}",
        pause.status()
    );

    // Status should now reflect PausedManual.
    let status_paused = get(addr, &format!("/internal/corpus/watch/status/{corpus_id}")).await;
    assert_eq!(status_paused.status(), reqwest::StatusCode::OK);
    let paused_json: serde_json::Value = status_paused.json().await.unwrap();
    let paused_marker = paused_json["status"].to_string();
    assert!(
        paused_marker.contains("paused_manual") || paused_marker.contains("PausedManual"),
        "after pause, status MUST surface a PausedManual variant; \
         got status body: {paused_json}"
    );

    // Resume.
    let resume = post_json(
        addr,
        &format!("/internal/corpus/watch/resume/{corpus_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        resume.status(),
        reqwest::StatusCode::OK,
        "resume MUST 200 on a paused corpus; got {}",
        resume.status()
    );

    // Status should no longer be PausedManual.
    let status_resumed = get(addr, &format!("/internal/corpus/watch/status/{corpus_id}")).await;
    assert_eq!(status_resumed.status(), reqwest::StatusCode::OK);
    let resumed_json: serde_json::Value = status_resumed.json().await.unwrap();
    let resumed_marker = resumed_json["status"].to_string();
    assert!(
        !resumed_marker.contains("paused_manual") && !resumed_marker.contains("PausedManual"),
        "after resume, status MUST NOT remain PausedManual; \
         got status body: {resumed_json}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_unregisters_corpus_and_subsequent_status_404s() {
    let (data_dir, addr) = install_singleton_and_spawn().await;
    let folder = make_unique_folder(&data_dir, "delete-target");
    let corpus_id = register_one(addr, folder, "Delete Target").await;

    // DELETE /{corpus_id} tears down.
    let del = delete(addr, &format!("/internal/corpus/watch/{corpus_id}")).await;
    assert_eq!(
        del.status(),
        reqwest::StatusCode::OK,
        "delete MUST 200 on a registered corpus; got {}",
        del.status()
    );

    // Subsequent status MUST 404 (the corpus is gone).
    let status_after = get(addr, &format!("/internal/corpus/watch/status/{corpus_id}")).await;
    assert_eq!(
        status_after.status(),
        reqwest::StatusCode::NOT_FOUND,
        "after delete, status MUST 404 — anything else means the \
         corpus is still registered, defeating the delete contract. \
         Got: {}",
        status_after.status()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_against_unknown_corpus_400s_with_error_body() {
    // Negative control: pause on a corpus that was never registered
    // must surface an error (not a panic, not a 200). The handler's
    // `Err` arm maps to 400; pinning that so a refactor doesn't
    // silently flip it to 200 with `ok: false` (which a desktop
    // client would interpret as success).
    let (_data_dir, addr) = install_singleton_and_spawn().await;
    let resp = post_json(
        addr,
        "/internal/corpus/watch/pause/this-corpus-does-not-exist",
        serde_json::json!({}),
    )
    .await;
    assert!(
        resp.status().is_client_error(),
        "pause on an unknown corpus MUST be a 4xx client error — \
         a 2xx here means the handler silently fabricated success. \
         Got: {}",
        resp.status()
    );
    let json: serde_json::Value = resp.json().await.unwrap_or_default();
    assert!(
        json.get("error").is_some(),
        "error response body MUST carry an `error` field for \
         diagnostics; got: {json}"
    );
}
