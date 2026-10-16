// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sentence search by meaning through the real client router (OICP v0.6
//! §2.11), over the conformance suite's fixture library: the background index,
//! its sidecar, the two routes, the scorer's closed set and its refusal, the
//! scan's ceiling, and the embedder's identity on every vector served.
//!
//! The embedder is a fake whose vectors are its words hashed under a seed, so
//! a sentence searched for by its own words is its own best match, and a
//! second seed is a second model whose vectors share no space with the first.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use futures::Stream;
use kernel_types::{NodeId, Sha256Hash};
use oicp_types::{
    IndexPlace, KnowledgeResult, KnowledgeSearchResponse, SentenceRoute, SentenceScorer,
};
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot};
use sovereign_contracts::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use sovereign_daemon::search_stage::SearchStage;
use sovereign_daemon::sentence_index::store::PROBE_TEXT;
use sovereign_daemon::sentence_index::SentenceSearchConfig;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::fixture_library::{digest_of, install, library, Library, DIM};
use crate::common::service_double::ProviderService;

const EMBED_MODEL: &str = "fake-word-hash";
const RERANKER: &str = "fake-reranker-q8";

/// Words hashed into `DIM` signed buckets under `seed`: the model.
fn words_vector(seed: u64, text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        let h = Sha256Hash::of_str(&format!("{seed}:{}", w.to_lowercase()));
        let b = h.as_bytes();
        v[b[0] as usize % DIM] += if b[1] & 1 == 0 { 1.0 } else { -1.0 };
    }
    v
}

#[derive(Default)]
struct WordHash {
    seed: AtomicU64,
    /// Hold every background batch (a sentence batch carrying the probe).
    hold_worker: AtomicBool,
    worker_batches: AtomicUsize,
    rerank_slot: bool,
}

impl WordHash {
    fn seeded(seed: u64) -> Arc<Self> {
        let w = Self::default();
        w.seed.store(seed, Ordering::SeqCst);
        Arc::new(w)
    }
}

#[async_trait::async_trait]
impl InferenceProvider for WordHash {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unimplemented!("sentence search never completes")
    }

    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("sentence search never streams")
    }

    async fn embed(&self, text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(words_vector(self.seed.load(Ordering::SeqCst), text))
    }

    async fn embed_batch(&self, texts: &[String]) -> sovereign_core::error::Result<Vec<Vec<f32>>> {
        // A background batch is fixture sentences then the probe; a search's
        // is its query (no closing stop in these tests) then the probe.
        let background = texts.last().is_some_and(|t| t == PROBE_TEXT)
            && texts
                .first()
                .is_some_and(|t| t != PROBE_TEXT && t.ends_with(['.', '!', '?']));
        if background {
            if self.hold_worker.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            self.worker_batches.fetch_add(1, Ordering::SeqCst);
        }
        let seed = self.seed.load(Ordering::SeqCst);
        Ok(texts.iter().map(|t| words_vector(seed, t)).collect())
    }

    /// The reranker scores a sentence by its length, so its order is
    /// visibly not cosine's.
    async fn rerank_batch(
        &self,
        _q: &str,
        docs: &[String],
    ) -> sovereign_core::error::Result<Vec<f32>> {
        if !self.rerank_slot {
            return Err(sovereign_core::error::Error::NotImplemented(
                "no reranker".into(),
            ));
        }
        Ok(docs.iter().map(|d| d.len() as f32).collect())
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.rerank_slot
            .then(|| ResidentSlot {
                role: "rerank".into(),
                model_id: RERANKER.into(),
                resident: true,
                size_bytes: None,
                transitioning: false,
                placement: None,
            })
            .into_iter()
            .collect()
    }

    fn embed_model_id(&self) -> String {
        EMBED_MODEL.into()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
}

