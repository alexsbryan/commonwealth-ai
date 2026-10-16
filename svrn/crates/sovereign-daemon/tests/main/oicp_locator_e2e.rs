// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a span stands (OICP v0.6 §2.13, `knowledge:locator`) through the
//! real client router: the conformance suite's headings shelf
//! (`fixture/headings.json`, a headed text with a contents line and a
//! preface, and a text with no headings), stored as ingest stores it, its
//! heading paths kept beside each text (corpus-engine's `heading_paths_e2e`
//! proves ingest keeps them).
//!
//! What the `knowledge.locator` conformance check holds, in process (the
//! check itself installs through `ingest:recipe`, which needs the engine):
//! each listed quotation aligned verbatim carries the path the fixture
//! states, or none; a search hit carries the same locator.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use oicp_types::evidence::AlignResponse;
use oicp_types::{KnowledgeResult, SpanLocator};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::fixture_library::{install, state_over, Doc, DIM};

const SHELF: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/headings.json");

async fn post(state: &AppState, path: &str, body: serde_json::Value) -> serde_json::Value {
    let mut req = Request::post(path)
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55013".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

async fn shelf() -> (tempfile::TempDir, AppState, String, serde_json::Value) {
    let fx: serde_json::Value = serde_json::from_str(SHELF).unwrap();
    let corpus = fx["shelf"]["corpus_id"].as_str().unwrap().to_string();
    let docs: Vec<Doc> = serde_json::from_value(fx["shelf"]["documents"].clone()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &corpus, &docs, true).await;
    let state = state_over(indexes).await;
    (tmp, state, corpus, fx)
}

/// Red with the place before read (the previous part's path) or a place
/// invented for the text with no headings.
#[tokio::test]
async fn an_aligned_span_names_the_heading_path_it_stands_under_or_none() {
    let (_tmp, state, corpus, fx) = shelf().await;
    for l in fx["located"].as_array().unwrap() {
        let quote = l["quote"].as_str().unwrap();
        let want: Option<SpanLocator> = serde_json::from_value(l["locator"].clone()).unwrap();
        let body = post(
            &state,
            "/oicp/v1/align",
            serde_json::json!({"quote": quote, "corpora": [corpus]}),
        )
        .await;
        let reply: AlignResponse = serde_json::from_value(body.clone()).unwrap();
        let Some(a) = reply.alignments.first() else {
            panic!("`{quote}` aligns: {body}")
        };
        assert_eq!(a.span.exact, quote, "{body}");
        assert_eq!(a.span.locator, want, "`{quote}` ({})", l["document"]);
    }
}

#[tokio::test]
async fn a_search_hit_carries_the_locator_of_where_it_starts() {
    let (_tmp, state, corpus, _) = shelf().await;
    let body = post(
        &state,
        "/v1/knowledge/search",
        serde_json::json!({
            "query_embedding": vec![0.0_f32; DIM],
            "query_text": "gulls nested pier",
            "corpora": [corpus],
            "limit": 10,
        }),
    )
    .await;
    let hits: Vec<KnowledgeResult> = serde_json::from_value(body["results"].clone()).unwrap();
    let gulls = hits
        .iter()
        .find(|h| h.content.starts_with("The gulls nested"))
        .unwrap_or_else(|| panic!("the gulls paragraph is a hit: {body}"));
    assert_eq!(
        gulls.span.as_ref().and_then(|s| s.locator.clone()),
        Some(SpanLocator::HeadingPath {
            headings: vec![
                "BOOK I. OF THE HARBOUR.".into(),
                "CHAPTER II. OF THE GULLS.".into()
            ]
        }),
        "{body}"
    );
}
