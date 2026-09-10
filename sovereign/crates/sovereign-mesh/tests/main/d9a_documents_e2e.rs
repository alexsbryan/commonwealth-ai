// SPDX-License-Identifier: AGPL-3.0-or-later
//! `documents_http` end to end — the document-asset family
//! (sv-surface D9a).
//!
//! Against a REAL `SqliteStateStore`, not the in-memory one: that
//! store stubs every `DocumentAssetStore` method to `Ok(())` /
//! `Ok(vec![])`, so a fixture built on it would report green over
//! handlers that never touched a row. The two folds this file has to
//! pin — the legacy listing's three skips and the promotion's title
//! and word-count rules — cannot be exhibited without real chunks.
//!
//! # Red-watch (2026-09-10, run)
//!
//! Every route moved to a planted path (`/v1/documents-planted`, …)
//! with the handlers, the DTOs and the two loopback layers untouched,
//! so the crate still built and the router still existed — the
//! sabotage is "the door is not where the caller knocks", which is
//! the drift a route census cannot see. `pass: 0 fail: 5`:
//!
//! ```text
//! documents_route_serves_the_stores_own_assets
//! document_by_id_is_404_when_the_store_does_not_hold_it
//! legacy_listing_applies_the_three_skips
//! promoting_a_legacy_source_mints_a_record_over_its_chunks
//! documents_without_a_store_is_the_named_503
//! ```
//!
//! The first four go red on the BODY: an axum method/path fallback
//! answers with no JSON at all, so `.json()` has no `documents` /
//! `document` key to read. The fifth goes red on its status line (404
//! vs 503) and that alone would not be a gate (ARCH §18.1) — a
//! routerless daemon 404s too — which is why the assertion under it
//! requires the body to parse and to NAME the missing object.
//! Restored: 5/5 green.

use std::sync::Arc;

use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::StateStore;
use sovereign_core::types::{
    AssetState, DocumentAsset, DocumentChunk, DocumentTypeTag, SourceType,
};
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::documents_http::documents_router;

use crate::common::{desktop_services_with_store, mesh_admin_services, spawn_router, TestProvider};

/// A real `CorpusEngine` over a tempdir — `ServingCore.corpus_engine`
/// is not an `Option`, and no route in this file reads it.
fn engine_at(tmp: &tempfile::TempDir) -> Arc<corpus_engine::CorpusEngine> {
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    Arc::new(corpus_engine::CorpusEngine::new(
        recipes,
        indexes,
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
    ))
}

fn asset(id: &str, title: &str, filename: &str) -> DocumentAsset {
    DocumentAsset {
        id: id.to_string(),
        title: title.to_string(),
        filename: filename.to_string(),
        file_size_mb: 1.5,
        word_count: 400,
        chunk_count: 3,
        document_type: DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("asset:{id}"),
        skeleton: None,
        state: AssetState::Ready,
        owner: None,
    }
}

fn chunk(source: &str, idx: usize, content: &str) -> DocumentChunk {
    DocumentChunk {
        id: format!("{source}#{idx}"),
        source: source.to_string(),
        content: content.to_string(),
        chunk_index: idx,
        embedding: None,
        created_at: 1_757_000_000,
        source_type: SourceType::UserDocument,
        version: 1_757_000_000,
        deleted_at: None,
    }
}

/// A serving daemon over a real sqlite store, plus the store handle so
/// a test can seed it through the SAME object the route reads.
async fn daemon_with_store() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<dyn StateStore>,
    Arc<EmbeddedDaemon>,
) {
    let engine_tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(&engine_tmp);
    let db_tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(
        sovereign_store::sqlite::SqliteStateStore::open(&db_tmp.path().join("sovereign.db"))
            .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        desktop_services_with_store(engine, Arc::clone(&store), Arc::new(TestProvider::new())),
    );
    // `root` is returned so the daemon's dir outlives the test; `db_tmp`
    // so the sqlite file does.
    (db_tmp, root, store, daemon)
}

