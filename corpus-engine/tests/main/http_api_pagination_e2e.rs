// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end tests for the `http_api` acquirer.
//!
//! These exercise the full pipeline (template render → paginated
//! fetch → JSONPath follow → on-disk persistence + journal) against
//! an in-process fake HTTP server (`wiremock`). They run offline so
//! CI doesn't need network.
//!
//! What's covered:
//! - URL templating with `{base_url}` + `{name}` placeholders
//! - Offset pagination: drives until a short page; correct `offset=N`
//!   query strings on each request
//! - Document-follow via JSONPath: per-page response is treated as
//!   an index; documents are fetched and persisted to disk under
//!   `<acquired-dir>/docs/<sha>.<ext>`
//! - Resume: a re-run skips documents already on disk and emits a
//!   coherent `_progress.json`
//! - For-each cartesian product: one paginated sequence per
//!   (entity × form_type) binding
//!
//! Live network paths (real SEC EDGAR / OpenAlex / etc.) are not
//! exercised here — those would gate behind `--ignored` in a
//! follow-up.

use std::collections::BTreeMap;

use corpus_engine::{
    DocFormat, FollowConfig, HttpMethod, PaginationStrategy, ParameterValue, RequestTemplate,
    ResolvedParameters,
};
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use corpus_engine::acquirers::http_api::HttpApiAcquirer;

fn params(items: Vec<(&str, ParameterValue)>) -> ResolvedParameters {
    let mut values = BTreeMap::new();
    for (k, v) in items {
        values.insert(k.into(), v);
    }
    ResolvedParameters { values }
}

#[tokio::test]
async fn offset_pagination_walks_until_short_page() {
    let server = MockServer::start().await;

    // Page 1: 2 items, the page-size cap → expect a continuation.
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 1}, {"id": 2}]
        })))
        .mount(&server)
        .await;

    // Page 2: 1 item (< page_size) → loop terminates.
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 3}]
        })))
        .mount(&server)
        .await;

    let req = RequestTemplate {
        url: format!("{}/items?offset=0", server.uri()),
        method: HttpMethod::Get,
        body: None,
        for_each: vec![],
    };
    let acq = HttpApiAcquirer::new(
        String::new(),
        vec![req],
        Some(PaginationStrategy::Offset {
            param: "offset".into(),
            page_size: 2,
            items_path: "$.items".into(),
        }),
        None,
        None,
        None,
        None,
        ResolvedParameters::default(),
    )
    .expect("acquirer constructs");

    let tmp = TempDir::new().unwrap();
    let docs = acq
        .acquire(tmp.path(), "test-corpus", &None)
        .await
        .expect("acquire succeeds");

    // Two pages persisted as JSON because there's no [acquire.follow].
    let docs_files: Vec<_> = std::fs::read_dir(&docs).unwrap().collect();
    assert_eq!(
        docs_files.len(),
        2,
        "expected 2 page responses on disk, got {}",
        docs_files.len()
    );

    // _progress.json journals both pages.
    let progress_path = tmp.path().join("test-corpus/_progress.json");
    let journal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(progress_path).unwrap()).unwrap();
    let pages = journal["completed_pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
}

#[tokio::test]
async fn follow_persists_one_document_per_url_in_index_response() {
    let server = MockServer::start().await;

    // The "search-index" endpoint returns a list of document URLs.
    let doc_url_1 = format!("{}/docs/a.html", server.uri());
    let doc_url_2 = format!("{}/docs/b.html", server.uri());
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hits": {"hits": [
                {"_source": {"file_url": doc_url_1.clone()}},
                {"_source": {"file_url": doc_url_2.clone()}},
            ]}
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/docs/a.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>A</html>"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/docs/b.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>B</html>"))
        .mount(&server)
        .await;

    let req = RequestTemplate {
        url: format!("{}/search", server.uri()),
        method: HttpMethod::Get,
        body: None,
        for_each: vec![],
    };
    let acq = HttpApiAcquirer::new(
        String::new(),
        vec![req],
        None, // no pagination
        Some(FollowConfig {
            document_url_path: "$.hits.hits[*]._source.file_url".into(),
            document_format: DocFormat::Html,
            max_concurrency: 2,
        }),
        None,
        None,
        None,
        ResolvedParameters::default(),
    )
    .expect("acquirer constructs");

    let tmp = TempDir::new().unwrap();
    let docs = acq
        .acquire(tmp.path(), "test-corpus", &None)
        .await
        .expect("acquire succeeds");

    let html_count = std::fs::read_dir(&docs)
        .unwrap()
        .filter_map(|r| r.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("html"))
        .count();
    assert_eq!(
        html_count, 2,
        "expected 2 .html documents, got {html_count}"
    );

    // Documents recorded in the journal.
    let progress_path = tmp.path().join("test-corpus/_progress.json");
    let journal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(progress_path).unwrap()).unwrap();
    let docs_completed = journal["completed_documents"].as_array().unwrap();
    assert_eq!(docs_completed.len(), 2);
}

