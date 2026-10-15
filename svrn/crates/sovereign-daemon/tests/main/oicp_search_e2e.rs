// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scoped and ranged knowledge search through the real client router (OICP
//! v0.6 §2.8), over the conformance suite's fixture library
//! (`common::fixture_library`), whose chunks are paragraphs cut from their
//! stored texts, so every hit has a range and its text says what it says.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kernel_types::Sha256Hash;
use oicp_types::evidence::reasons;
use oicp_types::KnowledgeResult;
use sovereign_daemon::search_stage::SearchStage;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::fixture_library::{install, library, state_over, Library, DIM};

async fn search(state: &AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/v1/knowledge/search")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55005".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn hits(body: &serde_json::Value) -> Vec<KnowledgeResult> {
    serde_json::from_value(body["results"].clone()).unwrap_or_else(|e| panic!("{e}: {body}"))
}

async fn fixture(texts: bool) -> (tempfile::TempDir, AppState, Library) {
    let lib = library();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &lib.corpus_id, &lib.documents, texts).await;
    let state = state_over(indexes).await;
    (tmp, state, lib)
}

/// A query whose words reach every fixture text.
const BROAD: &str = "river archive ferry";

fn query(lib: &Library, q: &str, documents: &[String]) -> serde_json::Value {
    query_top(lib, q, documents, 10)
}

fn query_top(lib: &Library, q: &str, documents: &[String], limit: u32) -> serde_json::Value {
    serde_json::json!({
        "query_embedding": vec![0.0_f32; DIM],
        "query_text": q,
        "corpora": [lib.corpus_id],
        "limit": limit,
        "documents": documents,
    })
}

fn name_of(lib: &Library, doc: &str) -> String {
    let d = lib.documents.iter().find(|d| d.name == doc).unwrap();
    Sha256Hash::of_str(&d.text).to_hex()
}