async fn host(
    dir: &std::path::Path,
    embedder: Arc<WordHash>,
    install_lib: bool,
) -> (AppState, Library) {
    let lib = library();
    let indexes = dir.join("indexes");
    if install_lib {
        std::fs::create_dir_all(&indexes).unwrap();
        install(&indexes, &lib.corpus_id, &lib.documents, true).await;
    }
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; DIM]) }));
    let engine = crate::common::reading_double(indexes, embed).on_texts_digest(digest_of);
    let state = AppState::new_with_platform_and_engine_and_serving(
        NodeId::from_u128(1),
        Some(Arc::new(engine)),
        sovereign_daemon::state::serving::ServingSeed {
            local_inference: Some(ProviderService::new(embedder)),
        },
    );
    (state, lib)
}

fn configure(state: &AppState, f: impl FnOnce(&mut SentenceSearchConfig)) {
    let mut c = SentenceSearchConfig::from_data_file().unwrap();
    f(&mut c);
    state.inner.node.sentences.set_config(c);
}

async fn search(state: &AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/v1/knowledge/search")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55006".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn ask(lib: &Library, q: &str, documents: &[String], limit: u32) -> serde_json::Value {
    serde_json::json!({
        "query": q, "corpora": [lib.corpus_id], "documents": documents,
        "limit": limit, "granularity": "sentence",
    })
}

fn reply(status: StatusCode, body: &serde_json::Value) -> KnowledgeSearchResponse {
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body.clone()).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// Search until the index route answers: the background worker has embedded
/// every sentence of the scope.
async fn indexed(state: &AppState, body: serde_json::Value) -> KnowledgeSearchResponse {
    for _ in 0..400 {
        let (status, b) = search(state, body.clone()).await;
        let r = reply(status, &b);
        if r.route == Some(SentenceRoute::Index) {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the index route never answered")
}

fn text_of(lib: &Library, name: &str) -> String {
    Sha256Hash::of_str(&lib.documents.iter().find(|d| d.name == name).unwrap().text).to_hex()
}

/// Every hit's range read back from its stored text says its `content`.
fn assert_dereference(lib: &Library, hits: &[KnowledgeResult]) {
    for h in hits {
        let span = h
            .span
            .as_ref()
            .unwrap_or_else(|| panic!("a sentence with no span: {h:?}"));
        let doc = lib
            .documents
            .iter()
            .find(|d| Sha256Hash::of_str(&d.text).to_hex() == span.document.text_sha256)
            .expect("the hit's text is a fixture text");
        let there: String = doc
            .text
            .chars()
            .skip(span.start as usize)
            .take((span.end - span.start) as usize)
            .collect();
        assert_eq!(
            there.as_bytes(),
            h.content.as_bytes(),
            "range [{}, {})",
            span.start,
            span.end
        );
        assert_eq!(span.exact, h.content);
        assert_eq!(
            h.document.as_ref().map(|d| &d.text_sha256),
            Some(&span.document.text_sha256)
        );
    }
}

/// Platform row PV2: once indexed, a sentence query answers from the index:
/// every hit a sentence whose range reads back byte-equal, best first,
/// scored by the data file's scorer and naming its model, and a sentence
/// searched by its own words is first. Failing inputs: a range one code
/// point off; a `documents` scope that leaks another text's sentences.
#[tokio::test]
async fn an_indexed_scope_answers_sentences_that_read_back_and_stay_in_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, lib) = host(tmp.path(), WordHash::seeded(1), true).await;
    let r = indexed(&state, ask(&lib, "river archive ferry", &[], 20)).await;
    assert_eq!(
        r.scorer.as_ref().map(|s| (s.name, s.model.as_str())),
        Some((SentenceScorer::Cosine, EMBED_MODEL))
    );
    assert!(
        r.sentences_pending.is_none() && r.truncated.is_none(),
        "{r:?}"
    );
    assert!(r.results.len() >= 10, "{}", r.results.len());
    assert!(r.results.windows(2).all(|w| w[0].score >= w[1].score));
    assert_dereference(&lib, &r.results);

    for (doc, sentence) in &lib.signatures {
        let scope = text_of(&lib, doc);
        let (status, b) =
            search(&state, ask(&lib, sentence, std::slice::from_ref(&scope), 5)).await;
        let r = reply(status, &b);
        assert_eq!(r.route, Some(SentenceRoute::Index));
        assert!(r.results.len() <= 5);
        assert!(
            r.results.iter().all(|h| h
                .span
                .as_ref()
                .is_some_and(|s| s.document.text_sha256 == scope)),
            "a sentence from outside the `documents` scope: {b}"
        );
        assert!(r.results[0].content.contains(sentence.as_str()), "{b}");
        assert_dereference(&lib, &r.results);
    }
}

/// The sentence route's trace names its own stages under one span, and never
/// locates a passage (`span_of`) or queries the passage index.
#[tokio::test]
async fn the_index_route_reads_stored_addresses_and_never_locates_a_passage() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, lib) = host(tmp.path(), WordHash::seeded(1), true).await;
    indexed(&state, ask(&lib, "archive", &[], 3)).await;
    let trace = crate::oicp_search_e2e::captured_debug(async {
        let (status, body) = search(&state, ask(&lib, "archive", &[], 3)).await;
        assert_eq!(reply(status, &body).route, Some(SentenceRoute::Index));
    })
    .await;
    let events = |stage: &str| {
        trace
            .lines()
            .filter(|l| l.contains("search stage") && l.contains(&format!("stage=\"{stage}\"")))
            .count()
    };
    for stage in SearchStage::SENTENCE_ROUTE.map(SearchStage::as_str) {
        assert_eq!(events(stage), 1, "one `{stage}` event:\n{trace}");
    }
    assert_eq!(events("span_of") + events("index_query"), 0, "{trace}");
    assert!(trace.contains("search{route=\"sentences\""), "{trace}");
}

