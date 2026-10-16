// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus install --inline` against a fake daemon that states its install
//! limit (OICP v0.6 §2.12): a recipe past it is refused before a byte is
//! sent, one within it installs and waits on the published progress.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use super::*;

const RECIPE: &str =
    "[corpus]\nid = \"inline-t\"\nname = \"T\"\n\n[acquire]\ntype = \"inline\"\n\n\
                      [extract]\ntype = \"plaintext\"\n\n[chunk]\ntype = \"paragraph\"\n\n\
                      [[document]]\nname = \"a\"\ntext = '''A short text.'''\n";

/// A fake daemon stating `max_install_bytes`; the count of installs it read.
async fn fake(max_install_bytes: u64) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    let app = Router::new()
        .route(
            "/oicp/v1/capabilities",
            get(move || async move {
                Json(json!({
                    "oicp_version": "0.4.0", "models": [],
                    "features": ["ingest:v1", "ingest:recipe", "ingest:documents"],
                    "knowledge": {
                        "corpora": [], "search_endpoint": "/v1/knowledge/search",
                        "ingest": {
                            "install_endpoint": "/oicp/v1/corpus/install",
                            "progress_endpoint": "/oicp/v1/corpus/progress",
                            "max_install_bytes": max_install_bytes
                        }
                    }
                }))
            }),
        )
        .route(
            "/oicp/v1/corpus/install",
            post(move || async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Json(json!({"corpus_id": "inline-t", "spawned": true, "recipe_sha256": "ab"}))
            }),
        )
        .route(
            "/oicp/v1/corpus/progress",
            get(|| async {
                Json(
                    json!({"progress": {"inline-t": {"phase": "complete", "documents": [
                        {"source": "a", "state": "ready", "text_sha256": "cd"}
                    ]}}}),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, hits)
}

fn recipe_file() -> tempfile::NamedTempFile {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), RECIPE).unwrap();
    f
}

/// A recipe whose install body is past the stated limit is never sent.
/// Red against a client that posts first and reads the 413 after.
#[tokio::test]
async fn a_recipe_past_the_stated_limit_is_refused_before_it_is_sent() {
    let (base, hits) = fake(64).await;
    let f = recipe_file();
    let code = install_inline(&base, f.path(), BTreeMap::new(), None).await;
    assert_eq!(code, 1);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "nothing past the limit is sent"
    );
}

/// Within the limit it installs, and `--wait` reads the published progress.
#[tokio::test]
async fn a_recipe_within_the_stated_limit_installs_and_waits_on_progress() {
    let (base, hits) = fake(64 * 1024 * 1024).await;
    let f = recipe_file();
    let code = install_inline(&base, f.path(), BTreeMap::new(), Some(10)).await;
    assert_eq!(code, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn the_limit_is_held_to_the_byte_and_an_unstated_one_does_not_refuse() {
    assert!(fits(100, Some(100)).is_ok());
    let past = fits(101, Some(100)).unwrap_err();
    assert!(
        past.contains("101 bytes") && past.contains("at most 100"),
        "{past}"
    );
    assert!(fits(usize::MAX, None).is_ok());
}

#[test]
fn a_daemon_without_ingest_recipe_takes_no_recipe_inline() {
    let m: ProviderManifest = serde_json::from_value(json!({
        "oicp_version": "0.4.0", "models": [], "features": ["ingest:v1"],
        "knowledge": {"corpora": [], "search_endpoint": "/s", "ingest": {
            "install_endpoint": "/i", "progress_endpoint": "/p"}}
    }))
    .unwrap();
    let e = install_surface(&m).unwrap_err();
    assert!(e.contains("ingest:recipe"), "{e}");
}