#[tokio::test]
async fn documents_route_serves_the_stores_own_assets() {
    let (_db, _r, store, daemon) = daemon_with_store().await;
    store
        .save_document_asset(&asset("a-1", "Lease Agreement", "lease.pdf"))
        .await
        .unwrap();
    store
        .save_document_asset(&asset("a-2", "Q3 Report", "q3.docx"))
        .await
        .unwrap();
    let addr = spawn_router(documents_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/v1/documents"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let docs = body["documents"].as_array().expect("a `documents` array");
    assert_eq!(docs.len(), 2, "both saved assets cross: {body}");
    let ids: std::collections::HashSet<&str> =
        docs.iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert!(ids.contains("a-1") && ids.contains("a-2"), "got {ids:?}");
    // The whole record crosses, not a projection: the skeleton state
    // and the title are what the picker renders.
    let one = docs.iter().find(|d| d["id"] == "a-1").unwrap();
    assert_eq!(one["title"], "Lease Agreement");
    assert_eq!(one["filename"], "lease.pdf");
}

#[tokio::test]
async fn document_by_id_is_404_when_the_store_does_not_hold_it() {
    let (_db, _r, store, daemon) = daemon_with_store().await;
    store
        .save_document_asset(&asset("a-1", "Lease Agreement", "lease.pdf"))
        .await
        .unwrap();
    let addr = spawn_router(documents_router(daemon)).await;

    let hit: serde_json::Value = reqwest::get(format!("http://{addr}/v1/documents/a-1"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        hit["document"]["title"], "Lease Agreement",
        "a stored asset comes back whole: {hit}"
    );

    let resp = reqwest::get(format!("http://{addr}/v1/documents/nope"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "an id the store does not hold is a 404, not an Ok(None) the pane \
         renders as 'still ingesting'"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or_default().contains("nope"),
        "the 404 names the id it could not find, got {body}"
    );
}

#[tokio::test]
async fn legacy_listing_applies_the_three_skips() {
    let (_db, _r, store, daemon) = daemon_with_store().await;
    // An asset that OWNS `asset:a-1` — its chunks must not be offered
    // for promotion.
    store
        .save_document_asset(&asset("a-1", "Lease Agreement", "lease.pdf"))
        .await
        .unwrap();
    store
        .store_chunks(&[
            chunk("asset:a-1", 0, "owned by an asset already"),
            // Corpus content is not an upload.
            chunk("corpus:wikipedia", 0, "corpus chunk one two three"),
            // The one real legacy document: five words, one chunk.
            chunk(
                "/home/u/notes/minutes.txt",
                0,
                "alpha beta gamma delta epsilon",
            ),
        ])
        .await
        .unwrap();
    let addr = spawn_router(documents_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/v1/documents/legacy"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let docs = body["documents"].as_array().expect("a `documents` array");
    assert_eq!(
        docs.len(),
        1,
        "asset-owned and corpus sources are both skipped: {body}"
    );
    let row = &docs[0];
    assert_eq!(row["source"], "/home/u/notes/minutes.txt");
    assert_eq!(
        row["filename"], "minutes.txt",
        "the filename is the last path segment"
    );
    assert_eq!(row["chunk_count"], 1);
    assert_eq!(
        row["word_count"], 5,
        "the word count is the fold over chunk contents, not a store column"
    );
}

#[tokio::test]
async fn promoting_a_legacy_source_mints_a_record_over_its_chunks() {
    let (_db, _r, store, daemon) = daemon_with_store().await;
    store
        .store_chunks(&[
            chunk("/home/u/docs/board_minutes-2025.md", 0, "one two three"),
            chunk("/home/u/docs/board_minutes-2025.md", 1, "four five"),
        ])
        .await
        .unwrap();
    let addr = spawn_router(documents_router(daemon)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/documents/legacy/promote"))
        .json(&serde_json::json!({ "source": "/home/u/docs/board_minutes-2025.md" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let doc = &body["document"];

    assert_eq!(
        doc["title"], "board minutes 2025",
        "the title rule strips the extension and reads _ and - as spaces: {body}"
    );
    assert_eq!(doc["filename"], "board_minutes-2025.md");
    assert_eq!(doc["chunk_count"], 2);
    assert_eq!(doc["word_count"], 5, "summed across both chunks");
    assert_eq!(
        doc["state"].as_str().unwrap_or_default().to_lowercase(),
        "partiallyready",
        "a promoted record has had no structural pass, and says so: {body}"
    );
    assert!(doc["skeleton"].is_null(), "no skeleton is invented: {body}");

    // Promoting a source with no chunks is a 404, not an empty record.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/documents/legacy/promote"))
        .json(&serde_json::json!({ "source": "/nothing/here.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn documents_without_a_store_is_the_named_503() {
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    let addr = spawn_router(documents_router(daemon)).await;

    for path in ["/v1/documents", "/v1/documents/legacy", "/v1/documents/a-1"] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(
            resp.status(),
            503,
            "{path}: a mesh-admin daemon holds no StateStore"
        );
        // The status alone is not the gate — a routerless daemon is
        // also "not 200". The body must NAME the missing object.
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("StateStore"),
            "{path}: the 503 must say WHICH object is absent, got {body}"
        );
    }
}