/// A scope whose sentences are still embedding is answered by the passages
/// route and says so, with the pending count; its hits are still sentences
/// that read back. Failing input: a half-built index answered as `index`.
#[tokio::test]
async fn a_scope_still_embedding_is_answered_by_the_passages_route_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let embedder = WordHash::seeded(1);
    embedder.hold_worker.store(true, Ordering::SeqCst);
    let (state, lib) = host(tmp.path(), Arc::clone(&embedder), true).await;
    let (status, body) = search(&state, ask(&lib, "river archive ferry harbour", &[], 10)).await;
    let r = reply(status, &body);
    assert_eq!(r.route, Some(SentenceRoute::Passages), "{body}");
    assert!(r.sentences_pending.unwrap_or(0) > 0, "{body}");
    assert!(!r.results.is_empty(), "{body}");
    assert_eq!(r.scorer.map(|s| s.name), Some(SentenceScorer::Cosine));
    assert_dereference(&lib, &r.results);
}

/// A vector is served only under the embedder that made it: after the
/// embedder changes, every sentence is pending again (the passages route)
/// until the index is rebuilt under the new one. Failing input: the stored
/// vectors served to the new model's query.
#[tokio::test]
async fn another_embedder_s_vectors_are_never_served() {
    let tmp = tempfile::tempdir().unwrap();
    let embedder = WordHash::seeded(1);
    let (state, lib) = host(tmp.path(), Arc::clone(&embedder), true).await;
    let first = indexed(&state, ask(&lib, "archive", &[], 50)).await;
    let all = first.results.len() as u64;

    embedder.seed.store(2, Ordering::SeqCst);
    let (status, body) = search(&state, ask(&lib, "archive", &[], 50)).await;
    let r = reply(status, &body);
    assert_eq!(
        r.route,
        Some(SentenceRoute::Passages),
        "served the old model's vectors: {body}"
    );
    assert!(r.sentences_pending.unwrap_or(0) >= all, "{body}");

    let again = indexed(&state, ask(&lib, "archive", &[], 50)).await;
    let q = sovereign_daemon::sentence_index::store::unit(words_vector(2, "archive"));
    for h in &again.results {
        let v = sovereign_daemon::sentence_index::store::unit(words_vector(2, &h.content));
        let cos: f32 = q.iter().zip(&v).map(|(a, b)| a * b).sum();
        assert!(
            (cos - h.score).abs() < 1e-4,
            "{} scored {} under the new model, {cos} expected",
            h.content,
            h.score
        );
    }
}