#[tokio::test]
async fn every_hit_carries_its_range_and_the_text_there_is_its_exact() {
    let (_tmp, state, lib) = fixture(true).await;
    let (status, body) = search(&state, query(&lib, BROAD, &[])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let found = hits(&body);
    assert!(found.len() >= 3, "{body}");
    for h in &found {
        let span = h
            .span
            .as_ref()
            .unwrap_or_else(|| panic!("a paragraph of a stored text has a range: {h:?}"));
        assert_eq!(h.span_absent, None);
        let doc = lib
            .documents
            .iter()
            .find(|d| Sha256Hash::of_str(&d.text).to_hex() == span.document.text_sha256)
            .unwrap_or_else(|| panic!("the span names a fixture text: {span:?}"));
        let there: String = doc
            .text
            .chars()
            .skip(span.start as usize)
            .take((span.end - span.start) as usize)
            .collect();
        assert_eq!(there, span.exact, "{span:?}");
        assert_eq!(span.exact, h.content, "the chunk is the range");
        assert_eq!(
            Some(&span.document.text_sha256),
            h.document.as_ref().map(|d| &d.text_sha256)
        );
    }
}

/// A corpus built before the text store names no text, so no hit has a range
/// and each says why with its document's reason, never silence.
#[tokio::test]
async fn a_corpus_without_texts_names_why_no_hit_has_a_range() {
    let (_tmp, state, lib) = fixture(false).await;
    let (_, body) = search(&state, query(&lib, BROAD, &[])).await;
    let found = hits(&body);
    assert!(!found.is_empty(), "{body}");
    for h in &found {
        assert!(h.span.is_none(), "{h:?}");
        assert_eq!(h.span_absent.as_deref(), Some(reasons::TEXTS_NOT_STORED));
    }
}

#[tokio::test]
async fn a_documents_filter_returns_hits_from_those_texts_only() {
    let (_tmp, state, lib) = fixture(true).await;
    let (_, body) = search(&state, query(&lib, BROAD, &[])).await;
    let mut reached: Vec<String> = Vec::new();
    for h in hits(&body) {
        let n = h.document.map(|d| d.text_sha256).unwrap();
        if !reached.contains(&n) {
            reached.push(n);
        }
    }
    assert!(
        reached.len() >= 2,
        "the broad query reaches several texts: {body}"
    );

    // The top one hit, scoped to a text that does not rank first: only a
    // filter inside the search, before its top is cut, returns one.
    let second = reached[1].clone();
    let (status, body) = search(&state, query_top(&lib, BROAD, &[second.clone()], 1)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let scoped = hits(&body);
    assert_eq!(scoped.len(), 1, "the scope thinned its own hits: {body}");
    assert_eq!(
        scoped[0].document.as_ref().map(|d| d.text_sha256.as_str()),
        Some(second.as_str()),
        "a hit from an unnamed text leaked: {body}"
    );

    let harbour = name_of(&lib, "harbour-notes");
    let (_, body) = search(&state, query(&lib, BROAD, &[harbour.clone()])).await;
    for h in &hits(&body) {
        assert_eq!(
            h.document.as_ref().map(|d| d.text_sha256.as_str()),
            Some(harbour.as_str()),
            "a hit from an unnamed text leaked: {body}"
        );
    }

    let two = [harbour, name_of(&lib, "okafor2019")];
    let (_, body) = search(&state, query(&lib, BROAD, &two)).await;
    let both: Vec<String> = hits(&body)
        .iter()
        .filter_map(|h| h.document.as_ref().map(|d| d.text_sha256.clone()))
        .collect();
    assert!(both.iter().all(|n| two.contains(n)), "{body}");
    assert!(
        two.iter().all(|n| both.contains(n)),
        "each named text answers: {body}"
    );
}

#[tokio::test]
async fn a_documents_entry_that_is_not_a_text_name_is_refused() {
    let (_tmp, state, lib) = fixture(true).await;
    let (status, _) = search(&state, query(&lib, BROAD, &["harbour-notes".into()])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Platform row PV1: a search's trace names each of its stages once per
/// corpus, with an elapsed time, under its `search` span. A build that drops one stage's event
/// (`span_of`'s was the planted one) turns this red.
#[tokio::test]
async fn a_search_trace_names_every_stage_with_its_time_under_one_span() {
    let (_tmp, state, lib) = fixture(true).await;
    let trace = captured_debug(async {
        let (status, body) = search(&state, query(&lib, BROAD, &[])).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    })
    .await;
    for stage in SearchStage::PASSAGE_ROUTE.map(SearchStage::as_str) {
        let lines: Vec<&str> = trace
            .lines()
            .filter(|l| l.contains("search stage") && l.contains(&format!("stage=\"{stage}\"")))
            .collect();
        assert_eq!(lines.len(), 1, "one `{stage}` event:\n{trace}");
        assert!(lines[0].contains("elapsed_us="), "{}", lines[0]);
        assert!(
            lines[0].contains("search{route=\"knowledge\" query="),
            "under the search span: {}",
            lines[0]
        );
    }
}

/// Every tracing line at DEBUG while `f` runs, without colour.
pub(crate) async fn captured_debug<F: std::future::Future<Output = ()>>(f: F) -> String {
    #[derive(Clone)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for Captured {
        type Writer = Captured;
        fn make_writer(&self) -> Captured {
            self.clone()
        }
    }
    // With one dispatcher registered, tracing-core (0.1.36,
    // `Dispatchers::rebuilder`'s `has_just_one`) computes a callsite's
    // interest from the REGISTERING thread's default alone, and caches it
    // for every thread: another test's thread, with no subscriber, first
    // reaching the stage event's callsite stores `never`, and this capture
    // sees none of its events (watched: 0 `admission` events for 1, 7 runs
    // of 8, once the sentence tests searched beside the stage test). A
    // second dispatcher kept for the process makes registration read the
    // whole set, this capture's included; the rebuild below repairs a
    // callsite cached `never` before this capture began.
    static SECOND: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(Captured(std::sync::Arc::clone(&buf)))
        .with_ansi(false)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        f.await;
    }
    let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    out
}
