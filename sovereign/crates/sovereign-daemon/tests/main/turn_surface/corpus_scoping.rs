// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! The `enabled_corpora` wire surface: create seeds the allow-list,
//! PUT replaces it wholesale, and both refuse what would search
//! nothing while leaving the row as they found it.

use crate::common::{spawn_router, TestProvider};

use std::sync::Arc;

use sovereign_contracts::traits::StateStore;
use sovereign_daemon::turn_http::turn_router;

use super::serving_daemon;

/// Install a corpus at `<indexes>/<id>` with one chunk, marked complete so
/// the engine's `installed_indexes()` reports it — the same fixture
/// `knowledge_served_e2e` uses.
async fn install_corpus(indexes_dir: &std::path::Path, id: &str) {
    use corpus_index::index::{CorpusIndex, InsertChunk};
    let index = CorpusIndex::create(
        &indexes_dir.join(id),
        id,
        id,
        "qwen3-embedding-0.6b",
        4,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: "one chunk".into(),
                title: Some(id.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(id.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; 4],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
}

/// `enabled_corpora` on the create body — the wire form `svrn chat ask
/// --corpus` uses — lands on the row BEFORE the first turn, so retrieval's
/// allow-list filter sees it. Until 2026-09-01 the field had no wire form
/// at all and a daemon-served turn could not be scoped.
#[tokio::test]
async fn create_conversation_seeds_the_corpus_allow_list_it_was_given() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    install_corpus(&tmp.path().join("indexes"), "gutenberg").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.json::<serde_json::Value>().await.unwrap();
    let conv = body["id"].as_str().unwrap().to_string();
    assert_eq!(
        body["enabled_corpora"],
        serde_json::json!(["sep"]),
        "the create response echoes the seeded allow-list — the client's only \
         way to tell a daemon that scoped from one that dropped the key"
    );

    let row = store.get_conversation(&conv).await.unwrap();
    assert_eq!(
        row.enabled_corpora.as_deref(),
        Some(&["sep".to_string()][..]),
        "the allow-list must be on the row the first turn will read"
    );
}

/// The named failing input (§18.3): an id the daemon has not installed.
/// Retrieval would silently intersect it away and search NOTHING while the
/// answer read as "the corpus does not cover this". The route refuses with
/// a 400 that names the offender and lists what IS installed — the remedy,
/// not just the complaint — and seeds no row.
#[tokio::test]
async fn create_conversation_refuses_an_unknown_corpus_and_lists_the_installed_ones() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep", "nope"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "an unknown corpus id is the caller's mistake, not a daemon fault"
    );
    let body = resp.json::<serde_json::Value>().await.unwrap();
    let err = body["error"]
        .as_str()
        .expect("the refusal carries a reason");
    assert!(err.contains("unknown corpus id: nope"), "{err}");
    assert!(err.contains("installed: sep"), "{err}");
    assert!(
        store.list_conversations(10, 0).await.unwrap().is_empty(),
        "a refused create must not leave a half-seeded row behind"
    );
}

/// The allow-list has a SECOND writer — the desktop's corpus-chip strip,
/// which toggles it long after create — and until sv-surface it had no wire
/// form at all. The desktop wrote the column through its own store handle,
/// so in attach mode every chip the user touched landed on a row the turn
/// never reads: a filter that looked like it worked and did nothing.
///
/// PUT replaces the list wholesale, which is what a chip strip does.
#[tokio::test]
async fn enabled_corpora_put_writes_the_row_the_turn_reads() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    install_corpus(&tmp.path().join("indexes"), "gutenberg").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let body = http
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let conv = body["id"].as_str().unwrap().to_string();

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["gutenberg"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        store
            .get_conversation(&conv)
            .await
            .unwrap()
            .enabled_corpora
            .as_deref(),
        Some(&["gutenberg".to_string()][..]),
        "the PUT must REPLACE the seeded list on the row the turn reads, \
         not merge with it"
    );

    // `null` clears, and clearing means "search every installed corpus" —
    // the column's contract, so the route must not confuse it with the
    // empty list below.
    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": serde_json::Value::Null }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        store.get_conversation(&conv).await.unwrap().enabled_corpora,
        None,
        "null clears the column — not 'search nothing'"
    );
}

/// The named failing inputs (§18.1), both of which the desktop's local write
/// accepted: an id nothing has installed, and the empty list. Retrieval
/// intersects the allow-list SILENTLY, so either one produced an empty
/// fan-out and an answer that read as "the corpus does not cover this".
/// The route refuses both, names the remedy, and leaves the row as it was.
#[tokio::test]
async fn enabled_corpora_put_refuses_what_would_search_nothing() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let body = http
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let conv = body["id"].as_str().unwrap().to_string();

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["nope"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err = resp.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .expect("the refusal carries a reason")
        .to_string();
    assert!(err.contains("unknown corpus id: nope"), "{err}");
    assert!(err.contains("installed: sep"), "{err}");

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err = resp.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("would search nothing"), "{err}");

    assert_eq!(
        store
            .get_conversation(&conv)
            .await
            .unwrap()
            .enabled_corpora
            .as_deref(),
        Some(&["sep".to_string()][..]),
        "a refused PUT must leave the row exactly as it found it"
    );
}

/// A conversation the daemon does not hold is a 404, not a silent success.
/// The store's write reports `NotFound` and the route must carry that
/// through rather than collapse it into 204 (§18.3) — otherwise an attached
/// surface pointed at the wrong daemon toggles chips forever and is told
/// each one landed.
#[tokio::test]
async fn enabled_corpora_put_on_a_missing_conversation_is_404() {
    let (tmp, daemon, _store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .put(format!(
            "http://{addr}/v1/conversations/ghost/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