/// The index is kept beside the corpus's, so a restart reloads it: the next
/// host answers from the index at once and embeds no sentence again.
#[tokio::test]
async fn a_restart_reloads_the_index_rather_than_embedding_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, lib) = host(tmp.path(), WordHash::seeded(1), true).await;
    indexed(&state, ask(&lib, "archive", &[], 5)).await;
    let sidecar = tmp
        .path()
        .join("indexes")
        .join(&lib.corpus_id)
        .join(corpus_index::corpus::SENTENCES_FILENAME);
    for _ in 0..200 {
        if sidecar.is_file() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(sidecar.is_file(), "no sidecar at {}", sidecar.display());

    let embedder = WordHash::seeded(1);
    let (restarted, _) = host(tmp.path(), Arc::clone(&embedder), false).await;
    let (status, body) = search(&restarted, ask(&lib, "archive", &[], 5)).await;
    assert_eq!(
        reply(status, &body).route,
        Some(SentenceRoute::Index),
        "{body}"
    );
    assert_eq!(embedder.worker_batches.load(Ordering::SeqCst), 0);
}

/// The data file naming `rerank` on a host with no rerank slot is refused by
/// name, never answered with cosine's scores; with the slot, the reply is the
/// reranker's order and names its model. Failing input: a fallback to cosine.
#[tokio::test]
async fn rerank_without_a_rerank_slot_is_refused_by_name_never_answered_by_cosine() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, lib) = host(tmp.path(), WordHash::seeded(1), true).await;
    configure(&state, |c| c.scorer = SentenceScorer::Rerank);
    let (status, body) = search(&state, ask(&lib, "archive", &[], 5)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "scorer unavailable");
    assert_eq!(body["scorer"], "rerank");

    let tmp = tempfile::tempdir().unwrap();
    let embedder = Arc::new(WordHash {
        seed: AtomicU64::new(1),
        rerank_slot: true,
        ..Default::default()
    });
    let (state, lib) = host(tmp.path(), embedder, true).await;
    configure(&state, |c| c.scorer = SentenceScorer::Rerank);
    let r = indexed(&state, ask(&lib, "archive", &[], 5)).await;
    assert_eq!(
        r.scorer.as_ref().map(|s| (s.name, s.model.as_str())),
        Some((SentenceScorer::Rerank, RERANKER))
    );
    for h in &r.results {
        assert_eq!(h.score, h.content.len() as f32, "the reranker's score");
    }
    assert!(r.results.windows(2).all(|w| w[0].score >= w[1].score));
}

/// A scope past the data file's ceiling is answered from the ceiling's worth
/// and says so. Failing input: the scope answered short with nothing named.
#[tokio::test]
async fn a_scope_past_the_scan_ceiling_is_answered_and_named_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, lib) = host(tmp.path(), WordHash::seeded(1), true).await;
    configure(&state, |c| c.scan_ceiling = 3);
    let r = indexed(&state, ask(&lib, "archive", &[], 20)).await;
    let t = r
        .truncated
        .unwrap_or_else(|| panic!("a capped scan named nothing: {r:?}"));
    assert_eq!(t.scanned, 3);
    assert!(t.in_scope > 3, "{t:?}");
    assert!(r.results.len() <= 3);
}

/// A sentence search naming no corpus reaches the corpora already
/// sentence-indexed, and starts no indexing of its own: a host holding
/// thousands of corpora is not set embedding all of them by one query.
#[tokio::test]
async fn a_search_naming_no_corpus_reaches_only_the_indexed_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let embedder = WordHash::seeded(1);
    let (state, lib) = host(tmp.path(), Arc::clone(&embedder), true).await;
    let unnamed = serde_json::json!({"query": "archive", "granularity": "sentence"});
    let (status, body) = search(&state, unnamed.clone()).await;
    let r = reply(status, &body);
    assert!(
        r.corpora_searched.is_empty() && r.results.is_empty(),
        "{body}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        embedder.worker_batches.load(Ordering::SeqCst),
        0,
        "indexing started unasked"
    );

    indexed(&state, ask(&lib, "archive", &[], 5)).await;
    let (status, body) = search(&state, unnamed).await;
    let r = reply(status, &body);
    assert_eq!(r.corpora_searched, vec![lib.corpus_id.clone()], "{body}");
    assert_eq!(r.route, Some(SentenceRoute::Index));
}