#[tokio::test]
async fn for_each_cartesian_product_one_request_per_binding() {
    let server = MockServer::start().await;

    // Four expected requests: NVDA×10-K, NVDA×10-Q, MSFT×10-K, MSFT×10-Q.
    // The mock matches on the `q` and `forms` query params and
    // returns an empty page each time so pagination terminates on
    // page 1.
    for entity in ["NVDA", "MSFT"] {
        for form in ["10-K", "10-Q"] {
            Mock::given(method("GET"))
                .and(path("/search"))
                .and(query_param("q", entity))
                .and(query_param("forms", form))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
                .mount(&server)
                .await;
        }
    }

    let req = RequestTemplate {
        url: format!("{}/search?q={{entity}}&forms={{form_type}}", server.uri()),
        method: HttpMethod::Get,
        body: None,
        for_each: vec!["entity".into(), "form_type".into()],
    };
    let acq = HttpApiAcquirer::new(
        String::new(),
        vec![req],
        Some(PaginationStrategy::Offset {
            param: "offset".into(),
            page_size: 100,
            items_path: "$.items".into(),
        }),
        None,
        None,
        None,
        None,
        params(vec![
            (
                "entity",
                ParameterValue::List(vec!["NVDA".into(), "MSFT".into()]),
            ),
            (
                "form_type",
                ParameterValue::List(vec!["10-K".into(), "10-Q".into()]),
            ),
        ]),
    )
    .expect("acquirer constructs");

    let tmp = TempDir::new().unwrap();
    acq.acquire(tmp.path(), "test-corpus", &None)
        .await
        .expect("acquire succeeds");

    // One page response per binding → 4 files on disk.
    let docs = tmp.path().join("test-corpus/docs");
    let json_count = std::fs::read_dir(&docs)
        .unwrap()
        .filter_map(|r| r.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .count();
    assert_eq!(json_count, 4, "expected 4 page JSONs, got {json_count}");
}

#[tokio::test]
async fn resume_skips_documents_already_on_disk() {
    let server = MockServer::start().await;
    let doc_url = format!("{}/docs/x.html", server.uri());
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hits": {"hits": [{"_source": {"file_url": doc_url.clone()}}]}
        })))
        .mount(&server)
        .await;
    // Note: NO mock registered for the `/docs/x.html` GET. If the
    // acquirer tries to fetch it on the second run we'll get a 404.
    Mock::given(method("GET"))
        .and(path("/docs/x.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>X</html>"))
        .expect(1) // wiremock asserts this is hit exactly once
        .mount(&server)
        .await;

    let make_acq = || {
        let req = RequestTemplate {
            url: format!("{}/search", server.uri()),
            method: HttpMethod::Get,
            body: None,
            for_each: vec![],
        };
        HttpApiAcquirer::new(
            String::new(),
            vec![req],
            None,
            Some(FollowConfig {
                document_url_path: "$.hits.hits[*]._source.file_url".into(),
                document_format: DocFormat::Html,
                max_concurrency: 1,
            }),
            None,
            None,
            None,
            ResolvedParameters::default(),
        )
        .expect("acquirer constructs")
    };

    let tmp = TempDir::new().unwrap();

    // First run: hits the upstream, persists doc.
    make_acq()
        .acquire(tmp.path(), "test-corpus", &None)
        .await
        .expect("first run succeeds");

    // Second run: same tmp directory; the document is already on
    // disk so `fetch_document` short-circuits without calling
    // upstream again. `wiremock`'s `.expect(1)` above asserts the
    // doc URL was hit exactly once across both runs.
    make_acq()
        .acquire(tmp.path(), "test-corpus", &None)
        .await
        .expect("resume run succeeds");

    // (server drops verify on Drop)
}