/// Both corpora over the same texts, the second installed beside the first.
async fn two_corpora(
    dir: &std::path::Path,
    embedder: &Arc<WordHash>,
) -> (AppState, Library, String) {
    let (state, lib) = host(dir, Arc::clone(embedder), true).await;
    let behind = format!("{}-behind", lib.corpus_id);
    install(&dir.join("indexes"), &behind, &lib.documents, true).await;
    (state, lib, behind)
}

fn both(lib: &Library, behind: &str) -> serde_json::Value {
    serde_json::json!({
        "query": "river archive ferry harbour", "limit": 10, "granularity": "sentence",
        "corpora": [lib.corpus_id, behind],
    })
}

/// A library searched while another is embedding is queued behind it, with
/// the count the one ahead still has; the one ahead says it is embedding.
/// Failing input: the queued corpus answered as embedding now.
#[tokio::test]
async fn a_corpus_behind_another_is_answered_queued_behind_it() {
    let tmp = tempfile::tempdir().unwrap();
    let embedder = WordHash::seeded(1);
    embedder.hold_worker.store(true, Ordering::SeqCst);
    let (state, lib, behind) = two_corpora(tmp.path(), &embedder).await;
    let (status, body) = search(&state, ask(&lib, "river archive ferry harbour", &[], 10)).await;
    assert_eq!(reply(status, &body).route, Some(SentenceRoute::Passages));
    let (status, body) = search(&state, both(&lib, &behind)).await;
    let r = reply(status, &body);
    assert_eq!(r.route, Some(SentenceRoute::Passages), "{body}");
    let q = r.sentence_queue.clone().unwrap_or_default();
    assert_eq!(q.len(), 2, "{body}");
    assert_eq!(
        (q[0].corpus.as_str(), q[0].place),
        (lib.corpus_id.as_str(), IndexPlace::Embedding)
    );
    assert!(q[0].behind.is_empty(), "{body}");
    assert_eq!(
        (q[1].corpus.as_str(), q[1].place),
        (behind.as_str(), IndexPlace::Queued)
    );
    assert_eq!(q[1].behind.len(), 1, "{body}");
    assert_eq!(q[1].behind[0].corpus, lib.corpus_id);
    assert_eq!(q[1].behind[0].pending, Some(q[0].pending), "{body}");
    assert!(q[0].pending > 0 && q[1].pending > 0, "{body}");
}

/// A corpus that finished is named ahead of nobody. Failing input: the
/// finished first library still listed ahead of the one searched after it.
#[tokio::test]
async fn a_corpus_that_finished_is_not_named_ahead_of_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    let embedder = WordHash::seeded(1);
    let (state, lib, behind) = two_corpora(tmp.path(), &embedder).await;
    indexed(&state, ask(&lib, "river archive ferry harbour", &[], 10)).await;
    let sentences = &state.inner.node.sentences;
    for _ in 0..400 {
        let q = sentences.queue();
        if q.embedding.is_none() && q.waiting.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        sentences.queue(),
        Default::default(),
        "the worker finished the first library"
    );
    embedder.hold_worker.store(true, Ordering::SeqCst);
    let (status, body) = search(&state, both(&lib, &behind)).await;
    let r = reply(status, &body);
    assert_eq!(r.route, Some(SentenceRoute::Passages), "{body}");
    let q = r.sentence_queue.unwrap_or_default();
    assert_eq!(q.len(), 1, "{body}");
    assert_eq!(q[0].corpus, behind);
    assert_eq!(q[0].place, IndexPlace::Embedding, "{body}");
    assert!(q[0].behind.is_empty(), "{body}");
}
